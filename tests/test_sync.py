"""Tests for uv sync with native packages on a patched Python.

Tests are auto-generated from data/package-build-libs.json (packages needing
extra nix libraries) plus a curated list of popular native packages.

Two test modes:
- Wheel install (default): tests RPATH patching + ctypes hook
- Source build (--no-binary): tests build env (headers, pkg-config, compiler)
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from test_patch_python import run_uv


# --- Package registry ---
# Import checks for packages. If a package isn't listed here, a basic
# `import <name>; print('ok')` is generated automatically.
IMPORT_CHECKS = {
    "psycopg": "import psycopg; print('ok')",
    "psycopg-binary": "import psycopg; print('ok')",
    "psycopg2": "import psycopg2; print(psycopg2.__version__)",
    "psycopg2-binary": "import psycopg2; print(psycopg2.__version__)",
    "pillow": "from PIL import Image; print('ok')",
    "lxml": "from lxml import etree; print(etree.LXML_VERSION)",
    "cryptography": "from cryptography.hazmat.primitives.ciphers import Cipher; print('ok')",
    "grpcio": "import grpc; print(grpc.__version__)",
    "numpy": "import numpy; print(numpy.__version__)",
    "pandas": "import pandas; print(pandas.__version__)",
    "scipy": "import scipy; print(scipy.__version__)",
    "pyyaml": "import yaml; yaml.CSafeLoader; print('ok')",
    "cffi": "import _cffi_backend; print('ok')",
    "markupsafe": "import markupsafe; print(markupsafe.__version__)",
    "msgpack": "import msgpack; print(msgpack.packb({'a': 1}))",
    "ujson": "import ujson; print(ujson.dumps({'a': 1}))",
    "bcrypt": "import bcrypt; print('ok')",
    "orjson": "import orjson; print(orjson.dumps({'a': 1}))",
    "pydantic": "from pydantic import BaseModel; print('ok')",
    "rpds-py": "import rpds; print(rpds.HashTrieMap({'a': 1}))",
    "regex": "import regex; print(regex.search(r'\\w+', 'hello').group())",
    "h5py": "import h5py; print(h5py.__version__)",
    "matplotlib": "import matplotlib; print(matplotlib.__version__)",
    "pyzmq": "import zmq; print(zmq.__version__)",
    "pycurl": "import pycurl; print(pycurl.version)",
    "shapely": "import shapely; print(shapely.__version__)",
    "pycairo": "import cairo; print(cairo.version)",
    "reportlab": "import reportlab; print(reportlab.__version__)",
    "mysqlclient": "import MySQLdb; print('ok')",
    "mariadb": "import mariadb; print('ok')",
    "pyodbc": "import pyodbc; print('ok')",
    "av": "import av; print(av.__version__)",
    "aiokafka": "import aiokafka; print(aiokafka.__version__)",
    "soundfile": "import soundfile; print(soundfile.__version__)",
    "ruamel-yaml": "from ruamel.yaml import YAML; print('ok')",
    "ruamel-yaml-clib": "from ruamel.yaml import YAML; print('ok')",
    "tables": "import tables; print(tables.__version__)",
    "pysodium": "import pysodium; print('ok')",
    "plyvel": "import plyvel; print('ok')",
    "coincurve": "import coincurve; print('ok')",
    "pygobject3": "import gi; print('ok')",
    "imagecodecs": "import imagecodecs; print('ok')",
    "m2crypto": "import M2Crypto; print('ok')",
    "borgbackup": "import borg; print('ok')",
    "evdev": "import evdev; print('ok')",
    "pymssql": "import pymssql; print('ok')",
    "pyproj": "import pyproj; print(pyproj.__version__)",
    "pyfuse3": "import pyfuse3; print('ok')",
    "openexr": "import OpenEXR; print('ok')",
}

# pip name -> import name mapping for auto-generated checks
IMPORT_NAMES = {
    "pillow": "PIL",
    "pyyaml": "yaml",
    "psycopg-binary": "psycopg",
    "psycopg2-binary": "psycopg2",
    "rpds-py": "rpds",
    "ruamel-yaml": "ruamel.yaml",
    "ruamel-yaml-clib": "ruamel.yaml",
    "pygobject3": "gi",
    "mysqlclient": "MySQLdb",
    "pyzmq": "zmq",
    "pycairo": "cairo",
    "psycopg-c": "psycopg",
    "m2crypto": "M2Crypto",
    "borgbackup": "borg",
    "openexr": "OpenEXR",
}


def _import_check(name: str) -> str:
    """Get or generate an import check for a package."""
    if name in IMPORT_CHECKS:
        return IMPORT_CHECKS[name]
    import_name = IMPORT_NAMES.get(name, name.replace("-", "_"))
    return f"import {import_name}; print('ok')"


# --- Load package-build-libs.json ---
_data_dir = Path(__file__).parent.parent / "data"
_build_libs: dict[str, list[str]] = json.loads(
    (_data_dir / "package-build-libs.json").read_text()
)


# Packages that are known to not have binary wheels (always source build).
# These don't need a separate --no-binary test since they always build from source.
SOURCE_ONLY = {
    "psycopg2",
    "mysqlclient",
    "mariadb",
}

# Packages to skip entirely (broken, not on PyPI, or need special setup).
SKIP_PACKAGES = {
    "cysystemd",       # needs systemd running
    "dbus-python",     # needs dbus running
    "mpi4py",          # needs MPI runtime
    "pyfuse3",         # needs FUSE kernel module
    "pygame",          # needs display server
    "pygobject3",      # needs gobject-introspection runtime
    "hidapi",          # needs USB access
    "bjoern",          # obscure, C build complexity
    "confluent-kafka", # needs running Kafka
    "netcdf4",         # complex dep chain
    "pyfftw",          # needs FFTW variants
    "pyodbc",          # needs ODBC driver manager + drivers
    "pymssql",         # needs freetds + MSSQL connection
    "open3d",          # huge, needs GPU
    "gmsh",            # needs OpenGL
    "pillow-avif-plugin", # needs pillow co-installed
    "pillow-heif",     # needs pillow co-installed
    "psycopg2cffi",    # duplicate of psycopg2
    "psycopg-c",       # needs psycopg co-installed
    "psycopg2-binary", # duplicate (binary wheel variant)
}

# Packages that take a long time to build from source.
SLOW_SOURCE_BUILDS = {
    "grpcio",
    "cryptography",
    "matplotlib",
    "h5py",
    "imagecodecs",
    "tables",
    "av",
    "borgbackup",
    "pyproj",
    "openexr",
}


# --- Generate wheel test params ---
# These test the default (binary wheel) install path.
# Includes packages from package-build-libs.json + popular native packages.
_WHEEL_PACKAGES: list[str] = []

# All packages from the build-libs registry (that we don't skip)
for pkg in sorted(_build_libs.keys()):
    if pkg not in SKIP_PACKAGES:
        _WHEEL_PACKAGES.append(pkg)

# Additional popular native packages not in the registry
# (they don't need extra build deps beyond defaults)
for pkg in [
    "numpy", "pandas", "scipy", "pyyaml", "cffi", "markupsafe",
    "msgpack", "ujson", "bcrypt", "orjson", "pydantic", "rpds-py",
    "regex", "psycopg", "psycopg-binary",
]:
    if pkg not in _WHEEL_PACKAGES and pkg not in SKIP_PACKAGES:
        _WHEEL_PACKAGES.append(pkg)

WHEEL_TESTS = [
    pytest.param(
        pkg, [pkg], _import_check(pkg),
        id=pkg,
        marks=[pytest.mark.slow] if pkg in {"pandas", "scipy"} else [],
    )
    for pkg in _WHEEL_PACKAGES
]

# --- Generate source build test params ---
# Only packages from the build-libs registry (they need the extra deps).
# Skip packages that are always source-only (already tested by wheel tests).
SOURCE_BUILD_TESTS = [
    pytest.param(
        pkg, [pkg], _import_check(pkg),
        id=f"{pkg}-source",
        marks=(
            [pytest.mark.source_build]
            + ([pytest.mark.slow] if pkg in SLOW_SOURCE_BUILDS else [])
        ),
    )
    for pkg in sorted(_build_libs.keys())
    if pkg not in SKIP_PACKAGES and pkg not in SOURCE_ONLY
]


# --- Helpers ---

def _sync_package(
    uv_binary: Path,
    python_bin: Path,
    env: dict[str, str],
    project_dir: Path,
    name: str,
    dependencies: list[str],
    no_binary: bool = False,
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

    args = ["sync", "--python", str(python_bin)]
    if no_binary:
        args.extend(["--no-binary", name])

    result = run_uv(
        uv_binary,
        args,
        env_overrides=env,
        cwd=project_dir,
    )
    assert result.returncode == 0, f"uv sync failed:\n{result.stderr}"

    venv_python = project_dir / ".venv" / "bin" / "python"
    assert venv_python.exists(), f"venv python not found at {venv_python}"
    return venv_python


def _run_import_check(venv_python: Path, import_check: str, name: str):
    """Run an import check in the venv, with a clean env."""
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
    assert proc.returncode == 0, f"{name} import failed:\n{proc.stderr}"


# --- Test classes ---

class TestWheelInstall:
    """Test binary wheel install + RPATH patching + import."""

    @pytest.mark.parametrize("name,dependencies,import_check", WHEEL_TESTS)
    def test_wheel(
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
        _run_import_check(venv_python, import_check, name)


class TestSourceBuild:
    """Test source build (--no-binary) + build env + RPATH patching + import.

    These tests verify that package-build-libs.json provides the right
    headers and libraries for compilation.
    """

    @pytest.mark.parametrize("name,dependencies,import_check", SOURCE_BUILD_TESTS)
    def test_source_build(
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
            uv_binary, python_bin, env, project_dir, name, dependencies,
            no_binary=True,
        )
        _run_import_check(venv_python, import_check, name)


@pytest.mark.docker
class TestWheelInstallDocker:
    """Test wheel install in a FROM scratch container (no host libs)."""

    @pytest.mark.parametrize("name,dependencies,import_check", WHEEL_TESTS)
    def test_wheel_in_container(
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
            pytest.skip("nix not available on PATH")

        python_bin, env = installed_python
        project_dir = tmp_path / "test-project"
        _sync_package(uv_binary, python_bin, env, project_dir, name, dependencies)
        venv_dir = project_dir / ".venv"

        # Resolve the Python store path via .nix reference symlink
        cpython_dir = python_bin.parent.parent
        nix_ref = cpython_dir.parent / f"{cpython_dir.name}.nix"
        if nix_ref.is_symlink():
            python_store_path = str(nix_ref.resolve())
        else:
            python_store_path = str(cpython_dir)

        python_in_container = f"/nix/store/{Path(python_store_path).name}/bin/python3.12"

        site_packages = list(venv_dir.glob("lib/python*/site-packages"))
        assert site_packages, f"No site-packages found in {venv_dir}"
        py_version_dir = site_packages[0].parent.name

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
        assert output_str.strip(), (
            f"{name} produced no output in container (import likely failed)"
        )
