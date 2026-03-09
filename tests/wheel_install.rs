//! Wheel install tests for native Python packages.
//!
//! Tests that pre-built wheels install correctly and their native
//! extensions can be imported.

mod common;

use common::runner::{test_package, test_package_wheel_only};

/// Generate a wheel install test
macro_rules! wheel_test {
    ($name:ident, $pkg:expr, $check:expr) => {
        #[test]
        fn $name() {
            let result = test_package($pkg, $check, false);
            assert!(
                result.success,
                "{} failed:\nstdout: {}\nstderr: {}",
                $pkg, result.stdout, result.stderr
            );
        }
    };
}

/// Generate a wheel-only test that skips if no wheel is available.
///
/// These tests are for packages that are slow to build from source or may not
/// have wheels for all platforms. If no wheel is available for the current
/// platform/Python version, the test is skipped rather than failing.
macro_rules! wheel_test_wheel_only {
    ($name:ident, $pkg:expr, $check:expr) => {
        #[test]
        fn $name() {
            let result = test_package_wheel_only($pkg, $check);
            if result.skipped {
                eprintln!("SKIPPED: {}", result.skip_reason.as_deref().unwrap_or("no wheel available"));
                return;
            }
            assert!(
                result.success,
                "{} failed:\nstdout: {}\nstderr: {}",
                $pkg, result.stdout, result.stderr
            );
        }
    };
}

// =============================================================================
// Core packages - always tested
// =============================================================================

wheel_test!(test_numpy, "numpy", "import numpy; print(numpy.__version__)");
wheel_test!(test_pillow, "pillow", "from PIL import Image; print('ok')");
wheel_test!(test_cryptography, "cryptography",
    "from cryptography.hazmat.primitives.ciphers import Cipher; print('ok')");
wheel_test!(test_pyyaml, "pyyaml", "import yaml; yaml.CSafeLoader; print('ok')");
wheel_test!(test_lxml, "lxml", "from lxml import etree; print(etree.LXML_VERSION)");
wheel_test!(test_cffi, "cffi", "import _cffi_backend; print('ok')");
wheel_test!(test_markupsafe, "markupsafe", "import markupsafe; print(markupsafe.__version__)");
wheel_test!(test_msgpack, "msgpack", "import msgpack; print(msgpack.packb({'a': 1}))");
wheel_test!(test_ujson, "ujson", "import ujson; print(ujson.dumps({'a': 1}))");
wheel_test!(test_bcrypt, "bcrypt", "import bcrypt; print('ok')");
wheel_test!(test_orjson, "orjson", "import orjson; print(orjson.dumps({'a': 1}))");
wheel_test!(test_pydantic, "pydantic", "from pydantic import BaseModel; print('ok')");
wheel_test!(test_rpds_py, "rpds-py", "import rpds; print(rpds.HashTrieMap({'a': 1}))");
wheel_test!(test_regex, "regex", "import regex; print(regex.search(r'\\w+', 'hello').group())");
// Note: psycopg-binary provides C backend; psycopg[binary] installs both
wheel_test!(test_psycopg_binary, "psycopg[binary]", "import psycopg; print('ok')");
wheel_test!(test_aiohttp, "aiohttp", "import aiohttp; print(aiohttp.__version__)");
wheel_test!(test_asyncpg, "asyncpg", "import asyncpg; print('ok')");
wheel_test!(test_pynacl, "pynacl", "import nacl; print('ok')");
wheel_test!(test_argon2_cffi, "argon2-cffi-bindings", "import _argon2_cffi_bindings; print('ok')");
wheel_test!(test_dulwich, "dulwich", "import dulwich; print('ok')");

// =============================================================================
// Wheel-only packages - skipped if no wheel available for current platform
// =============================================================================

wheel_test_wheel_only!(test_scipy, "scipy", "import scipy; print(scipy.__version__)");
wheel_test_wheel_only!(test_pandas, "pandas", "import pandas; print(pandas.__version__)");
wheel_test_wheel_only!(test_grpcio, "grpcio", "import grpc; print(grpc.__version__)");
wheel_test_wheel_only!(test_h5py, "h5py", "import h5py; print(h5py.__version__)");
wheel_test_wheel_only!(test_matplotlib, "matplotlib", "import matplotlib; print(matplotlib.__version__)");
wheel_test_wheel_only!(test_tables, "tables", "import tables; print(tables.__version__)");
wheel_test_wheel_only!(test_av, "av", "import av; print(av.__version__)");
wheel_test_wheel_only!(test_imagecodecs, "imagecodecs", "import imagecodecs; print('ok')");
wheel_test_wheel_only!(test_pyproj, "pyproj", "import pyproj; print(pyproj.__version__)");
wheel_test_wheel_only!(test_tokenizers, "tokenizers", "import tokenizers; print('ok')");
wheel_test_wheel_only!(test_polars, "polars", "import polars; print(polars.__version__)");
wheel_test_wheel_only!(test_pyarrow, "pyarrow", "import pyarrow; print(pyarrow.__version__)");
wheel_test_wheel_only!(test_scikit_learn, "scikit-learn", "import sklearn; print(sklearn.__version__)");
wheel_test_wheel_only!(test_fiona, "fiona", "import fiona; print('ok')");
wheel_test_wheel_only!(test_rasterio, "rasterio", "import rasterio; print('ok')");

// =============================================================================
// Linux-only packages - skipped if no wheel available
// =============================================================================

#[test]
#[cfg(target_os = "linux")]
fn test_evdev() {
    let result = test_package_wheel_only("evdev", "import evdev; print('ok')");
    if result.skipped {
        eprintln!("SKIPPED: {}", result.skip_reason.as_deref().unwrap_or("no wheel available"));
        return;
    }
    assert!(result.success, "evdev failed: {}", result.stderr);
}

#[test]
#[cfg(target_os = "linux")]
fn test_uvloop() {
    let result = test_package("uvloop", "import uvloop; print('ok')", false);
    assert!(result.success, "uvloop failed: {}", result.stderr);
}
