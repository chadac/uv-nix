use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::config::UvNixConfig;

/// Describes how nixpkgs was resolved.
#[derive(Debug, Clone)]
pub enum NixpkgsSource {
    /// From a flake.lock file (pinned rev).
    FlakeLock { rev: String },
    /// From `[tool.uv-nix].nixpkgs` in pyproject.toml (flake ref).
    PyprojectToml { flake_ref: String },
    /// From a devenv.lock file (pinned rev).
    DevenvLock { rev: String },
    /// Auto-resolved from latest nixpkgs-unstable (written to pyproject.toml).
    AutoResolved { rev: String },
}

/// Resolve the nixpkgs source for a given project directory and config.
///
/// Priority: flake.lock → pyproject.toml pin → devenv.lock → auto-resolve + pin
pub fn resolve_nixpkgs(project_dir: &Path, config: &UvNixConfig) -> NixpkgsSource {
    // 1. flake.lock
    if let Some(rev) = parse_flake_lock(project_dir) {
        debug!("Resolved nixpkgs from flake.lock: {rev}");
        return NixpkgsSource::FlakeLock { rev };
    }

    // 2. Explicit pin in pyproject.toml
    if let Some(ref flake_ref) = config.nixpkgs {
        debug!("Using nixpkgs from pyproject.toml: {flake_ref}");
        return NixpkgsSource::PyprojectToml {
            flake_ref: flake_ref.clone(),
        };
    }

    // 3. devenv.lock
    if let Some(rev) = parse_devenv_lock(project_dir) {
        debug!("Resolved nixpkgs from devenv.lock: {rev}");
        return NixpkgsSource::DevenvLock { rev };
    }

    // 4. Auto-resolve latest nixpkgs-unstable and pin to pyproject.toml
    debug!("No nixpkgs pin found, auto-resolving from nixpkgs-unstable");
    match auto_resolve_nixpkgs(project_dir) {
        Some(rev) => {
            debug!("Auto-resolved nixpkgs rev: {rev}");
            NixpkgsSource::AutoResolved { rev }
        }
        None => {
            // Last resort: use a known recent rev (better than failing)
            tracing::warn!("Failed to auto-resolve nixpkgs, using hardcoded fallback");
            NixpkgsSource::AutoResolved {
                rev: "nixos-unstable".to_string(),
            }
        }
    }
}

/// Auto-resolve the latest nixpkgs-unstable commit via git ls-remote
/// and write the pin to pyproject.toml if it exists.
fn auto_resolve_nixpkgs(project_dir: &Path) -> Option<String> {
    let rev = resolve_latest_nixpkgs_rev()?;

    // Write pin to pyproject.toml if it exists
    let pyproject = project_dir.join("pyproject.toml");
    if pyproject.is_file() {
        if let Err(err) = write_nixpkgs_pin(&pyproject, &rev) {
            tracing::warn!("Failed to write nixpkgs pin to pyproject.toml: {err}");
        } else {
            debug!("Pinned nixpkgs rev {rev} in {}", pyproject.display());
        }
    }

    Some(rev)
}

/// Resolve the latest commit of nixpkgs-unstable via `git ls-remote`.
fn resolve_latest_nixpkgs_rev() -> Option<String> {
    let output = Command::new("git")
        .arg("ls-remote")
        .arg("https://github.com/NixOS/nixpkgs")
        .arg("refs/heads/nixpkgs-unstable")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    // Format: "<sha>\trefs/heads/nixpkgs-unstable"
    let rev = stdout.split_whitespace().next()?;
    if rev.len() >= 40 {
        Some(rev.to_string())
    } else {
        None
    }
}

/// Write a nixpkgs pin to pyproject.toml under `[tool.uv-nix]`.
fn write_nixpkgs_pin(pyproject_path: &Path, rev: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(pyproject_path)?;

    // Don't overwrite an existing pin
    if content.contains("nixpkgs") && content.contains("[tool.uv-nix]") {
        return Ok(());
    }

    let pin_value = format!("github:NixOS/nixpkgs/{rev}");

    let new_content = if content.contains("[tool.uv-nix]") {
        // Section exists, add nixpkgs after the header
        content.replace(
            "[tool.uv-nix]",
            &format!("[tool.uv-nix]\nnixpkgs = \"{pin_value}\""),
        )
    } else {
        // Append new section
        format!("{content}\n[tool.uv-nix]\nnixpkgs = \"{pin_value}\"\n")
    };

    std::fs::write(pyproject_path, new_content)?;
    Ok(())
}

/// Build a Nix expression that imports nixpkgs from the resolved source.
pub fn nixpkgs_import_expr(source: &NixpkgsSource) -> String {
    match source {
        NixpkgsSource::FlakeLock { rev }
        | NixpkgsSource::DevenvLock { rev }
        | NixpkgsSource::AutoResolved { rev } => {
            format!(
                "import (fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{rev}.tar.gz\") {{}}"
            )
        }
        NixpkgsSource::PyprojectToml { flake_ref } => {
            format!(
                "(builtins.getFlake \"{flake_ref}\").legacyPackages.${{builtins.currentSystem}}"
            )
        }
    }
}

/// Get a stable identifier for the nixpkgs source (used as cache key component).
pub fn nixpkgs_cache_key(source: &NixpkgsSource) -> String {
    match source {
        NixpkgsSource::FlakeLock { rev } => format!("flake-lock:{rev}"),
        NixpkgsSource::PyprojectToml { flake_ref } => format!("pyproject:{flake_ref}"),
        NixpkgsSource::DevenvLock { rev } => format!("devenv-lock:{rev}"),
        NixpkgsSource::AutoResolved { rev } => format!("auto:{rev}"),
    }
}

/// Resolved build paths (library, include, pkg-config, bin) from nixpkgs attrs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedBuildPaths {
    pub library_path: String,
    pub include_path: String,
    pub pkg_config_path: String,
    pub bin_path: String,
}

/// Resolve a list of nixpkgs attr paths to library, include, pkg-config, and bin paths
/// using `nix-build`. Unlike `nix eval`, this ensures the store paths are realized (built/fetched).
pub fn resolve_build_paths(
    attrs: &[String],
    source: &NixpkgsSource,
) -> anyhow::Result<ResolvedBuildPaths> {
    if attrs.is_empty() {
        return Ok(ResolvedBuildPaths {
            library_path: String::new(),
            include_path: String::new(),
            pkg_config_path: String::new(),
            bin_path: String::new(),
        });
    }

    let pkgs_expr = nixpkgs_import_expr(source);

    let attr_exprs: Vec<String> = attrs
        .iter()
        .map(|attr| {
            format!(
                "(pkgs.lib.getAttrFromPath (pkgs.lib.splitString \".\" \"{attr}\") pkgs)"
            )
        })
        .collect();

    let libs_list = attr_exprs.join("\n    ");

    // Use writeText to produce a JSON file. String interpolation of store paths
    // forces nix-build to realize (build/fetch) all referenced derivations.
    let expr = format!(
        r#"let pkgs = {pkgs_expr}; libs = [
    {libs_list}
  ]; in pkgs.writeText "uv-nix-build-paths.json" (builtins.toJSON {{
    lib = pkgs.lib.makeLibraryPath libs;
    include = builtins.concatStringsSep ":" (map (p: "${{pkgs.lib.getDev p}}/include") libs);
    pkgconfig = builtins.concatStringsSep ":" (map (p: "${{pkgs.lib.getDev p}}/lib/pkgconfig") libs);
    bin = builtins.concatStringsSep ":" (map (p: "${{pkgs.lib.getBin p}}/bin") libs);
  }})"#
    );

    debug!("Building nix expression for build paths");

    let output = crate::nix_command()
        .arg("build")
        .arg("--no-link")
        .arg("--print-out-paths")
        .arg("--impure")
        .arg("--expr")
        .arg(&expr)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix build failed: {}", stderr.trim());
    }

    let result_path = String::from_utf8(output.stdout)?.trim().to_string();
    let json_str = std::fs::read_to_string(&result_path)?;

    #[derive(Deserialize)]
    struct NixBuildPaths {
        lib: String,
        include: String,
        pkgconfig: String,
        bin: String,
    }

    let parsed: NixBuildPaths = serde_json::from_str(json_str.trim())?;
    let result = ResolvedBuildPaths {
        library_path: parsed.lib,
        include_path: parsed.include,
        pkg_config_path: parsed.pkgconfig,
        bin_path: parsed.bin,
    };
    debug!("Resolved build paths: {:?}", result);
    Ok(result)
}

/// Resolve a list of nixpkgs attr paths to a colon-separated library path string
/// using `nix eval`.
pub fn resolve_library_paths(
    attrs: &[String],
    source: &NixpkgsSource,
) -> anyhow::Result<String> {
    if attrs.is_empty() {
        return Ok(String::new());
    }

    let pkgs_expr = nixpkgs_import_expr(source);

    // Build the list of resolved attrs
    let attr_exprs: Vec<String> = attrs
        .iter()
        .map(|attr| {
            format!(
                "(pkgs.lib.getAttrFromPath (pkgs.lib.splitString \".\" \"{attr}\") pkgs)"
            )
        })
        .collect();

    let expr = format!(
        "let pkgs = {pkgs_expr}; in pkgs.lib.makeLibraryPath [\n  {}\n]",
        attr_exprs.join("\n  ")
    );

    debug!("Evaluating nix expression for extra libraries");

    let output = crate::nix_command()
        .arg("eval")
        .arg("--raw")
        .arg("--impure")
        .arg("--expr")
        .arg(&expr)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix eval failed: {}", stderr.trim());
    }

    let result = String::from_utf8(output.stdout)?.trim().to_string();
    debug!("Resolved extra library paths: {result}");
    Ok(result)
}

// --- Lock file parsing ---

/// Minimal flake.lock structure.
#[derive(Debug, Deserialize)]
struct FlakeLock {
    nodes: std::collections::HashMap<String, FlakeLockNode>,
    root: String,
}

#[derive(Debug, Deserialize)]
struct FlakeLockNode {
    inputs: Option<std::collections::HashMap<String, serde_json::Value>>,
    locked: Option<FlakeLocked>,
}

#[derive(Debug, Deserialize)]
struct FlakeLocked {
    owner: Option<String>,
    repo: Option<String>,
    rev: Option<String>,
}

/// Parse flake.lock to find the nixpkgs input's pinned rev.
fn parse_flake_lock(project_dir: &Path) -> Option<String> {
    let lock_path = project_dir.join("flake.lock");
    let content = std::fs::read_to_string(&lock_path).ok()?;
    let lock: FlakeLock = serde_json::from_str(&content).ok()?;

    // Find the root node and look for a "nixpkgs" input
    let root_node = lock.nodes.get(&lock.root)?;
    let inputs = root_node.inputs.as_ref()?;

    // The nixpkgs input might be named "nixpkgs" directly, or referenced by another name
    let nixpkgs_key = resolve_input_key(inputs, "nixpkgs")?;
    let nixpkgs_node = lock.nodes.get(&nixpkgs_key)?;
    let locked = nixpkgs_node.locked.as_ref()?;

    // Verify it's a GitHub nixpkgs repo
    if locked.owner.as_deref() == Some("NixOS") && locked.repo.as_deref() == Some("nixpkgs") {
        locked.rev.clone()
    } else {
        None
    }
}

/// Resolve an input key, handling both direct string references and
/// `follows`-style arrays.
fn resolve_input_key(
    inputs: &std::collections::HashMap<String, serde_json::Value>,
    name: &str,
) -> Option<String> {
    let value = inputs.get(name)?;
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(arr) => {
            // `follows` format: ["some", "path"] — use the last component
            arr.last()
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        _ => None,
    }
}

/// Minimal devenv.lock structure.
#[derive(Debug, Deserialize)]
struct DevenvLock {
    nodes: std::collections::HashMap<String, DevenvLockNode>,
}

#[derive(Debug, Deserialize)]
struct DevenvLockNode {
    locked: Option<DevenvLocked>,
    original: Option<DevenvOriginal>,
}

#[derive(Debug, Deserialize)]
struct DevenvLocked {
    rev: Option<String>,
    #[serde(rename = "type")]
    lock_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DevenvOriginal {
    owner: Option<String>,
    repo: Option<String>,
}

/// Parse devenv.lock to find a NixOS/nixpkgs pinned rev.
fn parse_devenv_lock(project_dir: &Path) -> Option<String> {
    let lock_path = project_dir.join("devenv.lock");
    let content = std::fs::read_to_string(&lock_path).ok()?;
    let lock: DevenvLock = serde_json::from_str(&content).ok()?;

    // Look for any node that points to NixOS/nixpkgs
    for (_name, node) in &lock.nodes {
        let original = node.original.as_ref()?;
        if original.owner.as_deref() == Some("NixOS") && original.repo.as_deref() == Some("nixpkgs")
        {
            if let Some(locked) = &node.locked {
                if locked.lock_type.as_deref() == Some("github") {
                    return locked.rev.clone();
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_flake_lock() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": {
        "lastModified": 1700000000,
        "narHash": "sha256-abc",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "abc123def456",
        "type": "github"
      },
      "original": {
        "owner": "NixOS",
        "ref": "nixos-unstable",
        "repo": "nixpkgs",
        "type": "github"
      }
    },
    "root": {
      "inputs": {
        "nixpkgs": "nixpkgs"
      }
    }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();

        let rev = parse_flake_lock(dir.path()).unwrap();
        assert_eq!(rev, "abc123def456");
    }

    #[test]
    fn test_parse_devenv_lock() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("devenv.lock"),
            r#"{
  "nodes": {
    "nixpkgs-stable": {
      "locked": {
        "rev": "def789ghi012",
        "type": "github"
      },
      "original": {
        "owner": "NixOS",
        "repo": "nixpkgs"
      }
    }
  }
}"#,
        )
        .unwrap();

        let rev = parse_devenv_lock(dir.path()).unwrap();
        assert_eq!(rev, "def789ghi012");
    }

    #[test]
    fn test_nixpkgs_import_expr() {
        let expr = nixpkgs_import_expr(&NixpkgsSource::FlakeLock {
            rev: "abc123".to_string(),
        });
        assert!(expr.contains("abc123"));
        assert!(expr.contains("fetchTarball"));

        let expr = nixpkgs_import_expr(&NixpkgsSource::AutoResolved {
            rev: "abc456".to_string(),
        });
        assert!(expr.contains("abc456"));
        assert!(expr.contains("fetchTarball"));

        let expr = nixpkgs_import_expr(&NixpkgsSource::PyprojectToml {
            flake_ref: "github:NixOS/nixpkgs/nixos-24.11".to_string(),
        });
        assert!(expr.contains("builtins.getFlake"));
    }
}
