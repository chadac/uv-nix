// Shared test utilities — not all items are used by every test target.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

/// Path to the uv binary under test
pub static UV_BIN: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::var("UV_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("uv/target/debug/uv"))
});

/// Shared test directory
static TEST_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = std::env::temp_dir().join("uv-nix-tests");
    std::fs::create_dir_all(&dir).expect("Failed to create test directory");
    dir
});

/// Create (or reuse) a per-package venv for isolated testing.
fn get_package_venv(package: &str) -> PathBuf {
    let slug = package.replace(['[', ']', ' '], "_");
    let venv_path = TEST_DIR.join(format!("venv-{slug}"));

    if venv_path.join("bin/python").exists() {
        return venv_path;
    }

    let status = Command::new(UV_BIN.as_path())
        .args(["venv", venv_path.to_str().unwrap()])
        .status()
        .expect("Failed to create venv");

    assert!(status.success(), "venv creation failed for {package}");
    venv_path
}

/// Result of a package test
#[derive(Debug)]
pub struct TestResult {
    pub success: bool,
    pub skipped: bool,
    pub skip_reason: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

/// Check if a wheel is available for a package on the current platform.
///
/// Uses `uv pip install --dry-run --only-binary :all:` to check if uv would
/// download a wheel. If it would fall back to source, this returns None.
pub fn check_wheel_available(package: &str) -> Option<String> {
    let venv = get_package_venv(package);
    let python = venv.join("bin/python");

    // Use --dry-run with --only-binary to check if a wheel exists
    // If no wheel is available, this will fail with "no matching distribution"
    let output = Command::new(UV_BIN.as_path())
        .args([
            "pip",
            "install",
            "--dry-run",
            "--only-binary",
            ":all:",
            "--python",
            python.to_str().unwrap(),
            package,
        ])
        .output()
        .expect("wheel check command failed to execute");

    if output.status.success() {
        None // Wheel is available
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Some(format!(
            "No wheel available for {}: {}",
            package,
            stderr.lines().next().unwrap_or("unknown error")
        ))
    }
}

/// Install a package and run an import check.
///
/// If `require_wheel` is true and no wheel is available, returns a skipped result.
pub fn test_package(package: &str, import_check: &str, no_binary: bool) -> TestResult {
    test_package_impl(package, import_check, no_binary, false)
}

/// Install a package and run an import check, requiring a wheel.
///
/// If no wheel is available for the current platform/Python version, the test
/// is skipped rather than attempting a source build.
pub fn test_package_wheel_only(package: &str, import_check: &str) -> TestResult {
    test_package_impl(package, import_check, false, true)
}

fn test_package_impl(
    package: &str,
    import_check: &str,
    no_binary: bool,
    require_wheel: bool,
) -> TestResult {
    let venv = get_package_venv(package);
    let python = venv.join("bin/python");

    // Check wheel availability if required
    if require_wheel && let Some(reason) = check_wheel_available(package) {
        return TestResult {
            success: true, // Not a failure, just skipped
            skipped: true,
            skip_reason: Some(reason),
            stdout: String::new(),
            stderr: String::new(),
        };
    }

    // Build install command (use -v for debug output in CI)
    let mut install = Command::new(UV_BIN.as_path());
    install.args(["pip", "install", "-v", "--python", python.to_str().unwrap()]);
    // Bypass uv-nix build env cache so tests always exercise the full resolution path
    install.env("UV_NIX_NO_CACHE", "1");
    if no_binary {
        install.args(["--no-binary", package]);
        // Disable uv's sdist build cache for source builds — cached wheels may
        // have been compiled against a different nixpkgs and contain stale paths.
        install.arg("--no-cache");
    }
    install.arg(package);

    let install_out = install.output().expect("install command failed to execute");
    let install_stderr = String::from_utf8_lossy(&install_out.stderr).to_string();

    if !install_out.status.success() {
        return TestResult {
            success: false,
            skipped: false,
            skip_reason: None,
            stdout: String::from_utf8_lossy(&install_out.stdout).to_string(),
            stderr: format!(
                "Install failed (exit code {}):\n\n--- stderr (last 200 lines) ---\n{}",
                install_out.status.code().unwrap_or(-1),
                install_stderr
                    .lines()
                    .rev()
                    .take(200)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
    }

    // Run import check
    let check = Command::new(&python)
        .args(["-c", import_check])
        .output()
        .expect("python check command failed to execute");

    let check_stdout = String::from_utf8_lossy(&check.stdout).to_string();
    let check_stderr = String::from_utf8_lossy(&check.stderr).to_string();

    TestResult {
        success: check.status.success(),
        skipped: false,
        skip_reason: None,
        stdout: check_stdout,
        stderr: if check.status.success() {
            check_stderr
        } else {
            // Include install output for debugging when import fails
            format!(
                "Import failed (exit code {}):\n{check_stderr}\n\n--- install output ---\n{install_stderr}",
                check.status.code().unwrap_or(-1)
            )
        },
    }
}
