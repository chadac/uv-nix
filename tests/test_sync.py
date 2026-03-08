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
_data_dir = Path(__file__).parent.parent / "data"
_build_libs: dict[str, list[str]] = json.loads(
    (_data_dir / "package-build-libs.json").read_text()
)

# --- Import checks ---
IMPORT_CHECKS = {
    "psycopg": "import psycopg; print('ok')",
    "psycopg-binary": "import psycopg; print('ok')",
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
    "ruamel-yaml-clib": "from ruamel.yaml import YAML; print('ok')",
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
}


def _import_check(name: str) -> str:
    if name in IMPORT_CHECKS:
        return IMPORT_CHECKS[name]
    import_name = IMPORT_NAMES.get(name, name.replace("-", "_"))
    return f"import {import_name}; print('ok')"


# Packages to skip
SKIP_PACKAGES = {
    "cysystemd", "dbus-python", "mpi4py", "pyfuse3", "pygame",
    "pygobject3", "hidapi", "bjoern", "confluent-kafka", "netcdf4",
    "pyfftw", "pyodbc", "pymssql", "pillow-avif-plugin", "pillow-heif",
    "psycopg2cffi", "psycopg-c", "psycopg2-binary",
}

SLOW_SOURCE_BUILDS = {
    "grpcio", "cryptography", "matplotlib", "h5py", "imagecodecs",
    "tables", "av", "borgbackup", "pyproj", "openexr",
}

SOURCE_ONLY = {"psycopg2", "mysqlclient", "mariadb"}


# --- Build test param lists ---
_wheel_packages: list[str] = []
for pkg in sorted(_build_libs.keys()):
    if pkg not in SKIP_PACKAGES:
        _wheel_packages.append(pkg)
for pkg in [
    "numpy", "pandas", "scipy", "pyyaml", "cffi", "markupsafe",
    "msgpack", "ujson", "bcrypt", "orjson", "pydantic", "rpds-py",
    "regex", "psycopg", "psycopg-binary",
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
