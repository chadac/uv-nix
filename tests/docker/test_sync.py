"""Tests for uv sync with native packages on a patched Python.

Each test spawns an isolated Docker container with Python pre-installed
(warmed image). Tests: uv init → uv add <package> → python -c <check>

Auto-generated from data/package-build-libs.json + curated list.
"""

import json
from pathlib import Path

import pytest

from conftest import run_lib_test


# --- Load package-build-libs.json ---
_data_dir = Path(__file__).parent.parent.parent / "data"
_build_libs_raw: dict[str, dict] = json.loads(
    (_data_dir / "package-build-libs.json").read_text()
)
# Flatten to set of package names (the keys)
_build_libs: dict[str, dict] = _build_libs_raw

# --- Import checks ---
IMPORT_CHECKS = {
    "psycopg[binary]": "import psycopg; print(psycopg.__version__)",
    "psycopg2": "import psycopg2; print(psycopg2.__version__)",
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
    "av": "import av; print(av.__version__)",
    "aiokafka": "import aiokafka; print(aiokafka.__version__)",
    "soundfile": "import soundfile; print(soundfile.__version__)",
    "ruamel-yaml": "from ruamel.yaml import YAML; print('ok')",
    "tables": "import tables; print(tables.__version__)",
    "pysodium": "import pysodium; print('ok')",
    "plyvel": "import plyvel; print('ok')",
    "coincurve": "import coincurve; print('ok')",
    "imagecodecs": "import imagecodecs; print('ok')",
    "evdev": "import evdev; print('ok')",
    "pyproj": "import pyproj; print(pyproj.__version__)",
}

IMPORT_NAMES = {
    "pillow": "PIL", "pyyaml": "yaml", "psycopg-binary": "psycopg",
    "rpds-py": "rpds", "ruamel-yaml": "ruamel.yaml",
    "ruamel-yaml-clib": "ruamel.yaml", "pyzmq": "zmq", "pycairo": "cairo",
    "mysqlclient": "MySQLdb", "pynacl": "nacl", "m2crypto": "M2Crypto",
    "borgbackup": "borg", "openexr": "OpenEXR", "scikit-learn": "sklearn",
    "argon2-cffi-bindings": "_argon2_cffi_bindings",
}


def _import_check(name: str) -> str:
    if name in IMPORT_CHECKS:
        return IMPORT_CHECKS[name]
    import_name = IMPORT_NAMES.get(name, name.replace("-", "_"))
    return f"import {import_name}; print('ok')"


# Packages to skip (can't be tested in this environment)
SKIP_PACKAGES = {
    # System service dependencies
    "cysystemd", "dbus-python", "pyfuse3", "pygobject3",
    # Hardware/driver dependencies
    "hidapi", "pygame",
    # External service/protocol dependencies
    "mpi4py", "confluent-kafka", "pyodbc", "pymssql",
    # Requires libraries not in nixpkgs or complex setup
    "bjoern", "netcdf4", "pyfftw",
    # Alternate builds of other packages (tested via primary name)
    "pillow-avif-plugin", "pillow-heif", "psycopg2cffi", "psycopg-c", "psycopg2-binary",
    # Backend-only packages, can't be imported standalone (tested via psycopg[binary])
    "psycopg-binary",
    # ctypes/dlopen: needs libpq on LD_LIBRARY_PATH (not RPATH-patchable)
    "psycopg",
    # ctypes/dlopen: needs libsodium on LD_LIBRARY_PATH
    "pysodium",
    # Broken upstream packaging (needs pkg_resources but doesn't declare it)
    "pygeos",
    # C extension can't be imported without ruamel-yaml (tested via ruamel-yaml)
    "ruamel-yaml-clib",
}

SLOW_SOURCE_BUILDS = {
    "grpcio", "cryptography", "matplotlib", "h5py", "imagecodecs",
    "tables", "av", "borgbackup", "pyproj", "openexr",
}

SOURCE_ONLY = {
    "psycopg2", "mysqlclient", "mariadb", "kerberos", "jsonslicer",
    "m2crypto", "pycairo", "borgbackup",
}

# --- Build test param lists ---
_wheel_packages: list[str] = []
for pkg in sorted(_build_libs.keys()):
    if pkg not in SKIP_PACKAGES and pkg not in SOURCE_ONLY:
        _wheel_packages.append(pkg)
for pkg in [
    "numpy", "pandas", "scipy", "pyyaml", "cffi", "markupsafe",
    "msgpack", "ujson", "bcrypt", "orjson", "pydantic", "rpds-py",
    "regex", "psycopg[binary]",
]:
    if pkg not in _wheel_packages and pkg not in SKIP_PACKAGES:
        _wheel_packages.append(pkg)

WHEEL_TESTS = [
    pytest.param(
        pkg, _import_check(pkg),
        id=pkg,
        marks=[pytest.mark.slow] if pkg in {"pandas", "scipy"} else [],
    )
    for pkg in _wheel_packages
]

SOURCE_BUILD_TESTS = [
    pytest.param(
        pkg, _import_check(pkg),
        id=f"{pkg}-source",
        marks=(
            [pytest.mark.source_build]
            + ([pytest.mark.slow] if pkg in SLOW_SOURCE_BUILDS else [])
        ),
    )
    for pkg in sorted(_build_libs.keys())
    if pkg not in SKIP_PACKAGES and pkg not in SOURCE_ONLY
]

# Source-only packages: no wheels on PyPI, always built from source.
# Tested separately because `uv add` (without --no-binary) already triggers
# a source build for these.
SOURCE_ONLY_TESTS = [
    pytest.param(
        pkg, _import_check(pkg),
        id=f"{pkg}-source-only",
        marks=[pytest.mark.source_build]
            + ([pytest.mark.slow] if pkg in SLOW_SOURCE_BUILDS else []),
    )
    for pkg in sorted(SOURCE_ONLY)
    if pkg not in SKIP_PACKAGES
]


# --- Test classes ---

class TestWheelInstall:
    """Test binary wheel install in isolated containers (pre-warmed image)."""

    @pytest.mark.parametrize("package,check", WHEEL_TESTS)
    def test_wheel(self, warmed_image: str, package: str, check: str):
        result = run_lib_test(package, check, image=warmed_image)
        assert result.returncode == 0, (
            f"{package} failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )


class TestSourceBuild:
    """Test source build (--no-binary) in isolated containers."""

    @pytest.mark.parametrize("package,check", SOURCE_BUILD_TESTS)
    def test_source_build(self, warmed_image: str, package: str, check: str):
        result = run_lib_test(
            package, check, image=warmed_image, no_binary=True, timeout=600,
        )
        assert result.returncode == 0, (
            f"{package} source build failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )


class TestSourceOnly:
    """Test source-only packages (no wheels on PyPI)."""

    @pytest.mark.parametrize("package,check", SOURCE_ONLY_TESTS)
    def test_source_only(self, warmed_image: str, package: str, check: str):
        result = run_lib_test(
            package, check, image=warmed_image, timeout=600,
        )
        assert result.returncode == 0, (
            f"{package} source-only build failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
