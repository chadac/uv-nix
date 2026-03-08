use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::nixpkgs;

/// Embedded default runtime library config (platform-specific).
const DEFAULT_LIBS_JSON: &str = include_str!("../data/default-libs.json");

/// Embedded per-Python-package build dependency registry.
pub(crate) const PACKAGE_BUILD_LIBS_JSON: &str = include_str!("../data/package-build-libs.json");

/// Platform-specific library configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DefaultLibsConfig {
    /// Libraries available on all platforms
    #[serde(default)]
    shared: Vec<String>,
    /// Linux-only libraries (glibc, util-linux, etc.)
    #[serde(default)]
    linux: Vec<String>,
    /// Darwin-only libraries (libiconv, etc.)
    #[serde(default)]
    darwin: Vec<String>,
}

/// Per-package build dependency entry from package-build-libs.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PackageBuildEntry {
    #[serde(default)]
    pub libs: Vec<String>,
    #[serde(default, rename = "build-tools")]
    pub build_tools: Vec<String>,
    /// Linux-only libs for this package
    #[serde(default, rename = "libs-linux")]
    pub libs_linux: Vec<String>,
    /// Darwin-only libs for this package
    #[serde(default, rename = "libs-darwin")]
    pub libs_darwin: Vec<String>,
}

/// All Nix paths needed at runtime, resolved from a single `nix-build` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NixConfig {
    /// Path to patchelf (Linux) or install_name_tool (Darwin)
    pub patcher: PathBuf,
    /// ELF interpreter path (Linux only, empty on Darwin)
    #[serde(default)]
    pub interpreter: PathBuf,
    /// Colon-separated RPATH entries (runtime libs only, for patchelf).
    pub rpath: String,
    /// Colon-separated library path (runtime + all package lib deps, for LIBRARY_PATH / RPATH patching).
    pub library_path: String,
    /// Path to stdenv.cc/bin (for PATH in build env).
    pub cc_bin: String,
    /// Path to coreutils/bin (for PATH in build env).
    pub coreutils_bin: String,
    pub pkg_config: PathBuf,
    /// True if running on Darwin/macOS
    #[serde(default)]
    pub is_darwin: bool,
}

// Legacy alias for compatibility
impl NixConfig {
    pub fn patchelf(&self) -> &PathBuf {
        &self.patcher
    }
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

    // Cache miss — resolve via nix build
    crate::status("Resolving", "nix configuration (first run)");
    let config = build_nix_config(&source)
        .map_err(|e| format!("Failed to resolve Nix configuration:\n\n{e}"))?;

    if let Err(err) = save_cache(&cache_key, &config) {
        warn!("Failed to cache NixConfig: {err}");
    }
    crate::status("Resolved", "nix configuration");
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
    hasher.update(b"nix-config-v3\0");
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
    if !config.patcher.exists() {
        debug!("Cached NixConfig patcher path no longer exists, invalidating");
        let _ = std::fs::remove_file(&path);
        return None;
    }
    // On Linux, also check interpreter exists
    if !config.is_darwin && !config.interpreter.as_os_str().is_empty() && !config.interpreter.exists() {
        debug!("Cached NixConfig interpreter path no longer exists, invalidating");
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

/// Check if we're running on Darwin/macOS.
fn is_darwin() -> bool {
    cfg!(target_os = "macos")
}

/// Collect all unique lib attrs from both default-libs.json and package-build-libs.json.
///
/// Returns (runtime_attrs, all_lib_attrs). build-tools are excluded — they are
/// resolved per-package on demand in build_env.rs.
/// 
/// Library selection is platform-aware: shared libs are always included,
/// plus linux-specific or darwin-specific libs based on the current platform.
fn collect_all_lib_attrs() -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let darwin = is_darwin();
    
    // Runtime libs (platform-specific structure)
    let libs_config: DefaultLibsConfig = serde_json::from_str(DEFAULT_LIBS_JSON)?;
    
    // Combine shared + platform-specific runtime libs
    let mut runtime_attrs: Vec<String> = libs_config.shared.clone();
    if darwin {
        runtime_attrs.extend(libs_config.darwin.clone());
    } else {
        runtime_attrs.extend(libs_config.linux.clone());
    }

    // Package build deps (map of package name → { libs, build-tools, libs-linux, libs-darwin })
    let package_map: std::collections::HashMap<String, PackageBuildEntry> =
        serde_json::from_str(PACKAGE_BUILD_LIBS_JSON)?;

    // Union all lib attrs (runtime + all package libs), deduplicated.
    // build-tools are excluded — they're resolved per-package on demand.
    let mut all_lib_set: BTreeSet<String> = BTreeSet::new();
    for attr in &runtime_attrs {
        all_lib_set.insert(attr.clone());
    }
    for entry in package_map.values() {
        // Shared libs for this package
        for attr in &entry.libs {
            all_lib_set.insert(attr.clone());
        }
        // Platform-specific libs for this package
        if darwin {
            for attr in &entry.libs_darwin {
                all_lib_set.insert(attr.clone());
            }
        } else {
            for attr in &entry.libs_linux {
                all_lib_set.insert(attr.clone());
            }
        }
    }

    let all_lib_attrs: Vec<String> = all_lib_set.into_iter().collect();

    Ok((runtime_attrs, all_lib_attrs))
}

/// Run a single `nix-build -E` to resolve all config paths at once.
fn build_nix_config(source: &nixpkgs::NixpkgsSource) -> anyhow::Result<NixConfig> {
    let pkgs_expr = nixpkgs::nixpkgs_import_expr(source);
    let darwin = is_darwin();

    let (runtime_attrs, all_lib_attrs) = collect_all_lib_attrs()?;

    let runtime_exprs: String = runtime_attrs
        .iter()
        .map(|a| format!("    {}", attr_resolve_expr(a)))
        .collect::<Vec<_>>()
        .join("\n");

    let lib_exprs: String = all_lib_attrs
        .iter()
        .map(|a| format!("    {}", attr_resolve_expr(a)))
        .collect::<Vec<_>>()
        .join("\n");

    // Platform-specific Nix expression
    let expr = if darwin {
        format!(
            r#"let
  pkgs = {pkgs_expr};
  runtimeLibs = [
{runtime_exprs}
  ];
  allLibs = [
{lib_exprs}
  ];
in pkgs.writeText "uv-nix-config.json" (builtins.toJSON {{
  patcher = "${{pkgs.darwin.cctools}}/bin/install_name_tool";
  interpreter = "";
  rpath = pkgs.lib.makeLibraryPath runtimeLibs;
  library_path = pkgs.lib.makeLibraryPath allLibs;
  cc_bin = "${{pkgs.stdenv.cc}}/bin";
  coreutils_bin = "${{pkgs.coreutils}}/bin";
  pkg_config = "${{pkgs.pkg-config}}/bin/pkg-config";
  is_darwin = true;
}})"#
        )
    } else {
        format!(
            r#"let
  pkgs = {pkgs_expr};
  runtimeLibs = [
{runtime_exprs}
  ];
  allLibs = [
{lib_exprs}
  ];
in pkgs.writeText "uv-nix-config.json" (builtins.toJSON {{
  patcher = "${{pkgs.patchelf}}/bin/patchelf";
  interpreter = pkgs.lib.strings.trim pkgs.stdenv.cc.bintools.dynamicLinker;
  rpath = pkgs.lib.makeLibraryPath runtimeLibs;
  library_path = pkgs.lib.makeLibraryPath allLibs;
  cc_bin = "${{pkgs.stdenv.cc}}/bin";
  coreutils_bin = "${{pkgs.coreutils}}/bin";
  pkg_config = "${{pkgs.pkg-config}}/bin/pkg-config";
  is_darwin = false;
}})"#
        )
    };

    debug!("Building NixConfig via nix build (darwin={})", darwin);

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
            patcher: PathBuf::from("/nix/store/xxx/bin/patchelf"),
            interpreter: PathBuf::from("/nix/store/yyy/lib/ld-linux-x86-64.so.2"),
            rpath: "/nix/store/aaa/lib:/nix/store/bbb/lib".to_string(),
            library_path: "/nix/store/aaa/lib:/nix/store/bbb/lib:/nix/store/ccc/lib".to_string(),
            cc_bin: "/nix/store/ccc/bin".to_string(),
            coreutils_bin: "/nix/store/ddd/bin".to_string(),
            pkg_config: PathBuf::from("/nix/store/zzz/bin/pkg-config"),
            is_darwin: false,
        };

        let json_path = cache_path.join("test-key.json");
        let content = serde_json::to_string_pretty(&config).unwrap();
        std::fs::write(&json_path, content).unwrap();
        assert!(json_path.exists());

        let loaded: NixConfig =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(loaded.patcher, config.patcher);
        assert_eq!(loaded.rpath, config.rpath);
        assert_eq!(loaded.library_path, config.library_path);
        assert_eq!(loaded.cc_bin, config.cc_bin);
        assert_eq!(loaded.coreutils_bin, config.coreutils_bin);
        assert_eq!(loaded.is_darwin, config.is_darwin);
    }

    #[test]
    fn test_collect_all_lib_attrs() {
        let (runtime, all_libs) = collect_all_lib_attrs().unwrap();
        // Runtime should have shared libs (openssl is in shared)
        assert!(runtime.contains(&"openssl".to_string()));
        // On Linux, should have glibc; on Darwin, should have libiconv
        #[cfg(target_os = "linux")]
        assert!(runtime.contains(&"glibc".to_string()));
        #[cfg(target_os = "macos")]
        assert!(runtime.contains(&"libiconv".to_string()));
        // All libs should include runtime + package lib deps
        assert!(all_libs.len() >= runtime.len());
        // Package lib entries should be present
        assert!(all_libs.contains(&"libpq".to_string()));
        // Build-tool-only entries (cargo) should NOT be in all_libs
        assert!(!all_libs.contains(&"cargo".to_string()));
    }
}
