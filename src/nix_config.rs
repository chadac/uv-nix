use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::nixpkgs;

/// Embedded default runtime library list (for RPATH patching).
const DEFAULT_LIBS_JSON: &str = include_str!("../data/default-libs.json");

/// Embedded per-Python-package build dependency registry.
const PACKAGE_BUILD_LIBS_JSON: &str = include_str!("../data/package-build-libs.json");

/// All Nix paths needed at runtime, resolved from a single `nix-build` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NixConfig {
    pub patchelf: PathBuf,
    pub interpreter: PathBuf,
    /// Colon-separated RPATH entries (runtime libs only, for patchelf).
    pub rpath: String,
    /// Colon-separated library path (runtime + all package build deps, for LIBRARY_PATH).
    pub library_path: String,
    /// Colon-separated include paths (runtime + all package build deps, for C_INCLUDE_PATH).
    pub include_path: String,
    /// Colon-separated pkg-config search paths (runtime + all package build deps).
    pub pkg_config_path: String,
    /// Colon-separated bin paths from package build deps (for PATH).
    pub bin_path: String,
    pub pkg_config: PathBuf,
}

static NIX_CONFIG: OnceLock<Result<NixConfig, String>> = OnceLock::new();

/// Get the lazily-resolved NixConfig, or None if resolution failed.
pub fn get() -> Option<&'static NixConfig> {
    NIX_CONFIG.get_or_init(|| resolve_config()).as_ref().ok()
}

/// Get the NixConfig or exit with the actual error.
///
/// This is uv-nix: Nix is required. Every hook calls this so the user
/// gets a clear error explaining what went wrong.
pub fn require() -> &'static NixConfig {
    match NIX_CONFIG.get_or_init(|| resolve_config()) {
        Ok(config) => config,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    }
}

/// Resolve the NixConfig by finding nixpkgs and running a single nix-build.
fn resolve_config() -> Result<NixConfig, String> {
    if !nix_available() {
        return Err(
            "uv-nix requires Nix.\n\n\
             `nix` was not found on PATH. Install Nix:\n\n\
                 curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh\n\n\
             Then ensure `nix` is on your PATH and try again."
                .to_string(),
        );
    }

    let cwd = std::env::current_dir()
        .map_err(|e| format!("Failed to get current directory: {e}"))?;
    let project_dir = find_project_root(&cwd).unwrap_or(cwd);

    let uv_nix_config = crate::config::find_config(&project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();

    let source = nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config);
    let nixpkgs_key = nixpkgs::nixpkgs_cache_key(&source);

    // Check on-disk cache
    let cache_key = compute_cache_key(&nixpkgs_key);
    if let Some(cached) = load_cache(&cache_key) {
        debug!("Using cached NixConfig from ~/.cache/uv-nix/");
        return Ok(cached);
    }

    // Cache miss — resolve via nix-build
    debug!("Resolving NixConfig via nix-build (first run or cache miss)");
    let config = build_nix_config(&source)
        .map_err(|e| format!("Failed to resolve Nix configuration:\n\n{e}"))?;

    if let Err(err) = save_cache(&cache_key, &config) {
        warn!("Failed to cache NixConfig: {err}");
    }
    Ok(config)
}

/// Check if `nix` is available on PATH.
fn nix_available() -> bool {
    crate::nix_command()
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Search upward from `start` for a project root directory.
///
/// A project root is identified by the presence of flake.lock, devenv.lock,
/// or pyproject.toml.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };

    loop {
        for marker in &["flake.lock", "devenv.lock", "pyproject.toml"] {
            if dir.join(marker).is_file() {
                return Some(dir);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Compute a SHA-256 cache key from the nixpkgs key and both lib configs.
fn compute_cache_key(nixpkgs_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nix-config-v2\0");
    hasher.update(nixpkgs_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(DEFAULT_LIBS_JSON.as_bytes());
    hasher.update(b"\0");
    hasher.update(PACKAGE_BUILD_LIBS_JSON.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Cache directory: `~/.cache/uv-nix/`.
fn cache_dir() -> Option<PathBuf> {
    dirs_or_home().map(|d| d.join("uv-nix"))
}

/// Get the XDG cache home or fall back to `~/.cache`.
fn dirs_or_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
        })
}

/// Load a cached NixConfig from `~/.cache/uv-nix/<hash>.json`.
fn load_cache(key: &str) -> Option<NixConfig> {
    let path = cache_dir()?.join(format!("{key}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    let config: NixConfig = serde_json::from_str(&content).ok()?;

    // Validate that key paths still exist
    if !config.patchelf.exists() || !config.interpreter.exists() {
        debug!("Cached NixConfig paths no longer exist, invalidating");
        let _ = std::fs::remove_file(&path);
        return None;
    }

    Some(config)
}

/// Save a NixConfig to `~/.cache/uv-nix/<hash>.json`.
fn save_cache(key: &str, config: &NixConfig) -> anyhow::Result<()> {
    let dir = cache_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine cache directory"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{key}.json"));
    let content = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    debug!("Cached NixConfig at {}", path.display());
    Ok(())
}

/// Build a nix attr resolution expression for a list of attr strings.
fn attr_resolve_expr(attr: &str) -> String {
    format!(
        "(pkgs.lib.getAttrFromPath (pkgs.lib.splitString \".\" \"{attr}\") pkgs)"
    )
}

/// Collect all unique attrs from both default-libs.json and package-build-libs.json.
fn collect_all_build_attrs() -> anyhow::Result<(Vec<String>, Vec<String>)> {
    // Runtime libs (plain list of attr strings)
    let runtime_attrs: Vec<String> = serde_json::from_str(DEFAULT_LIBS_JSON)?;

    // Package build deps (map of package name → list of attr strings)
    let package_map: std::collections::HashMap<String, Vec<String>> =
        serde_json::from_str(PACKAGE_BUILD_LIBS_JSON)?;

    // Union all build attrs (runtime + all package deps), deduplicated
    let mut all_build_set: BTreeSet<String> = BTreeSet::new();
    for attr in &runtime_attrs {
        all_build_set.insert(attr.clone());
    }
    for attrs in package_map.values() {
        for attr in attrs {
            all_build_set.insert(attr.clone());
        }
    }

    let all_build_attrs: Vec<String> = all_build_set.into_iter().collect();

    Ok((runtime_attrs, all_build_attrs))
}

/// Run a single `nix-build -E` to resolve all config paths at once.
fn build_nix_config(source: &nixpkgs::NixpkgsSource) -> anyhow::Result<NixConfig> {
    let pkgs_expr = nixpkgs::nixpkgs_import_expr(source);

    let (runtime_attrs, all_build_attrs) = collect_all_build_attrs()?;

    let runtime_exprs: String = runtime_attrs
        .iter()
        .map(|a| format!("    {}", attr_resolve_expr(a)))
        .collect::<Vec<_>>()
        .join("\n");

    let build_exprs: String = all_build_attrs
        .iter()
        .map(|a| format!("    {}", attr_resolve_expr(a)))
        .collect::<Vec<_>>()
        .join("\n");

    let expr = format!(
        r#"let
  pkgs = {pkgs_expr};
  runtimeLibs = [
{runtime_exprs}
  ];
  allBuildLibs = [
{build_exprs}
  ];
in pkgs.writeText "uv-nix-config.json" (builtins.toJSON {{
  patchelf = "${{pkgs.patchelf}}/bin/patchelf";
  interpreter = pkgs.lib.strings.trim pkgs.stdenv.cc.bintools.dynamicLinker;
  rpath = pkgs.lib.makeLibraryPath runtimeLibs;
  library_path = pkgs.lib.makeLibraryPath allBuildLibs;
  include_path = builtins.concatStringsSep ":" (map (p: "${{pkgs.lib.getDev p}}/include") allBuildLibs);
  pkg_config_path = builtins.concatStringsSep ":" (map (p: "${{pkgs.lib.getDev p}}/lib/pkgconfig") allBuildLibs);
  bin_path = builtins.concatStringsSep ":" (map (p: "${{pkgs.lib.getBin p}}/bin") allBuildLibs);
  pkg_config = "${{pkgs.pkg-config}}/bin/pkg-config";
}})"#
    );

    debug!("Building NixConfig via nix build");

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
    let config: NixConfig = serde_json::from_str(json_str.trim())?;

    debug!("Resolved NixConfig: {:?}", config);
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_cache_key_deterministic() {
        let key1 = compute_cache_key("flake-lock:abc123");
        let key2 = compute_cache_key("flake-lock:abc123");
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_compute_cache_key_differs() {
        let key1 = compute_cache_key("flake-lock:abc123");
        let key2 = compute_cache_key("flake-lock:def456");
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_find_project_root_with_flake_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("flake.lock"), "{}").unwrap();
        let sub = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&sub).unwrap();

        let root = find_project_root(&sub).unwrap();
        assert_eq!(root, dir.path());
    }

    #[test]
    fn test_find_project_root_with_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname = \"test\"").unwrap();

        let root = find_project_root(dir.path()).unwrap();
        assert_eq!(root, dir.path());
    }

    #[test]
    fn test_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("uv-nix");
        std::fs::create_dir_all(&cache_path).unwrap();

        let config = NixConfig {
            patchelf: PathBuf::from("/nix/store/xxx/bin/patchelf"),
            interpreter: PathBuf::from("/nix/store/yyy/lib/ld-linux-x86-64.so.2"),
            rpath: "/nix/store/aaa/lib:/nix/store/bbb/lib".to_string(),
            library_path: "/nix/store/aaa/lib:/nix/store/bbb/lib:/nix/store/ccc/lib".to_string(),
            include_path: "/nix/store/aaa/include".to_string(),
            pkg_config_path: "/nix/store/aaa/lib/pkgconfig".to_string(),
            bin_path: "/nix/store/ccc/bin".to_string(),
            pkg_config: PathBuf::from("/nix/store/zzz/bin/pkg-config"),
        };

        let json_path = cache_path.join("test-key.json");
        let content = serde_json::to_string_pretty(&config).unwrap();
        std::fs::write(&json_path, content).unwrap();
        assert!(json_path.exists());

        let loaded: NixConfig =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(loaded.patchelf, config.patchelf);
        assert_eq!(loaded.rpath, config.rpath);
        assert_eq!(loaded.library_path, config.library_path);
        assert_eq!(loaded.bin_path, config.bin_path);
    }

    #[test]
    fn test_collect_all_build_attrs() {
        let (runtime, all_build) = collect_all_build_attrs().unwrap();
        // Runtime should have default-libs entries
        assert!(runtime.contains(&"glibc".to_string()));
        assert!(runtime.contains(&"openssl".to_string()));
        // All build should include runtime + package deps
        assert!(all_build.len() >= runtime.len());
        // Package-build-libs entries should be present
        assert!(all_build.contains(&"libpq".to_string()));
    }
}
