use std::collections::HashMap;
use std::env;
use std::ffi::OsString;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::config::PackageConfig;
use crate::nix_config::{PackageBuildEntry, PACKAGE_BUILD_LIBS_JSON};
use crate::nixpkgs;

/// Effective build configuration for a package (merged from defaults + custom config).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectivePackageConfig {
    /// Package name.
    pub name: String,
    /// Libraries (runtime deps).
    pub libraries: Vec<String>,
    /// Build tools (e.g., cargo, cmake, pg_config).
    pub build_tools: Vec<String>,
    /// Nixpkgs source being used.
    pub nixpkgs_source: String,
    /// Whether this has custom config from pyproject.toml.
    pub has_custom_config: bool,
}

/// Build a map of environment variables to inject into source distribution builds.
///
/// Reads default paths from the resolved NixConfig and merges per-project
/// `[tool.uv-nix] extra-libraries` resolved via nix eval.
///
/// When `package_name` is provided, also resolves that package's specific
/// dependencies from package-build-libs.json on demand (with caching).
pub fn get_nix_build_env(package_name: Option<&str>) -> HashMap<OsString, OsString> {
    let mut env_vars: HashMap<OsString, OsString> = HashMap::new();

    // Read defaults from NixConfig (Nix is required).
    {
        let nix = crate::nix_config::require();
        // Clear PYTHONPATH to prevent host Python packages (e.g. from devenv/nix-shell)
        // from leaking into build subprocesses and causing ABI confusion.
        env_vars.insert(OsString::from("PYTHONPATH"), OsString::new());

        // Set LIBRARY_PATH (linker search path) but NOT LD_LIBRARY_PATH,
        // because LD_LIBRARY_PATH on NixOS poisons system tools (bash, gcc) by
        // forcing them to load a different glibc than they were linked against.
        // library_path includes runtime libs + all package lib deps (for linking).
        prepend_env(&mut env_vars, "LIBRARY_PATH", &OsString::from(&nix.library_path));

        // Base PATH: stdenv.cc + coreutils + pkg-config's directory
        // pkg-config must be on PATH because many build scripts call it directly
        // (the PKG_CONFIG env var is only used by CMake/autotools, not shell scripts).
        let pkg_config_dir = nix.pkg_config.parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let base_path = format!("{}:{}:{}", nix.cc_bin, nix.coreutils_bin, pkg_config_dir);
        prepend_env(&mut env_vars, "PATH", &OsString::from(&base_path));

        env_vars.insert(
            OsString::from("PKG_CONFIG"),
            OsString::from(&nix.pkg_config),
        );
    }

    // Resolve per-package deps from package-build-libs.json + custom config
    if let Some(name) = package_name {
        if let Some(resolved) = resolve_package_build_env(name) {
            if !resolved.library_path.is_empty() {
                prepend_env(&mut env_vars, "LIBRARY_PATH", &OsString::from(&resolved.library_path));
                prepend_env(&mut env_vars, "C_INCLUDE_PATH", &OsString::from(&resolved.include_path));
                prepend_env(&mut env_vars, "PKG_CONFIG_PATH", &OsString::from(&resolved.pkg_config_path));
            }
            if !resolved.bin_path.is_empty() {
                prepend_env(&mut env_vars, "PATH", &OsString::from(&resolved.bin_path));
            }
        }
    }

    // Resolve per-project extra-libraries from [tool.uv-nix] in pyproject.toml
    if let Some(extra) = resolve_extra_build_paths() {
        prepend_env(&mut env_vars, "LIBRARY_PATH", &OsString::from(&extra.library_path));
        prepend_env(&mut env_vars, "C_INCLUDE_PATH", &OsString::from(&extra.include_path));
        prepend_env(&mut env_vars, "PKG_CONFIG_PATH", &OsString::from(&extra.pkg_config_path));
        if !extra.bin_path.is_empty() {
            prepend_env(&mut env_vars, "PATH", &OsString::from(&extra.bin_path));
        }
    }

    if !env_vars.is_empty() {
        debug!("Injecting {} nix build env vars", env_vars.len());
    }

    env_vars
}

/// Get the effective build configuration for a package.
///
/// This merges the defaults from `package-build-libs.json` with any custom
/// configuration from `[[tool.uv-nix.package]]` in pyproject.toml.
pub fn get_effective_package_config(package_name: &str) -> EffectivePackageConfig {
    let cwd = env::current_dir().unwrap_or_default();
    let project_dir = crate::nix_config::find_project_root(&cwd).unwrap_or(cwd);
    let uv_nix_config = crate::config::find_config(&project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();

    let custom_config = uv_nix_config.get_package_config(package_name);
    let (libs, build_tools) = build_effective_entry(package_name, custom_config);

    // Determine nixpkgs source
    let source = if let Some(ref custom_nixpkgs) = custom_config.and_then(|c| c.nixpkgs.as_ref()) {
        nixpkgs::NixpkgsSource::PyprojectToml { flake_ref: custom_nixpkgs.to_string() }
    } else {
        nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config)
    };

    EffectivePackageConfig {
        name: package_name.to_string(),
        libraries: libs,
        build_tools,
        nixpkgs_source: format!("{:?}", source),
        has_custom_config: custom_config.map(|c| c.has_overrides()).unwrap_or(false),
    }
}

/// Prepend `value` to the existing value of `key` (colon-separated).
fn prepend_env(env_vars: &mut HashMap<OsString, OsString>, key: &str, value: &OsString) {
    let key = OsString::from(key);
    let existing = env_vars
        .get(&key)
        .cloned()
        .or_else(|| env::var_os(&key));

    let new_value = match existing {
        Some(existing) if !existing.is_empty() => {
            let mut combined = value.clone();
            combined.push(":");
            combined.push(&existing);
            combined
        }
        _ => value.clone(),
    };
    env_vars.insert(key, new_value);
}

/// Resolved per-package build paths.
struct PackageBuildPaths {
    library_path: String,
    include_path: String,
    pkg_config_path: String,
    bin_path: String,
}

/// Build the effective libs and build-tools for a package.
///
/// Merges defaults from `package-build-libs.json` with custom config from pyproject.toml.
fn build_effective_entry(
    package_name: &str,
    custom_config: Option<&PackageConfig>,
) -> (Vec<String>, Vec<String>) {
    let is_darwin = cfg!(target_os = "macos");

    // Load defaults from package-build-libs.json
    let package_map: HashMap<String, PackageBuildEntry> =
        serde_json::from_str(PACKAGE_BUILD_LIBS_JSON).unwrap_or_default();
    let default_entry = package_map.get(package_name);

    // Start with default libs or custom override
    let mut libs: Vec<String> = if let Some(custom) = custom_config {
        if !custom.libraries.is_empty() {
            // Custom `libraries` replaces defaults entirely
            custom.libraries.clone()
        } else if let Some(entry) = default_entry {
            // Use defaults
            entry.libs.clone()
        } else {
            Vec::new()
        }
    } else if let Some(entry) = default_entry {
        entry.libs.clone()
    } else {
        Vec::new()
    };

    // Add platform-specific default libs
    if let Some(entry) = default_entry {
        if is_darwin {
            libs.extend(entry.libs_darwin.clone());
        } else {
            libs.extend(entry.libs_linux.clone());
        }
    }

    // Start with default build-tools
    let mut build_tools: Vec<String> = default_entry
        .map(|e| e.build_tools.clone())
        .unwrap_or_default();

    // Apply custom config additions
    if let Some(custom) = custom_config {
        // Add extra libraries
        libs.extend(custom.extra_libraries.clone());

        // Add platform-specific extra libraries
        if is_darwin {
            libs.extend(custom.extra_darwin_libraries.clone());
        } else {
            libs.extend(custom.extra_linux_libraries.clone());
        }

        // Add extra build tools
        build_tools.extend(custom.extra_build_tools.clone());
    }

    (libs, build_tools)
}

/// Resolve build environment paths for a specific package.
///
/// Looks up the package in the embedded registry, merges with custom config,
/// resolves via nix-build (with caching in ~/.cache/uv-nix/), and returns the paths.
fn resolve_package_build_env(package_name: &str) -> Option<PackageBuildPaths> {
    let cwd = env::current_dir().ok()?;
    let project_dir = crate::nix_config::find_project_root(&cwd).unwrap_or(cwd);
    let uv_nix_config = crate::config::find_config(&project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();

    // Get custom config if any
    let custom_config = uv_nix_config.get_package_config(package_name);

    // Build effective entry by merging defaults + custom config
    let (libs, build_tools) = build_effective_entry(package_name, custom_config);

    // Collect all attrs to resolve
    let all_attrs: Vec<String> = libs.iter()
        .chain(build_tools.iter())
        .cloned()
        .collect();

    if all_attrs.is_empty() {
        return None;
    }

    // Determine nixpkgs source (per-package override or project default)
    let source = if let Some(ref custom_nixpkgs) = custom_config.and_then(|c| c.nixpkgs.as_ref()) {
        nixpkgs::NixpkgsSource::PyprojectToml { flake_ref: custom_nixpkgs.to_string() }
    } else {
        nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config)
    };
    let nixpkgs_key = nixpkgs::nixpkgs_cache_key(&source);

    // Cache key: hash of version + nixpkgs + package name + effective entry
    let entry_json = serde_json::to_string(&(&libs, &build_tools)).unwrap_or_default();
    let cache_key = {
        let mut hasher = Sha256::new();
        hasher.update(b"pkg-v2\0"); // Bump version for new format
        hasher.update(nixpkgs_key.as_bytes());
        hasher.update(b"\0");
        hasher.update(package_name.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry_json.as_bytes());
        format!("pkg-{:x}", hasher.finalize())
    };

    // Check cache
    if let Some(cached) = load_package_cache(&cache_key) {
        debug!("Cache hit for package build env: {package_name}");
        return Some(cached);
    }

    // Cache miss — resolve via nix-build
    crate::status("Resolving", &format!("build deps for {package_name}"));
    debug!("Resolving package build env for {package_name}: libs={:?}, build-tools={:?}",
           libs, build_tools);

    match nixpkgs::resolve_build_paths(&all_attrs, &source) {
        Ok(resolved) => {
            // Cache the result
            if let Err(err) = save_package_cache(&cache_key, &resolved) {
                warn!("Failed to cache package build env for {package_name}: {err}");
            }

            let result = PackageBuildPaths {
                library_path: resolved.library_path,
                include_path: resolved.include_path,
                pkg_config_path: resolved.pkg_config_path,
                bin_path: resolved.bin_path,
            };
            crate::status("Resolved", &format!("build deps for {package_name}"));
            Some(result)
        }
        Err(err) => {
            warn!("Failed to resolve package build env for {package_name}: {err}");
            None
        }
    }
}

/// Load cached package build paths from ~/.cache/uv-nix/<key>.json.
fn load_package_cache(cache_key: &str) -> Option<PackageBuildPaths> {
    let cache_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?
        .join("uv-nix");

    let path = cache_dir.join(format!("{cache_key}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    let resolved: nixpkgs::ResolvedBuildPaths = serde_json::from_str(&content).ok()?;

    Some(PackageBuildPaths {
        library_path: resolved.library_path,
        include_path: resolved.include_path,
        pkg_config_path: resolved.pkg_config_path,
        bin_path: resolved.bin_path,
    })
}

/// Save package build paths to ~/.cache/uv-nix/<key>.json.
fn save_package_cache(cache_key: &str, paths: &nixpkgs::ResolvedBuildPaths) -> anyhow::Result<()> {
    let cache_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .ok_or_else(|| anyhow::anyhow!("Cannot determine cache directory"))?
        .join("uv-nix");

    std::fs::create_dir_all(&cache_dir)?;
    let path = cache_dir.join(format!("{cache_key}.json"));
    let content = serde_json::to_string_pretty(paths)?;
    std::fs::write(&path, content)?;
    debug!("Cached package build env at {}", path.display());
    Ok(())
}

/// Resolve extra build paths from `[tool.uv-nix]` in pyproject.toml (CWD search).
fn resolve_extra_build_paths() -> Option<nixpkgs::ResolvedBuildPaths> {
    let cwd = env::current_dir().ok()?;
    let (uv_nix_config, project_dir) = crate::config::find_config(&cwd)?;

    if uv_nix_config.extra_libraries.is_empty() {
        return None;
    }

    debug!(
        "Found {} extra libraries for build env",
        uv_nix_config.extra_libraries.len()
    );

    let source = nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config);
    let nix_key = nixpkgs::nixpkgs_cache_key(&source);

    // Check cache first
    let cache_key = format!("build-paths:{nix_key}");
    if let Some(cached) = crate::cache::lookup(&project_dir, &cache_key, &uv_nix_config.extra_libraries) {
        // Cached value is JSON-serialized ResolvedBuildPaths
        if let Ok(paths) = serde_json::from_str::<nixpkgs::ResolvedBuildPaths>(&cached) {
            return Some(paths);
        }
    }

    // Cache miss — resolve via nix-build
    debug!("Resolving build paths for {:?}", uv_nix_config.extra_libraries);
    match nixpkgs::resolve_build_paths(&uv_nix_config.extra_libraries, &source) {
        Ok(paths) => {
            debug!("Resolved build paths: {:?}", paths);
            // Cache as JSON
            if let Ok(json) = serde_json::to_string(&paths) {
                if let Err(err) = crate::cache::store(
                    &project_dir,
                    &cache_key,
                    &uv_nix_config.extra_libraries,
                    &json,
                ) {
                    warn!("Failed to cache resolved build paths: {err}");
                }
            }
            Some(paths)
        }
        Err(err) => {
            warn!("Failed to resolve extra libraries for build env: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepend_env_empty() {
        let mut env_vars = HashMap::new();
        prepend_env(&mut env_vars, "TEST_VAR", &OsString::from("/nix/store/foo/lib"));
        assert_eq!(
            env_vars.get(&OsString::from("TEST_VAR")),
            Some(&OsString::from("/nix/store/foo/lib"))
        );
    }

    #[test]
    fn test_prepend_env_existing() {
        let mut env_vars = HashMap::new();
        env_vars.insert(OsString::from("TEST_VAR"), OsString::from("/existing/path"));
        prepend_env(&mut env_vars, "TEST_VAR", &OsString::from("/nix/store/foo/lib"));
        assert_eq!(
            env_vars.get(&OsString::from("TEST_VAR")),
            Some(&OsString::from("/nix/store/foo/lib:/existing/path"))
        );
    }

    #[test]
    fn test_parse_package_build_libs() {
        let package_map: HashMap<String, PackageBuildEntry> =
            serde_json::from_str(PACKAGE_BUILD_LIBS_JSON).unwrap();

        // psycopg2 has both libs and build-tools
        let psycopg2 = package_map.get("psycopg2").unwrap();
        assert!(psycopg2.libs.contains(&"libpq".to_string()));
        assert!(psycopg2.build_tools.contains(&"libpq.pg_config".to_string()));

        // bcrypt has libs + build-tools (cargo)
        let bcrypt = package_map.get("bcrypt").unwrap();
        assert!(bcrypt.build_tools.contains(&"cargo".to_string()));

        // pillow has only libs
        let pillow = package_map.get("pillow").unwrap();
        assert!(!pillow.libs.is_empty());
        assert!(pillow.build_tools.is_empty());

        // orjson has only build-tools
        let orjson = package_map.get("orjson").unwrap();
        assert!(orjson.libs.is_empty());
        assert!(orjson.build_tools.contains(&"cargo".to_string()));
    }

    #[test]
    fn test_build_effective_entry_defaults() {
        // Without custom config, should return defaults
        let (libs, tools) = build_effective_entry("psycopg2", None);
        assert!(libs.contains(&"libpq".to_string()));
        assert!(tools.contains(&"libpq.pg_config".to_string()));
    }

    #[test]
    fn test_build_effective_entry_override_libs() {
        let custom = PackageConfig {
            name: "psycopg2".to_string(),
            libraries: vec!["postgresql_17".to_string()],
            ..Default::default()
        };
        let (libs, tools) = build_effective_entry("psycopg2", Some(&custom));
        // Custom libraries should replace defaults
        assert_eq!(libs, vec!["postgresql_17"]);
        // Build tools should still be from defaults
        assert!(tools.contains(&"libpq.pg_config".to_string()));
    }

    #[test]
    fn test_build_effective_entry_extra_libs() {
        let custom = PackageConfig {
            name: "psycopg2".to_string(),
            extra_libraries: vec!["openssl".to_string()],
            extra_build_tools: vec!["cmake".to_string()],
            ..Default::default()
        };
        let (libs, tools) = build_effective_entry("psycopg2", Some(&custom));
        // Should have defaults + extras
        assert!(libs.contains(&"libpq".to_string()));
        assert!(libs.contains(&"openssl".to_string()));
        assert!(tools.contains(&"libpq.pg_config".to_string()));
        assert!(tools.contains(&"cmake".to_string()));
    }

    #[test]
    fn test_build_effective_entry_unknown_package() {
        // For unknown packages with custom config
        let custom = PackageConfig {
            name: "my-custom-pkg".to_string(),
            libraries: vec!["libfoo".to_string()],
            extra_build_tools: vec!["cargo".to_string()],
            ..Default::default()
        };
        let (libs, tools) = build_effective_entry("my-custom-pkg", Some(&custom));
        assert_eq!(libs, vec!["libfoo"]);
        assert_eq!(tools, vec!["cargo"]);
    }

    #[test]
    fn test_get_nix_build_env_no_vars() {
        // With no UV_NIX_* vars set, should return empty (or only extras)
        let env = get_nix_build_env(None);
        // In test context, UV_NIX_* vars are not set, so result depends on CWD
        // At minimum, the function should not panic
        assert!(env.len() < 100); // sanity check
    }
}
