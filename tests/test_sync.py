"""Tests for uv sync with native packages on a patched Python.

Each test spawns an isolated Docker container (busybox + nix mounts)
that runs: uv init → uv add <package> → python -c <import check>

Tests are auto-generated from data/package-build-libs.json plus a
curated list of popular native packages.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest


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
    "imagecodecs": "import imagecodecs; print('ok')",
    "m2crypto": "import M2Crypto; print('ok')",
    "borgbackup": "import borg; print('ok')",
    "evdev": "import evdev; print('ok')",
    "pyproj": "import pyproj; print(pyproj.__version__)",
    "openexr": "import OpenEXR; print('ok')",
}

IMPORT_NAMES = {
    "pillow": "PIL",
    "pyyaml": "yaml",
    "psycopg-binary": "psycopg",
    "psycopg2-binary": "psycopg2",
    "rpds-py": "rpds",
    "ruamel-yaml": "ruamel.yaml",
    "ruamel-yaml-clib": "ruamel.yaml",
    "mysqlclient": "MySQLdb",
    "pyzmq": "zmq",
    "pycairo": "cairo",
    "m2crypto": "M2Crypto",
    "borgbackup": "borg",
    "openexr": "OpenEXR",
}


def _import_check(name: str) -> str:
    if name in IMPORT_CHECKS:
        return IMPORT_CHECKS[name]
    import_name = IMPORT_NAMES.get(name, name.replace("-", "_"))
    return f"import {import_name}; print('ok')"


# Packages to skip (need special runtime, not on PyPI, etc.)
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

SOURCE_ONLY = {"psycopg2", "mysqlclient", "mariadb"}

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


# --- Docker helper ---

# Resolve paths once at import time (on the host)
_PROJECT_ROOT = Path(__file__).parent.parent
_ENTRYPOINT = Path(__file__).parent / "docker-test-lib.sh"


def _docker_env() -> dict:
    """Resolve nix/SSL/git paths needed for Docker containers."""
    nix_bin = os.popen("readlink -f $(which nix)").read().strip()
    nix_bin_dir = str(Path(nix_bin).parent)
    git_bin = os.popen("readlink -f $(which git)").read().strip()
    git_bin_dir = str(Path(git_bin).parent)
    git_core_dir = os.popen("git --exec-path").read().strip()
    ca_bundle = os.popen(
        "ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1"
    ).read().strip()
    uv_bin = os.environ.get("UV_BIN", str(_PROJECT_ROOT / "uv" / "target" / "debug" / "uv"))
    return {
        "uv_bin": uv_bin,
        "nix_bin_dir": nix_bin_dir,
        "git_bin_dir": git_bin_dir,
        "git_core_dir": git_core_dir,
        "ca_bundle": ca_bundle,
    }


# Cache docker env for the session
_DOCKER_ENV: dict | None = None


def _get_docker_env() -> dict:
    global _DOCKER_ENV
    if _DOCKER_ENV is None:
        _DOCKER_ENV = _docker_env()
    return _DOCKER_ENV


def run_in_container(
    package: str,
    check: str,
    no_binary: bool = False,
    timeout: int = 300,
) -> subprocess.CompletedProcess:
    """Run the library test entrypoint in a busybox container."""
    env = _get_docker_env()

    cmd = [
        "docker", "run", "--rm",
        "--network", "host",
        "-v", f"{env['uv_bin']}:/usr/local/bin/uv:ro",
        "-v", "/nix:/nix",
        "-v", f"{env['nix_bin_dir']}:/nix-bin:ro",
        "-v", f"{env['git_bin_dir']}:/git-bin:ro",
        "-v", f"{env['git_core_dir']}:/git-core:ro",
        "-v", f"{_ENTRYPOINT}:/entrypoint.sh:ro",
        "-e", "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin",
        "-e", "NIX_REMOTE=daemon",
        "-e", f"NIX_SSL_CERT_FILE={env['ca_bundle']}",
        "-e", f"SSL_CERT_FILE={env['ca_bundle']}",
        "-e", f"GIT_SSL_CAINFO={env['ca_bundle']}",
        "-e", f"GIT_EXEC_PATH=/git-core",
        "busybox",
        "/bin/sh", "/entrypoint.sh", package, check,
    ]
    if no_binary:
        cmd.append("--no-binary")

    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


# --- Test classes ---

class TestWheelInstall:
    """Test binary wheel install in isolated containers."""

    @pytest.mark.parametrize("package,check", WHEEL_TESTS)
    def test_wheel(self, package: str, check: str):
        result = run_in_container(package, check)
        assert result.returncode == 0, (
            f"{package} failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )


class TestSourceBuild:
    """Test source build (--no-binary) in isolated containers."""

    @pytest.mark.parametrize("package,check", SOURCE_BUILD_TESTS)
    def test_source_build(self, package: str, check: str):
        result = run_in_container(package, check, no_binary=True, timeout=600)
        assert result.returncode == 0, (
            f"{package} source build failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
