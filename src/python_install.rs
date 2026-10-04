//! Nix-managed Python installation support.
//!
//! This module provides logic to detect and prefer Python from nixpkgs
//! instead of uv's managed Python installations when a compatible version
//! is available.
//!
//! Version parsing/matching reuses uv's own PEP 440 engine (`pep440_rs`, the
//! published form of uv's in-tree `uv_pep440`) rather than a bespoke parser,
//! so `requires-python` specifiers and `.python-version` pins are interpreted
//! exactly as uv interprets them.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use anyhow::{Context, Result};
use pep440_rs::{Version, VersionSpecifiers};
use tracing::debug;

use crate::{config, nixpkgs};

/// Python version requirement extracted from project files.
#[derive(Debug, Clone)]
pub struct PythonRequirement {
    /// PEP 440 specifiers a candidate interpreter must satisfy
    /// (e.g. `>=3.10,<3.13`, or `==3.12.*` for a bare `3.12` pin).
    pub specifiers: VersionSpecifiers,
    /// Minor version if a bare `3.12`/`3.12.1` pin was given, used to
    /// query the matching `pythonXY` nixpkgs attribute directly.
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
    let content = fs::read_to_string(&path).ok()?;
    let line = content.lines().next()?.trim();

    parse_python_pin(line)
}

/// Read Python version requirement from `uv.lock`.
fn read_uv_lock_python(project_dir: &Path) -> Option<PythonRequirement> {
    let path = project_dir.join("uv.lock");
    let content = fs::read_to_string(&path).ok()?;

    // Look for "requires-python = " line
    for line in content.lines() {
        if let Some(version_str) = line.strip_prefix("requires-python = ") {
            let version_str = version_str.trim().trim_matches('"').trim_matches('\'');
            return parse_requires_python(version_str);
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

    parse_requires_python(requires_python)
}

/// Parse a version request string into a requirement.
///
/// Accepts both a PEP 440 specifier set (`>=3.10,<3.13`) and a bare pin
/// (`3.12`, `cpython@3.12`), so uv's canonical request strings and the
/// contents of `.python-version` are both handled.
pub fn parse_request(request: &str) -> Option<PythonRequirement> {
    parse_requires_python(request).or_else(|| parse_python_pin(request))
}

/// Resolve the default `python3` from the project's nixpkgs.
pub fn resolve_default_python(project_dir: &Path) -> Result<PathBuf> {
    let uv_nix_config = config::find_config(project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();
    let source = nixpkgs::resolve_nixpkgs(project_dir, &uv_nix_config);
    resolve_python_from_nixpkgs("python3", &source)
}

/// Parse a `requires-python` value (a PEP 440 specifier set, e.g. `>=3.10,<3.13`).
fn parse_requires_python(value: &str) -> Option<PythonRequirement> {
    let specifiers = VersionSpecifiers::from_str(value.trim()).ok()?;
    Some(PythonRequirement {
        specifiers,
        minor: None,
    })
}

/// Parse a `.python-version` pin (a bare version like `3.12` or `3.12.1`,
/// optionally prefixed by an implementation such as `cpython@`).
///
/// A bare `major.minor` pin is treated as `==major.minor.*`; a full
/// `major.minor.patch` pin as `==major.minor.patch`.
fn parse_python_pin(pin: &str) -> Option<PythonRequirement> {
    // Strip an optional implementation prefix, e.g. `cpython@3.12` / `pypy@3.10`.
    let version_part = pin.rsplit(['@', '-']).next()?.trim();

    let parts: Vec<&str> = version_part.split('.').collect();
    let major: u8 = parts.first()?.parse().ok()?;
    let minor: u8 = parts.get(1)?.parse().ok()?;

    let spec_str = if parts.len() >= 3 && parts[2].chars().all(|c| c.is_ascii_digit()) {
        // Exact patch pin.
        format!("=={version_part}")
    } else {
        // Minor pin: match the whole minor series.
        format!("=={major}.{minor}.*")
    };

    let specifiers = VersionSpecifiers::from_str(&spec_str).ok()?;
    Some(PythonRequirement {
        specifiers,
        minor: Some((major, minor)),
    })
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
        let attr = format!("python{major}{minor}");
        if let Ok(python_path) = resolve_python_from_nixpkgs(&attr, &source)
            && let Ok(version) = get_python_version(&python_path)
            && requirement.specifiers.contains(&version)
        {
            debug!(
                "Found matching nixpkgs Python {}.{}: {}",
                major,
                minor,
                python_path.display()
            );
            return Ok(Some(python_path));
        }
    }

    // Try python3 (default)
    if let Ok(python_path) = resolve_python_from_nixpkgs("python3", &source)
        && let Ok(version) = get_python_version(&python_path)
        && requirement.specifiers.contains(&version)
    {
        debug!(
            "Found matching default nixpkgs Python: {}",
            python_path.display()
        );
        return Ok(Some(python_path));
    }

    Ok(None)
}

/// Resolve a Python binary path from nixpkgs.
fn resolve_python_from_nixpkgs(attr: &str, source: &nixpkgs::NixpkgsSource) -> Result<PathBuf> {
    let pkgs_expr = nixpkgs::nixpkgs_import_expr(source);
    let expr = if attr == "python3" {
        format!("({pkgs_expr})")
    } else {
        format!("({pkgs_expr}).{attr}")
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

/// Get the version of a Python binary as a PEP 440 version.
fn get_python_version(python_bin: &Path) -> Result<Version> {
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

    Version::from_str(version_str)
        .with_context(|| format!("Failed to parse Python version: {version_str}"))
}
