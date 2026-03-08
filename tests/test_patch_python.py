"""Tests for Python interpreter patching via Nix derivation.

Each test runs in an isolated Docker container (busybox + nix mounts).
"""

import os
import subprocess
from pathlib import Path

import pytest


def _docker_env() -> dict:
    """Resolve nix/SSL/git paths needed for Docker containers."""
    nix_bin = os.popen("readlink -f $(which nix)").read().strip()
    git_bin = os.popen("readlink -f $(which git)").read().strip()
    git_core_dir = os.popen("git --exec-path").read().strip()
    ca_bundle = os.popen(
        "ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1"
    ).read().strip()
    project_root = Path(__file__).parent.parent
    uv_bin = os.environ.get("UV_BIN", str(project_root / "uv" / "target" / "debug" / "uv"))
    return {
        "uv_bin": uv_bin,
        "nix_bin_dir": str(Path(nix_bin).parent),
        "git_bin_dir": str(Path(git_bin).parent),
        "git_core_dir": git_core_dir,
        "ca_bundle": ca_bundle,
    }


_DOCKER_ENV: dict | None = None


def _get_docker_env() -> dict:
    global _DOCKER_ENV
    if _DOCKER_ENV is None:
        _DOCKER_ENV = _docker_env()
    return _DOCKER_ENV


def run_in_container(script: str, timeout: int = 300) -> subprocess.CompletedProcess:
    """Run a shell script in a busybox container with nix mounts."""
    env = _get_docker_env()
    cmd = [
        "docker", "run", "--rm",
        "--network", "host",
        "-v", f"{env['uv_bin']}:/usr/local/bin/uv:ro",
        "-v", "/nix:/nix",
        "-v", f"{env['nix_bin_dir']}:/nix-bin:ro",
        "-v", f"{env['git_bin_dir']}:/git-bin:ro",
        "-v", f"{env['git_core_dir']}:/git-core:ro",
        "-e", "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin",
        "-e", "NIX_REMOTE=daemon",
        "-e", f"NIX_SSL_CERT_FILE={env['ca_bundle']}",
        "-e", f"SSL_CERT_FILE={env['ca_bundle']}",
        "-e", f"GIT_SSL_CAINFO={env['ca_bundle']}",
        "-e", f"GIT_EXEC_PATH=/git-core",
        "busybox",
        "/bin/sh", "-c", script,
    ]
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


class TestPatchPythonInstall:
    """Test that `uv python install` produces a correctly patched Python."""

    def test_install_and_nix_ref(self):
        """After install, a .nix sibling should reference /nix/store/."""
        result = run_in_container("""
            cd /tmp
            uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            NIXREF="${PYDIR}.nix"
            # Check .nix reference exists and points to /nix/store
            test -L "$NIXREF" && readlink "$NIXREF" | grep -q '^/nix/store/' && echo "NIX_REF_OK"
            # Check python dir is a real directory (writable)
            test -d "$PYDIR" && ! test -L "$PYDIR" && echo "DIR_OK"
            # Check files inside are symlinks to nix store
            test -L "$PYDIR/bin/python3.12" && echo "SYMLINK_OK"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "NIX_REF_OK" in result.stdout, f"No .nix ref:\n{result.stdout}"
        assert "DIR_OK" in result.stdout, f"Dir is not writable:\n{result.stdout}"
        assert "SYMLINK_OK" in result.stdout, f"Files not symlinked:\n{result.stdout}"

    def test_patched_python_runs(self):
        """The patched Python interpreter should be executable."""
        result = run_in_container("""
            cd /tmp
            uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            "$PYDIR/bin/python3.12" -c "import sys; print(sys.version)"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "3.12" in result.stdout

    def test_ssl_module_works(self):
        """The patched Python should be able to import ssl."""
        result = run_in_container("""
            cd /tmp
            uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            "$PYDIR/bin/python3.12" -c "import ssl; print(ssl.OPENSSL_VERSION)"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "OpenSSL" in result.stdout

    def test_stdlib_imports(self):
        """Key stdlib modules including C extensions should import."""
        result = run_in_container("""
            cd /tmp
            uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            "$PYDIR/bin/python3.12" -c "
import os, sys, json, ssl, sqlite3, ctypes, hashlib, zlib
print('stdlib ok')
"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "stdlib ok" in result.stdout
