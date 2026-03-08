//! Docker container utilities for integration tests.
//!
//! Only compiled when the `docker-tests` feature is enabled.

use std::path::PathBuf;
use std::process::Command;

/// Check if Docker is available on the system
pub fn is_docker_available() -> bool {
    Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Resolve paths needed for Docker containers with Nix mounts
fn resolve_docker_env() -> DockerEnv {
    let nix_bin = Command::new("sh")
        .args(["-c", "readlink -f $(which nix)"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let git_bin = Command::new("sh")
        .args(["-c", "readlink -f $(which git)"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let git_core_dir = Command::new("git")
        .arg("--exec-path")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let ca_bundle = Command::new("sh")
        .args([
            "-c",
            "ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let uv_bin = std::env::var("UV_BIN").unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("uv/target/debug/uv")
            .to_string_lossy()
            .to_string()
    });

    DockerEnv {
        uv_bin,
        nix_bin_dir: PathBuf::from(&nix_bin)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        git_bin_dir: PathBuf::from(&git_bin)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        git_core_dir,
        ca_bundle,
    }
}

struct DockerEnv {
    uv_bin: String,
    nix_bin_dir: String,
    git_bin_dir: String,
    git_core_dir: String,
    ca_bundle: String,
}

/// Run a shell script in a Docker container with Nix mounts
pub fn run_in_container(image: &str, script: &str) -> Result<String, String> {
    let env = resolve_docker_env();

    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--network",
            "host",
            "-v",
            &format!("{}:/usr/local/bin/uv:ro", env.uv_bin),
            "-v",
            "/nix:/nix",
            "-v",
            &format!("{}:/nix-bin:ro", env.nix_bin_dir),
            "-v",
            &format!("{}:/git-bin:ro", env.git_bin_dir),
            "-v",
            &format!("{}:/git-core:ro", env.git_core_dir),
            "-e",
            "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin",
            "-e",
            "NIX_REMOTE=daemon",
            "-e",
            &format!("NIX_SSL_CERT_FILE={}", env.ca_bundle),
            "-e",
            &format!("SSL_CERT_FILE={}", env.ca_bundle),
            "-e",
            &format!("GIT_SSL_CAINFO={}", env.ca_bundle),
            "-e",
            "GIT_EXEC_PATH=/git-core",
            image,
            "/bin/sh",
            "-c",
            script,
        ])
        .output()
        .map_err(|e| format!("Failed to run docker: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!("Container failed:\nstdout: {}\nstderr: {}", stdout, stderr))
    }
}
