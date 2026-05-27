use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::config::{UvNixConfig, UseSource};

/// Describes how nixpkgs was resolved.
#[derive(Debug, Clone)]
pub enum NixpkgsSource {
    /// Explicit pin from `[tool.uv-nix].nixpkgs` (flake ref).
    ExplicitPin { flake_ref: String },
    /// From a flake.lock file (pinned rev).
    FlakeLock { rev: String },
    /// From a devenv.lock file (pinned rev).
    DevenvLock { rev: String },
    /// From a .flox/env/manifest.lock file (pinned rev).
    FloxLock { rev: String },
    /// Auto-resolved from latest nixpkgs-unstable (written to pyproject.toml).
    AutoResolved { rev: String },
}

/// Resolve the nixpkgs source for a given project directory and config.
///
/// Priority:
/// 1. `[tool.uv-nix].nixpkgs` — explicit flake ref, always wins
/// 2. `[tool.uv-nix].use` — go directly to that source (error if not found)
/// 3. Auto-detect chain: flake.lock → devenv.lock → .flox/env/manifest.lock
/// 4. Auto-resolve latest nixpkgs-unstable + pin to pyproject.toml
pub fn resolve_nixpkgs(project_dir: &Path, config: &UvNixConfig) -> NixpkgsSource {
    // 1. Explicit pin in pyproject.toml
    if let Some(ref flake_ref) = config.nixpkgs {
        debug!("Using nixpkgs from pyproject.toml: {flake_ref}");
        return NixpkgsSource::ExplicitPin {
            flake_ref: flake_ref.clone(),
        };
    }

    // 2. Explicit source selection via `use`
    if let Some(ref source) = config.use_source {
        debug!("Using nixpkgs source from [tool.uv-nix].use: {source:?}");
        return resolve_from_source(project_dir, config, source);
    }

    // 3. Auto-detect chain
    if let Some(source) = auto_detect(project_dir, config) {
        return source;
    }

    // 4. Auto-resolve latest nixpkgs-unstable and pin to pyproject.toml
    debug!("No nixpkgs pin found, auto-resolving from nixpkgs-unstable");
    match auto_resolve_nixpkgs(project_dir) {
        Some(rev) => {
            debug!("Auto-resolved nixpkgs rev: {rev}");
            NixpkgsSource::AutoResolved { rev }
        }
        None => {
            tracing::warn!("Failed to auto-resolve nixpkgs, using hardcoded fallback");
            NixpkgsSource::AutoResolved {
                rev: "nixos-unstable".to_string(),
            }
        }
    }
}

/// Resolve from a specific source (when `[tool.uv-nix].use` is set).
/// Warns if the lockfile is not found rather than silently falling through.
fn resolve_from_source(
    project_dir: &Path,
    config: &UvNixConfig,
    source: &UseSource,
) -> NixpkgsSource {
    let custom_path = config.lock_path_for(source);

    match source {
        UseSource::FlakeLock => {
            let path = custom_path.unwrap_or("flake.lock");
            let lock_path = project_dir.join(path);
            match parse_flake_lock(&lock_path) {
                Some(rev) => {
                    debug!("Resolved nixpkgs from {}: {rev}", lock_path.display());
                    NixpkgsSource::FlakeLock { rev }
                }
                None => {
                    tracing::warn!(
                        "use = \"flake.lock\" but {} not found or has no nixpkgs input",
                        lock_path.display()
                    );
                    fallback_auto_resolve(project_dir)
                }
            }
        }
        UseSource::Devenv => {
            let path = custom_path.unwrap_or("devenv.lock");
            let lock_path = project_dir.join(path);
            match parse_devenv_lock(&lock_path) {
                Some(rev) => {
                    debug!("Resolved nixpkgs from {}: {rev}", lock_path.display());
                    NixpkgsSource::DevenvLock { rev }
                }
                None => {
                    tracing::warn!(
                        "use = \"devenv\" but {} not found or has no nixpkgs input",
                        lock_path.display()
                    );
                    fallback_auto_resolve(project_dir)
                }
            }
        }
        UseSource::Flox => {
            let path = custom_path.unwrap_or(".flox/env/manifest.lock");
            let lock_path = project_dir.join(path);
            match parse_flox_lock(&lock_path) {
                Some(rev) => {
                    debug!("Resolved nixpkgs from {}: {rev}", lock_path.display());
                    NixpkgsSource::FloxLock { rev }
                }
                None => {
                    tracing::warn!(
                        "use = \"flox\" but {} not found or has no nixpkgs rev",
                        lock_path.display()
                    );
                    fallback_auto_resolve(project_dir)
                }
            }
        }
    }
}

/// Auto-detect nixpkgs from available lockfiles.
/// Tries: flake.lock → devenv.lock → .flox/env/manifest.lock
fn auto_detect(project_dir: &Path, config: &UvNixConfig) -> Option<NixpkgsSource> {
    // flake.lock
    let flake_path = config
        .lock_path_for(&UseSource::FlakeLock)
        .unwrap_or("flake.lock");
    if let Some(rev) = parse_flake_lock(&project_dir.join(flake_path)) {
        debug!("Resolved nixpkgs from flake.lock: {rev}");
        return Some(NixpkgsSource::FlakeLock { rev });
    }

    // devenv.lock
    let devenv_path = config
        .lock_path_for(&UseSource::Devenv)
        .unwrap_or("devenv.lock");
    if let Some(rev) = parse_devenv_lock(&project_dir.join(devenv_path)) {
        debug!("Resolved nixpkgs from devenv.lock: {rev}");
        return Some(NixpkgsSource::DevenvLock { rev });
    }

    // .flox/env/manifest.lock
    let flox_path = config
        .lock_path_for(&UseSource::Flox)
        .unwrap_or(".flox/env/manifest.lock");
    if let Some(rev) = parse_flox_lock(&project_dir.join(flox_path)) {
        debug!("Resolved nixpkgs from .flox/env/manifest.lock: {rev}");
        return Some(NixpkgsSource::FloxLock { rev });
    }

    None
}

/// Fallback when an explicit `use` source fails to resolve.
fn fallback_auto_resolve(project_dir: &Path) -> NixpkgsSource {
    match auto_resolve_nixpkgs(project_dir) {
        Some(rev) => NixpkgsSource::AutoResolved { rev },
        None => NixpkgsSource::AutoResolved {
            rev: "nixos-unstable".to_string(),
        },
    }
}

/// Auto-resolve the latest nixpkgs-unstable commit via git ls-remote
/// and write the pin to pyproject.toml if it exists.
fn auto_resolve_nixpkgs(project_dir: &Path) -> Option<String> {
    let rev = resolve_latest_nixpkgs_rev()?;

    // Write pin to pyproject.toml if it exists
    let pyproject = project_dir.join("pyproject.toml");
    if pyproject.is_file() {
        crate::status("Pinning", &format!("nixpkgs-unstable ({}) in pyproject.toml", &rev[..12]));
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
///
/// Uses toml_edit to preserve existing formatting, comments, and ordering.
fn write_nixpkgs_pin(pyproject_path: &Path, rev: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(pyproject_path)?;
    let mut doc: toml_edit::DocumentMut = content.parse()?;

    let pin_value = format!("github:NixOS/nixpkgs/{rev}");

    // Navigate to tool.uv-nix, creating sections as needed
    let tool = doc.entry("tool").or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let tool_table = tool.as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[tool] is not a table"))?;
    tool_table.set_implicit(true);

    let uv_nix = tool_table.entry("uv-nix").or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let uv_nix_table = uv_nix.as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[tool.uv-nix] is not a table"))?;

    // Don't overwrite an existing pin
    if uv_nix_table.contains_key("nixpkgs") {
        return Ok(());
    }

    uv_nix_table.insert("nixpkgs", toml_edit::value(&pin_value));

    std::fs::write(pyproject_path, doc.to_string())?;
    Ok(())
}

/// Build a Nix expression that imports nixpkgs from the resolved source.
///
/// Uses `builtins.fetchTree` with an explicit system string so the expression
/// is pure (no `--impure` needed) for rev-based sources.
///
/// For explicit flake ref pins, uses `builtins.getFlake` which requires `--impure`.
pub fn nixpkgs_import_expr(source: &NixpkgsSource) -> String {
    let system = crate::current_system();
    match source {
        NixpkgsSource::FlakeLock { rev }
        | NixpkgsSource::DevenvLock { rev }
        | NixpkgsSource::FloxLock { rev }
        | NixpkgsSource::AutoResolved { rev } => {
            format!(
                "import (builtins.fetchTree {{ type = \"github\"; owner = \"NixOS\"; repo = \"nixpkgs\"; rev = \"{rev}\"; }}) {{ system = \"{system}\"; }}"
            )
        }
        NixpkgsSource::ExplicitPin { flake_ref } => {
            format!(
                "(builtins.getFlake \"{flake_ref}\").legacyPackages.\"{system}\""
            )
        }
    }
}

/// Whether the given source requires `--impure` for nix evaluation.
pub fn requires_impure(source: &NixpkgsSource) -> bool {
    matches!(source, NixpkgsSource::ExplicitPin { .. })
}

/// Get a stable identifier for the nixpkgs source (used as cache key component).
pub fn nixpkgs_cache_key(source: &NixpkgsSource) -> String {
    match source {
        NixpkgsSource::ExplicitPin { flake_ref } => format!("explicit:{flake_ref}"),
        NixpkgsSource::FlakeLock { rev } => format!("flake-lock:{rev}"),
        NixpkgsSource::DevenvLock { rev } => format!("devenv-lock:{rev}"),
        NixpkgsSource::FloxLock { rev } => format!("flox-lock:{rev}"),
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

    let mut cmd = crate::nix_command();
    cmd.arg("build")
        .arg("--no-link")
        .arg("--print-out-paths");
    if requires_impure(source) {
        cmd.arg("--impure");
    }
    let output = cmd
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

    let mut cmd = crate::nix_command();
    cmd.arg("eval")
        .arg("--raw");
    if requires_impure(source) {
        cmd.arg("--impure");
    }
    let output = cmd
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

// =============================================================================
// Lock file parsers
// =============================================================================

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
fn parse_flake_lock(lock_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(lock_path).ok()?;
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
fn parse_devenv_lock(lock_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(lock_path).ok()?;
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

/// Minimal flox manifest.lock structure.
///
/// Flox stores packages as an array. Each entry has a `rev` field directly
/// and a `locked_url` like `https://github.com/flox/nixpkgs?rev=<hash>`.
/// Note: Flox uses their own fork (flox/nixpkgs), not NixOS/nixpkgs.
#[derive(Debug, Deserialize)]
struct FloxManifestLock {
    packages: Option<Vec<FloxPackageEntry>>,
}

#[derive(Debug, Deserialize)]
struct FloxPackageEntry {
    rev: Option<String>,
}

/// Parse .flox/env/manifest.lock to find a nixpkgs rev.
///
/// Extracts the `rev` field from the first package entry that has one.
/// All packages in a flox lock typically share the same nixpkgs rev.
fn parse_flox_lock(lock_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(lock_path).ok()?;
    let lock: FloxManifestLock = serde_json::from_str(&content).ok()?;

    let packages = lock.packages?;
    for pkg in &packages {
        if let Some(ref rev) = pkg.rev {
            if rev.len() >= 40 {
                return Some(rev.clone());
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

        let rev = parse_flake_lock(&dir.path().join("flake.lock")).unwrap();
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

        let rev = parse_devenv_lock(&dir.path().join("devenv.lock")).unwrap();
        assert_eq!(rev, "def789ghi012");
    }

    #[test]
    fn test_parse_flox_lock() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("manifest.lock"),
            r#"{
  "lockfile-version": 1,
  "manifest": {
    "schema-version": "1.12.0",
    "install": { "python3": { "pkg-path": "python3" } }
  },
  "packages": [
    {
      "attr_path": "python3",
      "install_id": "python3",
      "locked_url": "https://github.com/flox/nixpkgs?rev=64c08a7ca051951c8eae34e3e3cb1e202fe36786",
      "rev": "64c08a7ca051951c8eae34e3e3cb1e202fe36786",
      "version": "3.13.13",
      "system": "x86_64-linux"
    }
  ]
}"#,
        )
        .unwrap();

        let rev = parse_flox_lock(&dir.path().join("manifest.lock")).unwrap();
        assert_eq!(rev, "64c08a7ca051951c8eae34e3e3cb1e202fe36786");
    }

    #[test]
    fn test_parse_flox_lock_no_packages() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("manifest.lock"),
            r#"{ "lockfile-version": 1, "manifest": {}, "packages": [] }"#,
        )
        .unwrap();

        assert!(parse_flox_lock(&dir.path().join("manifest.lock")).is_none());
    }

    #[test]
    fn test_nixpkgs_import_expr() {
        let expr = nixpkgs_import_expr(&NixpkgsSource::FlakeLock {
            rev: "abc123".to_string(),
        });
        assert!(expr.contains("abc123"));
        assert!(expr.contains("fetchTree"));
        assert!(expr.contains("system"));
        assert!(!requires_impure(&NixpkgsSource::FlakeLock {
            rev: "abc123".to_string(),
        }));

        let expr = nixpkgs_import_expr(&NixpkgsSource::AutoResolved {
            rev: "abc456".to_string(),
        });
        assert!(expr.contains("abc456"));
        assert!(expr.contains("fetchTree"));

        let expr = nixpkgs_import_expr(&NixpkgsSource::ExplicitPin {
            flake_ref: "github:NixOS/nixpkgs/nixos-24.11".to_string(),
        });
        assert!(expr.contains("builtins.getFlake"));
        assert!(requires_impure(&NixpkgsSource::ExplicitPin {
            flake_ref: "github:NixOS/nixpkgs/nixos-24.11".to_string(),
        }));

        let expr = nixpkgs_import_expr(&NixpkgsSource::FloxLock {
            rev: "flox123".to_string(),
        });
        assert!(expr.contains("flox123"));
        assert!(expr.contains("fetchTree"));
    }

    #[test]
    fn test_resolve_nixpkgs_explicit_pin_wins() {
        let dir = tempfile::tempdir().unwrap();
        // Write a flake.lock that would normally be picked up
        fs::write(
            dir.path().join("flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "flake-rev", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();

        let config = UvNixConfig {
            nixpkgs: Some("github:NixOS/nixpkgs/my-explicit-pin".to_string()),
            ..Default::default()
        };

        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::ExplicitPin { flake_ref } => {
                assert_eq!(flake_ref, "github:NixOS/nixpkgs/my-explicit-pin");
            }
            other => panic!("Expected ExplicitPin, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_nixpkgs_auto_detect_flake_lock() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "detected-rev", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();

        let config = UvNixConfig::default();
        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::FlakeLock { rev } => assert_eq!(rev, "detected-rev"),
            other => panic!("Expected FlakeLock, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_nixpkgs_use_directive() {
        let dir = tempfile::tempdir().unwrap();

        // Write both flake.lock and devenv.lock
        fs::write(
            dir.path().join("flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "flake-rev", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("devenv.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "rev": "devenv-rev", "type": "github" },
      "original": { "owner": "NixOS", "repo": "nixpkgs" }
    }
  }
}"#,
        )
        .unwrap();

        // use = "devenv" should skip flake.lock and go to devenv.lock
        let config = UvNixConfig {
            use_source: Some(UseSource::Devenv),
            ..Default::default()
        };

        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::DevenvLock { rev } => assert_eq!(rev, "devenv-rev"),
            other => panic!("Expected DevenvLock, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_nixpkgs_auto_detect_devenv_when_no_flake() {
        let dir = tempfile::tempdir().unwrap();
        // Only devenv.lock, no flake.lock
        fs::write(
            dir.path().join("devenv.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "rev": "devenv-only-rev", "type": "github" },
      "original": { "owner": "NixOS", "repo": "nixpkgs" }
    }
  }
}"#,
        )
        .unwrap();

        let config = UvNixConfig::default();
        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::DevenvLock { rev } => assert_eq!(rev, "devenv-only-rev"),
            other => panic!("Expected DevenvLock, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_nixpkgs_auto_detect_flox_when_no_flake_or_devenv() {
        let dir = tempfile::tempdir().unwrap();
        // Only .flox/env/manifest.lock
        fs::create_dir_all(dir.path().join(".flox/env")).unwrap();
        fs::write(
            dir.path().join(".flox/env/manifest.lock"),
            r#"{
  "lockfile-version": 1,
  "manifest": {},
  "packages": [
    {
      "attr_path": "python3",
      "rev": "abcdef1234567890abcdef1234567890abcdef12",
      "version": "3.13.0",
      "system": "x86_64-linux"
    }
  ]
}"#,
        )
        .unwrap();

        let config = UvNixConfig::default();
        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::FloxLock { rev } => {
                assert_eq!(rev, "abcdef1234567890abcdef1234567890abcdef12")
            }
            other => panic!("Expected FloxLock, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_nixpkgs_flake_lock_wins_over_devenv() {
        let dir = tempfile::tempdir().unwrap();
        // Both flake.lock and devenv.lock present
        fs::write(
            dir.path().join("flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "flake-wins", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("devenv.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "rev": "devenv-loses", "type": "github" },
      "original": { "owner": "NixOS", "repo": "nixpkgs" }
    }
  }
}"#,
        )
        .unwrap();

        let config = UvNixConfig::default();
        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::FlakeLock { rev } => assert_eq!(rev, "flake-wins"),
            other => panic!("Expected FlakeLock, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_nixpkgs_custom_lock_path() {
        let dir = tempfile::tempdir().unwrap();
        // Write flake.lock in a subdirectory
        fs::create_dir_all(dir.path().join("subdir")).unwrap();
        fs::write(
            dir.path().join("subdir/flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "custom-path-rev", "type": "github" }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();

        let config = UvNixConfig {
            use_source: Some(UseSource::FlakeLock),
            flake: Some(crate::config::SourceConfig {
                lock: Some("subdir/flake.lock".to_string()),
            }),
            ..Default::default()
        };

        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::FlakeLock { rev } => assert_eq!(rev, "custom-path-rev"),
            other => panic!("Expected FlakeLock, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_nixpkgs_use_missing_lockfile_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        // use = "devenv" but no devenv.lock exists — should fall back to auto-resolve
        let config = UvNixConfig {
            use_source: Some(UseSource::Devenv),
            ..Default::default()
        };

        let source = resolve_nixpkgs(dir.path(), &config);
        match source {
            NixpkgsSource::AutoResolved { .. } => {} // expected
            other => panic!("Expected AutoResolved fallback, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_flake_lock_follows_style() {
        // Test the "follows" style where nixpkgs input is referenced via an array path
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": {
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "follows-rev-123",
        "type": "github"
      }
    },
    "devenv": {
      "inputs": {
        "nixpkgs": ["nixpkgs"]
      }
    },
    "root": {
      "inputs": {
        "nixpkgs": "nixpkgs",
        "devenv": "devenv"
      }
    }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();

        let rev = parse_flake_lock(&dir.path().join("flake.lock")).unwrap();
        assert_eq!(rev, "follows-rev-123");
    }

    #[test]
    fn test_parse_flake_lock_not_nixpkgs_repo() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("flake.lock"),
            r#"{
  "nodes": {
    "nixpkgs": {
      "locked": {
        "owner": "someuser",
        "repo": "not-nixpkgs",
        "rev": "abc123",
        "type": "github"
      }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#,
        )
        .unwrap();

        assert!(parse_flake_lock(&dir.path().join("flake.lock")).is_none());
    }

    #[test]
    fn test_write_nixpkgs_pin_creates_section() {
        let dir = tempfile::tempdir().unwrap();
        let pyproject = dir.path().join("pyproject.toml");
        fs::write(
            &pyproject,
            r#"[project]
name = "my-project"
version = "1.0.0"
"#,
        )
        .unwrap();

        write_nixpkgs_pin(&pyproject, "abc123def456").unwrap();

        let result = fs::read_to_string(&pyproject).unwrap();
        assert!(result.contains("[tool.uv-nix]"), "missing section:\n{result}");
        assert!(
            result.contains(r#"nixpkgs = "github:NixOS/nixpkgs/abc123def456""#),
            "missing pin:\n{result}"
        );
        // Original content preserved
        assert!(result.contains("[project]"), "lost [project]:\n{result}");
        assert!(result.contains("my-project"), "lost project name:\n{result}");
    }

    #[test]
    fn test_write_nixpkgs_pin_existing_section() {
        let dir = tempfile::tempdir().unwrap();
        let pyproject = dir.path().join("pyproject.toml");
        fs::write(
            &pyproject,
            r#"[project]
name = "my-project"

# Custom nix config
[tool.uv-nix]
extra-libraries = ["libGL"]
"#,
        )
        .unwrap();

        write_nixpkgs_pin(&pyproject, "abc123def456").unwrap();

        let result = fs::read_to_string(&pyproject).unwrap();
        assert!(
            result.contains(r#"nixpkgs = "github:NixOS/nixpkgs/abc123def456""#),
            "missing pin:\n{result}"
        );
        // Existing config preserved
        assert!(result.contains(r#"extra-libraries = ["libGL"]"#), "lost existing config:\n{result}");
        // Comment preserved
        assert!(result.contains("# Custom nix config"), "lost comment:\n{result}");
    }

    #[test]
    fn test_write_nixpkgs_pin_no_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let pyproject = dir.path().join("pyproject.toml");
        let original = r#"[tool.uv-nix]
nixpkgs = "github:NixOS/nixpkgs/existing-pin"
"#;
        fs::write(&pyproject, original).unwrap();

        write_nixpkgs_pin(&pyproject, "new-rev-should-not-appear").unwrap();

        let result = fs::read_to_string(&pyproject).unwrap();
        assert!(result.contains("existing-pin"), "overwrote existing pin:\n{result}");
        assert!(
            !result.contains("new-rev-should-not-appear"),
            "should not overwrite:\n{result}"
        );
    }

    #[test]
    fn test_nixpkgs_cache_key() {
        assert_eq!(
            nixpkgs_cache_key(&NixpkgsSource::FlakeLock { rev: "abc".to_string() }),
            "flake-lock:abc"
        );
        assert_eq!(
            nixpkgs_cache_key(&NixpkgsSource::FloxLock { rev: "def".to_string() }),
            "flox-lock:def"
        );
        assert_eq!(
            nixpkgs_cache_key(&NixpkgsSource::ExplicitPin {
                flake_ref: "github:NixOS/nixpkgs/nixos-24.11".to_string()
            }),
            "explicit:github:NixOS/nixpkgs/nixos-24.11"
        );
    }
}
