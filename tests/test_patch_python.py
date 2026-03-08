"""Tests for Python interpreter patching via Nix derivation."""

import os
import shutil
import subprocess
from pathlib import Path

import pytest


def run_uv(
    uv_binary: Path,
    args: list[str],
    env_overrides: dict[str, str] | None = None,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess:
    """Run uv with given args and environment."""
    env = os.environ.copy()
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        [str(uv_binary), *args],
        env=env,
        capture_output=True,
        text=True,
        cwd=cwd,
        timeout=300,
    )


def find_cpython_dir(tmp_python_dir: Path) -> Path:
    """Find the versioned cpython directory (not the unversioned symlink)."""
    # e.g. cpython-3.12.13-linux-x86_64-gnu (versioned, not the cpython-3.12-... symlink)
    candidates = sorted(tmp_python_dir.glob("cpython-3.12.*"))
    assert candidates, f"No cpython-3.12.* directory found in {tmp_python_dir}"
    return candidates[0]


class TestPatchPythonInstall:
    """Test that `uv python install` produces a correctly patched Python."""

    def test_install_creates_symlink(
        self, uv_binary: Path, nix_available: bool, tmp_python_dir: Path
    ):
        """After install, the cpython directory should be a symlink to /nix/store/."""
        if not nix_available:
            pytest.skip("nix-build not available on PATH")

        env = {"UV_PYTHON_INSTALL_DIR": str(tmp_python_dir)}

        result = run_uv(uv_binary, ["python", "install", "3.12"], env_overrides=env)
        assert result.returncode == 0, f"uv python install failed:\n{result.stderr}"

        cpython_dir = find_cpython_dir(tmp_python_dir)
        assert cpython_dir.is_symlink(), (
            f"cpython dir should be a symlink, got: {cpython_dir}"
        )

        target = cpython_dir.resolve()
        assert str(target).startswith("/nix/store/"), (
            f"Symlink should point to /nix/store/, got: {target}"
        )

    def test_patched_python_runs(
        self, uv_binary: Path, nix_available: bool, tmp_python_dir: Path
    ):
        """The patched Python interpreter should be executable."""
        if not nix_available:
            pytest.skip("nix-build not available on PATH")

        env = {"UV_PYTHON_INSTALL_DIR": str(tmp_python_dir)}

        result = run_uv(uv_binary, ["python", "install", "3.12"], env_overrides=env)
        assert result.returncode == 0, f"uv python install failed:\n{result.stderr}"

        cpython_dir = find_cpython_dir(tmp_python_dir)
        python_bin = cpython_dir / "bin" / "python3.12"
        assert python_bin.exists(), f"Python binary not found at {python_bin}"

        proc = subprocess.run(
            [str(python_bin), "-c", "import sys; print(sys.version)"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert proc.returncode == 0, f"Python failed to run:\n{proc.stderr}"
        assert "3.12" in proc.stdout

    def test_readelf_shows_correct_interpreter(
        self, uv_binary: Path, nix_available: bool, tmp_python_dir: Path
    ):
        """readelf should show the Nix dynamic linker."""
        if not nix_available:
            pytest.skip("nix-build not available on PATH")
        if not shutil.which("readelf"):
            pytest.skip("readelf not available on PATH")

        env = {"UV_PYTHON_INSTALL_DIR": str(tmp_python_dir)}

        result = run_uv(uv_binary, ["python", "install", "3.12"], env_overrides=env)
        assert result.returncode == 0

        cpython_dir = find_cpython_dir(tmp_python_dir)
        python_bin = cpython_dir / "bin" / "python3.12"

        proc = subprocess.run(
            ["readelf", "-l", str(python_bin)],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert proc.returncode == 0
        assert "/nix/store/" in proc.stdout, (
            f"Interpreter should point to /nix/store/:\n{proc.stdout}"
        )

    def test_ssl_module_works(
        self, uv_binary: Path, nix_available: bool, tmp_python_dir: Path
    ):
        """The patched Python should be able to import ssl (linked against openssl)."""
        if not nix_available:
            pytest.skip("nix-build not available on PATH")

        env = {"UV_PYTHON_INSTALL_DIR": str(tmp_python_dir)}

        result = run_uv(uv_binary, ["python", "install", "3.12"], env_overrides=env)
        assert result.returncode == 0

        cpython_dir = find_cpython_dir(tmp_python_dir)
        python_bin = cpython_dir / "bin" / "python3.12"

        proc = subprocess.run(
            [str(python_bin), "-c", "import ssl; print(ssl.OPENSSL_VERSION)"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert proc.returncode == 0, f"ssl import failed:\n{proc.stderr}"
        assert "OpenSSL" in proc.stdout


@pytest.mark.docker
class TestPatchPythonDocker:
    """Test patched Python in a minimal Docker container (no host libs)."""

    def test_python_runs_in_container(
        self,
        uv_binary: Path,
        nix_available: bool,
        tmp_python_dir: Path,
        docker_client,
        scratch_image,
    ):
        """Patched Python should run in a FROM scratch container with only Nix store paths."""
        if not nix_available:
            pytest.skip("nix-build not available on PATH")

        env = {"UV_PYTHON_INSTALL_DIR": str(tmp_python_dir)}

        result = run_uv(uv_binary, ["python", "install", "3.12"], env_overrides=env)
        assert result.returncode == 0

        cpython_dir = find_cpython_dir(tmp_python_dir)
        assert cpython_dir.is_symlink()

        store_path = cpython_dir.resolve()
        python_bin = f"/nix/store/{store_path.name}/bin/python3.12"

        output = docker_client.containers.run(
            scratch_image.id,
            command=[python_bin, "-c", "import sys; print(sys.version)"],
            volumes={"/nix/store": {"bind": "/nix/store", "mode": "ro"}},
            remove=True,
            stdout=True,
            stderr=True,
        )
        assert "3.12" in output.decode(), f"Expected Python 3.12 output, got: {output.decode()}"

    def test_stdlib_imports_in_container(
        self,
        uv_binary: Path,
        nix_available: bool,
        tmp_python_dir: Path,
        docker_client,
        scratch_image,
    ):
        """Key stdlib modules (including C extensions) should import in the container."""
        if not nix_available:
            pytest.skip("nix-build not available on PATH")

        env = {"UV_PYTHON_INSTALL_DIR": str(tmp_python_dir)}

        result = run_uv(uv_binary, ["python", "install", "3.12"], env_overrides=env)
        assert result.returncode == 0

        cpython_dir = find_cpython_dir(tmp_python_dir)
        assert cpython_dir.is_symlink()
        store_path = cpython_dir.resolve()
        python_bin = f"/nix/store/{store_path.name}/bin/python3.12"

        # Test stdlib modules including C extensions (ssl, sqlite3, ctypes, etc.)
        check = "; ".join([
            "import os",
            "import sys",
            "import json",
            "import ssl",
            "import sqlite3",
            "import ctypes",
            "import hashlib",
            "import zlib",
            "print('stdlib ok')",
        ])

        output = docker_client.containers.run(
            scratch_image.id,
            command=[python_bin, "-c", check],
            volumes={"/nix/store": {"bind": "/nix/store", "mode": "ro"}},
            remove=True,
            stdout=True,
            stderr=True,
        )
        assert "stdlib ok" in output.decode(), f"Stdlib imports failed: {output.decode()}"
