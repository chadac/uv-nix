//! Nix-managed Python installation support.
//!
//! This module provides logic to detect and prefer Python from nixpkgs
//! instead of uv's managed Python installations when a compatible version
//! is available.
use fs_err as fs;

use anyhow::{Context, Result};
use semver::VersionReq;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::debug;

use crate::{config, nixpkgs};

/// Python version requirement extracted from project files.
#[derive(Debug, Clone)]
pub struct PythonRequirement {
    /// Semver requirement (e.g., ">=3.10,<3.13")
    pub version: VersionReq,
    /// Minor version if specified (e.g., "3.12" -> Some((3, 12)))
    pub minor: Option<(u8, u8)>,
}

/// Find the Python version requirement for a project.
///
/// Checks in order:
/// 1. `.python-version` file
/// 2. `uv.lock` (requires-python field)
/// 3. `pyproject.toml` (requires-python in [project])
pub fn find_python_requirement(project_dir: &Path) -> Option<PythonRequirement> {
    // Try .python-version first
    if let Some(req) = read_python_version_file(project_dir) {
        return Some(req);
    }

    // Try uv.lock
    if let Some(req) = read_uv_lock_python(project_dir) {
        return Some(req);
    }

    // Try pyproject.toml
    read_pyproject_python(project_dir)
}

/// Read Python version from `.python-version` file.
fn read_python_version_file(project_dir: &Path) -> Option<PythonRequirement> {
    let path = project_dir.join(".python-version");
    let content = fs::read_to_string(&path).ok()?.trim().to_string();

    parse_python_version(&content)
}

/// Read Python version requirement from `uv.lock`.
fn read_uv_lock_python(project_dir: &Path) -> Option<PythonRequirement> {
    let path = project_dir.join("uv.lock");
    let content = fs::read_to_string(&path).ok()?;

    // Look for "requires-python = " line
    for line in content.lines() {
        if let Some(version_str) = line.strip_prefix("requires-python = ") {
            let version_str = version_str.trim_matches('"').trim_matches('\'');
            return parse_python_version(version_str);
        }
    }

    None
}

/// Read Python version requirement from `pyproject.toml`.
fn read_pyproject_python(project_dir: &Path) -> Option<PythonRequirement> {
    let path = project_dir.join("pyproject.toml");
    let content = fs::read_to_string(&path).ok()?;
    let doc: toml::Value = toml::from_str(&content).ok()?;

    // Look for [project].requires-python
    let requires_python = doc.get("project")?.get("requires-python")?.as_str()?;

    parse_python_version(requires_python)
}

/// Parse a Python version string into a requirement.
///
/// Handles formats like:
/// - "3.12" -> minor version match
/// - "3.12.1" -> exact version
/// - ">=3.10,<3.13" -> version range
fn parse_python_version(version_str: &str) -> Option<PythonRequirement> {
    let version_str = version_str.trim();

    // If it's a simple "3.12" format, extract minor version
    if let Some((major, minor)) = parse_simple_version(version_str) {
        // Create a semver range for the minor version (3.12.* matches 3.12.0-3.12.999)
        let semver_str = format!("~{}.{}", major, minor);
        if let Ok(version) = VersionReq::parse(&semver_str) {
            return Some(PythonRequirement {
                version,
                minor: Some((major, minor)),
            });
        }
    }

    // Try parsing as semver range
    if let Ok(version) = VersionReq::parse(version_str) {
        return Some(PythonRequirement {
            version,
            minor: None,
        });
    }

    None
}

/// Parse a simple "3.12" or "3.12.1" version into (major, minor).
fn parse_simple_version(s: &str) -> Option<(u8, u8)> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() >= 2 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        Some((major, minor))
    } else {
        None
    }
}

/// Check if nixpkgs provides a Python version matching the requirement.
///
/// Returns the Python binary path if a match is found.
pub fn find_nixpkgs_python(
    project_dir: &Path,
    requirement: &PythonRequirement,
) -> Result<Option<PathBuf>> {
    // Get nixpkgs source
    let uv_nix_config = config::find_config(project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();
    let source = nixpkgs::resolve_nixpkgs(project_dir, &uv_nix_config);

    // If a specific minor version is requested, try to find that exact version
    if let Some((major, minor)) = requirement.minor {
        let attr = format!("python{}{}", major, minor);
        if let Ok(python_path) = resolve_python_from_nixpkgs(&attr, &source) {
            // Verify the version matches
            if let Ok(version) = get_python_version(&python_path) {
                if requirement.version.matches(&version) {
                    debug!(
                        "Found matching nixpkgs Python {}.{}: {}",
                        major,
                        minor,
                        python_path.display()
                    );
                    return Ok(Some(python_path));
                }
            }
        }
    }

    // Try python3 (default)
    if let Ok(python_path) = resolve_python_from_nixpkgs("python3", &source) {
        if let Ok(version) = get_python_version(&python_path) {
            if requirement.version.matches(&version) {
                debug!(
                    "Found matching default nixpkgs Python: {}",
                    python_path.display()
                );
                return Ok(Some(python_path));
            }
        }
    }

    Ok(None)
}

/// Resolve a Python binary path from nixpkgs.
fn resolve_python_from_nixpkgs(attr: &str, source: &nixpkgs::NixpkgsSource) -> Result<PathBuf> {
    let pkgs_expr = nixpkgs::nixpkgs_import_expr(source);
    let expr = if attr == "python3" {
        format!("({})", pkgs_expr)
    } else {
        format!("({}).{}", pkgs_expr, attr)
    };

    let mut cmd = crate::nix_command();
    cmd.args(["build", "--no-link", "--print-out-paths"]);
    if nixpkgs::requires_impure(source) {
        cmd.arg("--impure");
    }
    let output = cmd
        .arg("--expr")
        .arg(&expr)
        .output()
        .context("Failed to run nix build")?;

    if !output.status.success() {
        anyhow::bail!("nix build failed for {}", attr);
    }

    let store_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let python_bin = PathBuf::from(store_path).join("bin").join("python3");

    if python_bin.exists() {
        Ok(python_bin)
    } else {
        anyhow::bail!("Python binary not found in {}", python_bin.display())
    }
}

/// Get the version of a Python binary.
fn get_python_version(python_bin: &Path) -> Result<semver::Version> {
    let output = Command::new(python_bin)
        .arg("--version")
        .output()
        .context("Failed to get Python version")?;

    if !output.status.success() {
        anyhow::bail!("Failed to get Python version");
    }

    // Parse "Python 3.12.1" -> "3.12.1"
    let version_str = String::from_utf8_lossy(&output.stdout);
    let version_str = version_str
        .trim()
        .strip_prefix("Python ")
        .context("Invalid Python version output")?;

    semver::Version::parse(version_str)
        .with_context(|| format!("Failed to parse Python version: {}", version_str))
}
