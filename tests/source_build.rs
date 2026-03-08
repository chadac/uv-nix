//! Source build tests for native Python packages.
//!
//! Tests that packages can be built from source distributions
//! (using --no-binary) and their native extensions work correctly.
//!
//! These tests are always marked as ignored by default since source
//! builds are slow. Run with: cargo test --test source_build -- --ignored

mod common;

use common::runner::test_package;

/// Generate a source build test (always ignored, run explicitly)
macro_rules! source_test {
    ($name:ident, $pkg:expr, $check:expr) => {
        #[test]
        #[ignore]
        fn $name() {
            let result = test_package($pkg, $check, true);
            assert!(
                result.success,
                "{} source build failed:\nstdout: {}\nstderr: {}",
                $pkg, result.stdout, result.stderr
            );
        }
    };
}

// =============================================================================
// Source build tests
// =============================================================================

// Fast source builds (< 1 minute)
source_test!(test_pyyaml_source, "pyyaml", "import yaml; yaml.CSafeLoader; print('ok')");
source_test!(test_cffi_source, "cffi", "import _cffi_backend; print('ok')");
source_test!(test_markupsafe_source, "markupsafe", "import markupsafe; print('ok')");
source_test!(test_msgpack_source, "msgpack", "import msgpack; print('ok')");
source_test!(test_ujson_source, "ujson", "import ujson; print('ok')");

// Source-only packages (no wheels available)
source_test!(test_psycopg2_source, "psycopg2", "import psycopg2; print(psycopg2.__version__)");
source_test!(test_mysqlclient_source, "mysqlclient", "import MySQLdb; print('ok')");

// Rust-based packages (require cargo)
source_test!(test_bcrypt_source, "bcrypt", "import bcrypt; print('ok')");
source_test!(test_orjson_source, "orjson", "import orjson; print('ok')");
source_test!(test_cryptography_source, "cryptography",
    "from cryptography.hazmat.primitives.ciphers import Cipher; print('ok')");
source_test!(test_rpds_py_source, "rpds-py", "import rpds; print('ok')");
source_test!(test_pydantic_core_source, "pydantic-core",
    "from pydantic_core import PydanticUndefined; print('ok')");

// Cython packages
source_test!(test_lxml_source, "lxml", "from lxml import etree; print('ok')");
source_test!(test_aiohttp_source, "aiohttp", "import aiohttp; print('ok')");

// Slow source builds (1+ minutes)
source_test!(test_numpy_source, "numpy", "import numpy; print(numpy.__version__)");
source_test!(test_pillow_source, "pillow", "from PIL import Image; print('ok')");
source_test!(test_grpcio_source, "grpcio", "import grpc; print('ok')");
source_test!(test_scipy_source, "scipy", "import scipy; print('ok')");
source_test!(test_h5py_source, "h5py", "import h5py; print('ok')");
source_test!(test_matplotlib_source, "matplotlib", "import matplotlib; print('ok')");
