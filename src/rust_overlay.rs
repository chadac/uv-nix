use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::nixpkgs::NixpkgsSource;

/// Locked rust-overlay state, persisted to `.venv/share/uv-nix/locked.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LockedInputs {
    #[serde(default)]
    pub rust_overlay: Option<LockedRustOverlay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedRustOverlay {
    pub rev: String,
    pub resolved_version: String,
}

/// Result of checking whether a package needs a newer Rust toolchain.
#[derive(Debug)]
pub enum RustRequirement {
    /// nixpkgs rustc is sufficient
    Satisfied,
    /// Need a specific minimum version from rust-overlay
    NeedsOverlay { msrv: Version },
}

/// Scan a source directory for Cargo.toml and extract `rust-version` (MSRV).
///
/// Walks up to 2 levels deep looking for Cargo.toml with a `rust-version` key.
/// Returns None if no MSRV is specified or the directory doesn't contain Rust.
pub fn detect_msrv(source_dir: &Path) -> Option<Version> {
    // Try root Cargo.toml first
    if let Some(v) = parse_msrv_from_cargo_toml(&source_dir.join("Cargo.toml")) {
        return Some(v);
    }

    // Some packages nest their Rust code one level deep
    if let Ok(entries) = std::fs::read_dir(source_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let candidate = entry.path().join("Cargo.toml");
                if let Some(v) = parse_msrv_from_cargo_toml(&candidate) {
                    return Some(v);
                }
            }
        }
    }

    None
}

/// Parse `rust-version` from a Cargo.toml file.
fn parse_msrv_from_cargo_toml(path: &Path) -> Option<Version> {
    let content = std::fs::read_to_string(path).ok()?;
    let doc: toml::Value = toml::from_str(&content).ok()?;
    let rust_version_str = doc.get("package")?.get("rust-version")?.as_str()?;

    // rust-version can be "1.95" (2-component) or "1.95.0" (3-component)
    parse_rust_version(rust_version_str)
}

/// Parse a rust-version string, handling both "1.95" and "1.95.0" forms.
fn parse_rust_version(s: &str) -> Option<Version> {
    // Try direct parse first
    if let Ok(v) = Version::parse(s) {
        return Some(v);
    }
    // Try appending .0 for 2-component versions
    if let Ok(v) = Version::parse(&format!("{s}.0")) {
        return Some(v);
    }
    None
}

/// Get the rustc version from nixpkgs by running `nix eval`.
pub fn nixpkgs_rustc_version(source: &NixpkgsSource) -> anyhow::Result<Version> {
    let pkgs_expr = crate::nixpkgs::nixpkgs_import_expr(source);
    let expr = format!("({pkgs_expr}).rustc.version");

    let mut cmd = crate::nix_command();
    cmd.arg("eval").arg("--raw");
    if crate::nixpkgs::requires_impure(source) {
        cmd.arg("--impure");
    }
    let output = cmd.arg("--expr").arg(&expr).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix eval pkgs.rustc.version failed: {}", stderr.trim());
    }

    let version_str = String::from_utf8(output.stdout)?;
    parse_rust_version(version_str.trim())
        .ok_or_else(|| anyhow::anyhow!("failed to parse rustc version: {version_str}"))
}

/// Check if the package's MSRV is satisfied by nixpkgs' rustc.
pub fn check_rust_requirement(msrv: &Version, nixpkgs_version: &Version) -> RustRequirement {
    if nixpkgs_version >= msrv {
        RustRequirement::Satisfied
    } else {
        RustRequirement::NeedsOverlay { msrv: msrv.clone() }
    }
}

/// Resolve the rust-overlay flake rev (either from lock or by fetching latest).
///
/// Returns the locked rev and the store path of the resolved rust toolchain.
pub fn resolve_rust_toolchain(
    msrv: &Version,
    nixpkgs_source: &NixpkgsSource,
    project_dir: &Path,
) -> anyhow::Result<ResolvedRustToolchain> {
    let locked = load_locked_inputs(project_dir);
    let overlay_rev = match locked.as_ref().and_then(|l| l.rust_overlay.as_ref()) {
        Some(locked_overlay) => {
            // Check if locked version still satisfies our MSRV
            if let Some(locked_ver) = parse_rust_version(&locked_overlay.resolved_version) {
                if &locked_ver >= msrv {
                    debug!("Using locked rust-overlay rev: {}", locked_overlay.rev);
                    locked_overlay.rev.clone()
                } else {
                    debug!(
                        "Locked rust {} < MSRV {}, re-resolving overlay",
                        locked_overlay.resolved_version, msrv
                    );
                    resolve_latest_overlay_rev()?
                }
            } else {
                resolve_latest_overlay_rev()?
            }
        }
        None => resolve_latest_overlay_rev()?,
    };

    // Find the minimum stable version that satisfies the MSRV
    let rust_version = find_stable_version(msrv);
    let rust_version_str = format!(
        "{}.{}.{}",
        rust_version.major, rust_version.minor, rust_version.patch
    );

    crate::status(
        "Resolving",
        &format!(
            "rust {} via rust-overlay (nixpkgs too old)",
            rust_version_str
        ),
    );

    let toolchain = resolve_toolchain_path(&overlay_rev, &rust_version_str, nixpkgs_source)?;

    // Save lock
    save_locked_inputs(
        project_dir,
        &LockedInputs {
            rust_overlay: Some(LockedRustOverlay {
                rev: overlay_rev,
                resolved_version: rust_version_str.clone(),
            }),
        },
    );

    crate::status("Resolved", &format!("rust {rust_version_str}"));

    Ok(toolchain)
}

/// Resolved rust toolchain paths from nix store.
#[derive(Debug, Clone)]
pub struct ResolvedRustToolchain {
    /// Path to the toolchain's bin/ directory (contains cargo, rustc)
    pub bin_path: PathBuf,
}

/// Resolve the latest rev of oxalica/rust-overlay via git ls-remote.
fn resolve_latest_overlay_rev() -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args([
            "ls-remote",
            "https://github.com/oxalica/rust-overlay",
            "refs/heads/master",
        ])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to resolve rust-overlay rev via git ls-remote");
    }

    let stdout = String::from_utf8(output.stdout)?;
    let rev = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Empty git ls-remote output for rust-overlay"))?;

    if rev.len() < 40 {
        anyhow::bail!("Invalid rev from rust-overlay: {rev}");
    }

    Ok(rev.to_string())
}

/// Find the minimum stable Rust version that satisfies the given MSRV.
///
/// Since rust-overlay uses exact versions like "1.95.0", we just use the
/// MSRV directly (with .0 patch if needed).
fn find_stable_version(msrv: &Version) -> Version {
    Version::new(msrv.major, msrv.minor, msrv.patch)
}

/// Resolve the toolchain store path via nix build.
///
/// Uses the same nixpkgs as the project (via `nixpkgs_import_expr`) and applies
/// the rust-overlay on top, so the toolchain is built against the same stdenv.
fn resolve_toolchain_path(
    overlay_rev: &str,
    rust_version: &str,
    nixpkgs_source: &NixpkgsSource,
) -> anyhow::Result<ResolvedRustToolchain> {
    let pkgs_expr = crate::nixpkgs::nixpkgs_import_expr(nixpkgs_source);

    // Apply the rust-overlay to the already-resolved nixpkgs. The overlay just
    // adds `rust-bin` to the package set — it doesn't rebuild anything else.
    let expr = format!(
        r#"let
  basePkgs = {pkgs_expr};
  overlay = (builtins.getFlake "github:oxalica/rust-overlay/{overlay_rev}").overlays.default;
  pkgs = basePkgs.extend overlay;
in pkgs.rust-bin.stable."{rust_version}".default"#
    );

    let mut cmd = crate::nix_command();
    cmd.arg("build")
        .arg("--no-link")
        .arg("--print-out-paths")
        .arg("--impure")
        .arg("--expr")
        .arg(&expr);

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Failed to build rust toolchain via rust-overlay: {}",
            stderr.trim()
        );
    }

    let store_path = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(ResolvedRustToolchain {
        bin_path: PathBuf::from(format!("{store_path}/bin")),
    })
}

/// Load locked inputs from `.venv/share/uv-nix/locked.json` (searching upward for .venv).
fn load_locked_inputs(project_dir: &Path) -> Option<LockedInputs> {
    let lock_path = project_dir.join(".venv/share/uv-nix/locked.json");
    let content = std::fs::read_to_string(&lock_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Save locked inputs to `.venv/share/uv-nix/locked.json`.
fn save_locked_inputs(project_dir: &Path, inputs: &LockedInputs) {
    let dir = project_dir.join(".venv/share/uv-nix");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("locked.json");
    let content = serde_json::to_string_pretty(inputs).unwrap_or_default();
    let _ = std::fs::write(&path, content);
    debug!("Saved locked inputs to {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rust_version_three_component() {
        let v = parse_rust_version("1.95.0").unwrap();
        assert_eq!(v, Version::new(1, 95, 0));
    }

    #[test]
    fn test_parse_rust_version_two_component() {
        let v = parse_rust_version("1.95").unwrap();
        assert_eq!(v, Version::new(1, 95, 0));
    }

    #[test]
    fn test_parse_rust_version_invalid() {
        assert!(parse_rust_version("not-a-version").is_none());
    }

    #[test]
    fn test_check_rust_requirement_satisfied() {
        let msrv = Version::new(1, 70, 0);
        let nixpkgs = Version::new(1, 94, 1);
        assert!(matches!(
            check_rust_requirement(&msrv, &nixpkgs),
            RustRequirement::Satisfied
        ));
    }

    #[test]
    fn test_check_rust_requirement_needs_overlay() {
        let msrv = Version::new(1, 95, 0);
        let nixpkgs = Version::new(1, 94, 1);
        assert!(matches!(
            check_rust_requirement(&msrv, &nixpkgs),
            RustRequirement::NeedsOverlay { .. }
        ));
    }

    #[test]
    fn test_detect_msrv_from_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "test"
version = "0.1.0"
rust-version = "1.95"
"#,
        )
        .unwrap();

        let msrv = detect_msrv(dir.path()).unwrap();
        assert_eq!(msrv, Version::new(1, 95, 0));
    }

    #[test]
    fn test_detect_msrv_nested() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("rust-src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("Cargo.toml"),
            r#"[package]
name = "nested"
version = "0.1.0"
rust-version = "1.82"
"#,
        )
        .unwrap();

        let msrv = detect_msrv(dir.path()).unwrap();
        assert_eq!(msrv, Version::new(1, 82, 0));
    }

    #[test]
    fn test_detect_msrv_no_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_msrv(dir.path()).is_none());
    }

    #[test]
    fn test_detect_msrv_no_rust_version_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "test"
version = "0.1.0"
"#,
        )
        .unwrap();

        assert!(detect_msrv(dir.path()).is_none());
    }

    #[test]
    fn test_locked_inputs_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path().join(".venv/share/uv-nix");
        std::fs::create_dir_all(&venv).unwrap();

        let inputs = LockedInputs {
            rust_overlay: Some(LockedRustOverlay {
                rev: "abc123".to_string(),
                resolved_version: "1.95.0".to_string(),
            }),
        };

        save_locked_inputs(dir.path(), &inputs);
        let loaded = load_locked_inputs(dir.path()).unwrap();
        let overlay = loaded.rust_overlay.unwrap();
        assert_eq!(overlay.rev, "abc123");
        assert_eq!(overlay.resolved_version, "1.95.0");
    }
}
