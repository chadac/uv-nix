use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

/// Path to the uv binary under test
pub static UV_BIN: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::var("UV_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("uv/target/debug/uv")
        })
});

/// Shared test directory
static TEST_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = std::env::temp_dir().join("uv-nix-tests");
    std::fs::create_dir_all(&dir).expect("Failed to create test directory");
    dir
});

/// Shared venv for wheel tests (created once)
pub static SHARED_VENV: LazyLock<PathBuf> = LazyLock::new(|| {
    let venv_path = TEST_DIR.join("shared-venv");

    // If venv already exists and is valid, reuse it
    if venv_path.join("bin/python").exists() {
        return venv_path;
    }

    // Create new venv
    let status = Command::new(UV_BIN.as_path())
        .args(["venv", venv_path.to_str().unwrap()])
        .status()
        .expect("Failed to create shared venv");

    assert!(status.success(), "venv creation failed");
    venv_path
});

/// Result of a package test
#[derive(Debug)]
pub struct TestResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Install a package and run an import check
pub fn test_package(package: &str, import_check: &str, no_binary: bool) -> TestResult {
    let venv = SHARED_VENV.as_path();
    let python = venv.join("bin/python");

    // Build install command
    let mut install = Command::new(UV_BIN.as_path());
    install.args(["pip", "install", "--python", python.to_str().unwrap()]);
    if no_binary {
        install.args(["--no-binary", package]);
    }
    install.arg(package);

    let install_out = install.output().expect("install command failed to execute");
    if !install_out.status.success() {
        return TestResult {
            success: false,
            stdout: String::from_utf8_lossy(&install_out.stdout).to_string(),
            stderr: format!(
                "Install failed:\n{}",
                String::from_utf8_lossy(&install_out.stderr)
            ),
        };
    }

    // Run import check
    let check = Command::new(&python)
        .args(["-c", import_check])
        .output()
        .expect("python check command failed to execute");

    TestResult {
        success: check.status.success(),
        stdout: String::from_utf8_lossy(&check.stdout).to_string(),
        stderr: String::from_utf8_lossy(&check.stderr).to_string(),
    }
}

/// Run a test in a Docker container (requires docker-tests feature)
#[cfg(feature = "docker-tests")]
pub fn test_package_in_container(
    package: &str,
    import_check: &str,
    no_binary: bool,
    image: &str,
) -> TestResult {
    use super::docker::run_in_container;

    let no_binary_flag = if no_binary {
        format!("--no-binary {}", package)
    } else {
        String::new()
    };

    let script = format!(
        r#"
        cd /tmp
        uv init --no-progress test-project
        cd test-project
        uv add --no-progress {} {}
        uv run --no-progress python -c "{}"
        "#,
        no_binary_flag, package, import_check
    );

    match run_in_container(image, &script) {
        Ok(stdout) => TestResult {
            success: true,
            stdout,
            stderr: String::new(),
        },
        Err(stderr) => TestResult {
            success: false,
            stdout: String::new(),
            stderr,
        },
    }
}
