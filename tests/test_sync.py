"""Tests for uv sync with native packages on a patched Python.

The host tests verify that uv sync + patching runs without errors.
The Docker tests verify that patched .so files actually work in an isolated
container with NO host libraries — only /nix/store paths are available.
"""

import os
import subprocess
from pathlib import Path

import pytest

from test_patch_python import find_cpython_dir, run_uv


# Each entry: (test_id, dependencies_list, import_check_code)
# Grouped by what they exercise:
#
# Bundled .libs/ directories (auditwheel-repaired wheels with vendored C libs):
#   psycopg-binary, pillow, lxml, cryptography, grpcio
#
# C extensions linked against system libs:
#   numpy, pandas, scipy, pyyaml, cffi, markupsafe, msgpack, ujson, bcrypt
#
# Rust extensions (PyO3/maturin):
#   orjson, pydantic-core, rpds-py, regex
NATIVE_PACKAGES = [
    # --- Bundled .libs/ (complex RPATH chains) ---
    pytest.param(
        "psycopg",
        ["psycopg"],
        "import psycopg; print('ok')",
        id="psycopg",
    ),
    pytest.param(
        "psycopg-binary",
        ["psycopg[binary]"],
        "import psycopg; print('ok')",
        id="psycopg-binary",
    ),
    pytest.param(
        "psycopg2",
        ["psycopg2"],
        "import psycopg2; print(psycopg2.__version__)",
        id="psycopg2",
    ),
    pytest.param(
        "pillow",
        ["pillow"],
        "from PIL import Image; print(Image.core.jpeglib_version)",
        id="pillow",
    ),
    pytest.param(
        "lxml",
        ["lxml"],
        "from lxml import etree; print(etree.LXML_VERSION)",
        id="lxml",
    ),
    pytest.param(
        "cryptography",
        ["cryptography"],
        "from cryptography.hazmat.bindings.openssl import binding; print(binding.Binding.lib.OPENSSL_VERSION_TEXT)",
        id="cryptography",
    ),
    pytest.param(
        "grpcio",
        ["grpcio"],
        "import grpc; print(grpc.__version__)",
        id="grpcio",
    ),
    # --- C extensions (simpler, but still need interpreter/RPATH) ---
    pytest.param(
        "numpy",
        ["numpy"],
        "import numpy; print(numpy.__version__)",
        id="numpy",
    ),
    pytest.param(
        "pandas",
        ["pandas"],
        "import pandas; print(pandas.__version__)",
        id="pandas",
        marks=pytest.mark.slow,
    ),
    pytest.param(
        "scipy",
        ["scipy"],
        "import scipy; print(scipy.__version__)",
        id="scipy",
        marks=pytest.mark.slow,
    ),
    pytest.param(
        "pyyaml",
        ["pyyaml"],
        "import yaml; yaml.CSafeLoader; print('ok')",
        id="pyyaml",
    ),
    pytest.param(
        "cffi",
        ["cffi"],
        "import _cffi_backend; print('ok')",
        id="cffi",
    ),
    pytest.param(
        "markupsafe",
        ["markupsafe"],
        "import markupsafe; print(markupsafe.__version__)",
        id="markupsafe",
    ),
    pytest.param(
        "msgpack",
        ["msgpack"],
        "import msgpack; print(msgpack.packb({'a': 1}))",
        id="msgpack",
    ),
    pytest.param(
        "ujson",
        ["ujson"],
        "import ujson; print(ujson.dumps({'a': 1}))",
        id="ujson",
    ),
    pytest.param(
        "bcrypt",
        ["bcrypt"],
        "import bcrypt; print(bcrypt.hashpw(b'test', bcrypt.gensalt()))",
        id="bcrypt",
    ),
    # --- Rust extensions (PyO3/maturin) ---
    pytest.param(
        "orjson",
        ["orjson"],
        "import orjson; print(orjson.dumps({'a': 1}))",
        id="orjson",
    ),
    pytest.param(
        "pydantic",
        ["pydantic"],
        "from pydantic import BaseModel; print('ok')",
        id="pydantic",
    ),
    pytest.param(
        "rpds-py",
        ["rpds-py"],
        "import rpds; print(rpds.HashTrieMap({'a': 1}))",
        id="rpds-py",
    ),
    pytest.param(
        "regex",
        ["regex"],
        "import regex; print(regex.search(r'\\w+', 'hello').group())",
        id="regex",
    ),
]


@pytest.fixture(scope="module")
def installed_python(
    uv_binary: Path,
    nix_available: bool,
    tmp_path_factory: pytest.TempPathFactory,
) -> tuple[Path, dict[str, str]]:
    """Install Python 3.12 once, shared across all sync tests."""
    if not nix_available:
        pytest.skip("nix-build not available on PATH")

    tmp_python_dir = tmp_path_factory.mktemp("python")
    env = {"UV_PYTHON_INSTALL_DIR": str(tmp_python_dir)}

    result = run_uv(uv_binary, ["python", "install", "3.12"], env_overrides=env)
    assert result.returncode == 0, f"uv python install failed:\n{result.stderr}"

    cpython_dir = find_cpython_dir(tmp_python_dir)
    python_bin = cpython_dir / "bin" / "python3.12"
    assert python_bin.exists()
    return python_bin, env


def _sync_package(
    uv_binary: Path,
    python_bin: Path,
    env: dict[str, str],
    project_dir: Path,
    name: str,
    dependencies: list[str],
) -> Path:
    """Create a project, run uv sync, return the venv python path."""
    project_dir.mkdir(exist_ok=True)
    deps_str = "\n".join(f'    "{dep}",' for dep in dependencies)
    (project_dir / "pyproject.toml").write_text(f"""\
[project]
name = "test-{name}"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = [
{deps_str}
]
""")

    result = run_uv(
        uv_binary,
        ["sync", "--python", str(python_bin)],
        env_overrides=env,
        cwd=project_dir,
    )
    assert result.returncode == 0, f"uv sync failed:\n{result.stderr}"

    venv_python = project_dir / ".venv" / "bin" / "python"
    assert venv_python.exists(), f"venv python not found at {venv_python}"
    return venv_python


class TestSyncNativePackage:
    """Test that uv sync + import works on the host (smoke test)."""

    @pytest.mark.parametrize("name,dependencies,import_check", NATIVE_PACKAGES)
    def test_native_package(
        self,
        uv_binary: Path,
        installed_python: tuple[Path, dict[str, str]],
        tmp_path: Path,
        name: str,
        dependencies: list[str],
        import_check: str,
    ):
        python_bin, env = installed_python
        project_dir = tmp_path / "test-project"
        venv_python = _sync_package(
            uv_binary, python_bin, env, project_dir, name, dependencies
        )

        # Use a clean environment to prevent nix-shell's packages from
        # leaking into the venv via PYTHONPATH
        import_env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("PYTHON")
        }

        proc = subprocess.run(
            [str(venv_python), "-c", import_check],
            capture_output=True,
            text=True,
            timeout=60,
            env=import_env,
        )
        assert proc.returncode == 0, (
            f"{name} import failed:\n{proc.stderr}"
        )


@pytest.mark.docker
class TestSyncNativePackageDocker:
    """Test native packages in a FROM scratch container.

    This is the real test: no host libraries, only /nix/store paths + the
    venv. If RPATHs are wrong, imports will fail here even if they pass on
    the host.
    """

    @pytest.mark.parametrize("name,dependencies,import_check", NATIVE_PACKAGES)
    def test_native_package_in_container(
        self,
        uv_binary: Path,
        installed_python: tuple[Path, dict[str, str]],
        nix_available: bool,
        docker_client,
        scratch_image,
        tmp_path: Path,
        name: str,
        dependencies: list[str],
        import_check: str,
    ):
        if not nix_available:
            pytest.skip("nix-build not available on PATH")

        python_bin, env = installed_python

        # Sync the package
        project_dir = tmp_path / "test-project"
        _sync_package(uv_binary, python_bin, env, project_dir, name, dependencies)
        venv_dir = project_dir / ".venv"

        # Resolve the Python store path via .nix reference symlink
        cpython_dir = python_bin.parent.parent
        nix_ref = cpython_dir.with_suffix(".nix")
        if nix_ref.is_symlink():
            python_store_path = str(nix_ref.resolve())
        else:
            python_store_path = str(cpython_dir)

        python_in_container = f"/nix/store/{Path(python_store_path).name}/bin/python3.12"

        # Find site-packages path in the venv
        site_packages = list(venv_dir.glob("lib/python*/site-packages"))
        assert site_packages, f"No site-packages found in {venv_dir}"
        py_version_dir = site_packages[0].parent.name  # e.g. "python3.12"

        # Write a check script that adds venv site-packages to sys.path
        check_script = tmp_path / "check.py"
        check_script.write_text(f"""\
import sys
sys.path.insert(0, "/venv/lib/{py_version_dir}/site-packages")
{import_check}
""")

        output = docker_client.containers.run(
            scratch_image.id,
            command=[python_in_container, "/check.py"],
            volumes={
                "/nix/store": {"bind": "/nix/store", "mode": "ro"},
                str(venv_dir): {"bind": "/venv", "mode": "ro"},
                str(check_script): {"bind": "/check.py", "mode": "ro"},
            },
            remove=True,
            stdout=True,
            stderr=True,
        )
        output_str = output.decode()
        # The import check should produce output and not crash
        assert output_str.strip(), (
            f"{name} produced no output in container (import likely failed)"
        )
