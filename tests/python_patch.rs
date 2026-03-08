//! Tests for Python interpreter patching via Nix derivation.
//!
//! Verifies that `uv python install` produces a correctly patched Python
//! that can import stdlib modules including C extensions.

mod common;

use common::runner::UV_BIN;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_python_install_creates_nix_ref() {
    let home = tempdir().unwrap();

    let output = Command::new(UV_BIN.as_path())
        .args(["python", "install", "3.12"])
        .env("HOME", home.path())
        .output()
        .expect("python install failed to execute");

    assert!(
        output.status.success(),
        "python install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Check for .nix symlink
    let python_dir = home.path().join(".local/share/uv/python");

    assert!(python_dir.exists(), "Python directory not created");

    let entries: Vec<_> = std::fs::read_dir(&python_dir)
        .expect("Failed to read python dir")
        .filter_map(|e| e.ok())
        .collect();

    let has_nix_ref = entries
        .iter()
        .any(|e| e.path().extension().map(|x| x == "nix").unwrap_or(false));

    assert!(
        has_nix_ref,
        "No .nix reference found in {:?}. Entries: {:?}",
        python_dir,
        entries.iter().map(|e| e.path()).collect::<Vec<_>>()
    );
}

#[test]
fn test_patched_python_runs() {
    let home = tempdir().unwrap();

    let install = Command::new(UV_BIN.as_path())
        .args(["python", "install", "3.12"])
        .env("HOME", home.path())
        .output()
        .expect("python install failed");

    assert!(install.status.success());

    // Find the installed Python binary
    let python_dir = home.path().join(".local/share/uv/python");
    let python_bin = find_python_binary(&python_dir);

    let output = Command::new(&python_bin)
        .args(["-c", "import sys; print(sys.version)"])
        .output()
        .expect("python failed to run");

    assert!(output.status.success(), "Python failed to run");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("3.12"),
        "Unexpected Python version"
    );
}

#[test]
fn test_patched_python_imports_ssl() {
    let home = tempdir().unwrap();

    Command::new(UV_BIN.as_path())
        .args(["python", "install", "3.12"])
        .env("HOME", home.path())
        .status()
        .expect("python install failed");

    let python_dir = home.path().join(".local/share/uv/python");
    let python_bin = find_python_binary(&python_dir);

    let output = Command::new(&python_bin)
        .args(["-c", "import ssl; print(ssl.OPENSSL_VERSION)"])
        .output()
        .expect("ssl import check failed");

    assert!(
        output.status.success(),
        "SSL import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("OpenSSL"),
        "Unexpected SSL output"
    );
}

#[test]
fn test_patched_python_stdlib_imports() {
    let home = tempdir().unwrap();

    Command::new(UV_BIN.as_path())
        .args(["python", "install", "3.12"])
        .env("HOME", home.path())
        .status()
        .expect("python install failed");

    let python_dir = home.path().join(".local/share/uv/python");
    let python_bin = find_python_binary(&python_dir);

    let check_script = r#"
import os, sys, json, ssl, sqlite3, ctypes, hashlib, zlib
print('stdlib ok')
"#;

    let output = Command::new(&python_bin)
        .args(["-c", check_script])
        .output()
        .expect("stdlib import check failed");

    assert!(
        output.status.success(),
        "Stdlib imports failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("stdlib ok"),
        "Unexpected output"
    );
}

/// Find the Python binary in the uv python directory
fn find_python_binary(python_dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(python_dir)
        .expect("Failed to read python dir")
        .filter_map(|e| e.ok())
        .find(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("cpython-3.12") && !name.ends_with(".nix")
        })
        .map(|e| e.path().join("bin/python3.12"))
        .expect("Python binary not found")
}
