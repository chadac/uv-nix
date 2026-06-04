use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::config::PackageConfig;
use crate::nix_config::{PACKAGE_BUILD_LIBS_JSON, PackageBuildEntry};
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
///
/// `source_dir` is the unpacked sdist directory — used to detect Rust MSRV
/// from Cargo.toml when the package requires cargo.
pub fn get_nix_build_env(
    package_name: Option<&str>,
    source_dir: Option<&Path>,
) -> anyhow::Result<HashMap<OsString, OsString>> {
    let Some(name) = package_name else {
        return Ok(get_base_build_env());
    };

    let dev_env = resolve_package_dev_env(name, source_dir)?;
    let mut env_vars: HashMap<OsString, OsString> = HashMap::new();
    for (key, value) in &dev_env.vars {
        env_vars.insert(OsString::from(key), OsString::from(value));
    }
    env_vars.insert(OsString::from("PYTHONPATH"), OsString::new());

    // Nix's pkg-config wrapper uses a platform-specific var (e.g.,
    // PKG_CONFIG_PATH_x86_64_unknown_linux_gnu) and ignores PKG_CONFIG_PATH
    // when it's set. Mirror our PKG_CONFIG_PATH into the platform-specific key.
    if let Some(pkg_path) = env_vars.get(&OsString::from("PKG_CONFIG_PATH")).cloned() {
        if let Some(key) = pkg_config_platform_key() {
            env_vars.insert(OsString::from(key), pkg_path);
        }
    }

    // Some packages (h5py) use dlopen() at build time to find libraries.
    // LIBRARY_PATH only helps the linker; dlopen needs DYLD_FALLBACK_LIBRARY_PATH
    // (macOS) or LD_LIBRARY_PATH (Linux).
    if let Some(lib_path) = env_vars.get(&OsString::from("LIBRARY_PATH")).cloned() {
        if cfg!(target_os = "macos") {
            env_vars.insert(OsString::from("DYLD_FALLBACK_LIBRARY_PATH"), lib_path);
        } else {
            env_vars
                .entry(OsString::from("LD_LIBRARY_PATH"))
                .or_insert(lib_path);
        }
    }

    debug!("Injecting {} nix build env vars from print-dev-env", env_vars.len());
    Ok(env_vars)
}

/// Fallback: build a minimal environment from the resolved NixConfig.
///
/// Used when no package name is provided or when nix print-dev-env fails.
fn get_base_build_env() -> HashMap<OsString, OsString> {
    let mut env_vars: HashMap<OsString, OsString> = HashMap::new();
    let nix = crate::nix_config::require();

    env_vars.insert(OsString::from("PYTHONPATH"), OsString::new());

    prepend_env(
        &mut env_vars,
        "LIBRARY_PATH",
        &OsString::from(&nix.library_path),
    );

    let pkg_config_dir = nix
        .pkg_config
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let base_path = format!("{}:{}:{}", nix.cc_bin, nix.coreutils_bin, pkg_config_dir);
    prepend_env(&mut env_vars, "PATH", &OsString::from(&base_path));

    env_vars.insert(
        OsString::from("PKG_CONFIG"),
        OsString::from(&nix.pkg_config),
    );

    debug!("Injecting {} base nix build env vars", env_vars.len());
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
        nixpkgs::NixpkgsSource::ExplicitPin {
            flake_ref: custom_nixpkgs.to_string(),
        }
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
    let existing = env_vars.get(&key).cloned().or_else(|| env::var_os(&key));

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

/// Return the platform-specific `PKG_CONFIG_PATH_<triple>` key that nix's
/// pkg-config wrapper expects (e.g., `PKG_CONFIG_PATH_arm64_apple_darwin`).
fn pkg_config_platform_key() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { Some("PKG_CONFIG_PATH_arm64_apple_darwin") }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { Some("PKG_CONFIG_PATH_x86_64_apple_darwin") }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { Some("PKG_CONFIG_PATH_x86_64_unknown_linux_gnu") }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { Some("PKG_CONFIG_PATH_aarch64_unknown_linux_gnu") }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    { None }
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
        // Add extra libraries (filtered by platform)
        libs.extend(custom.extra_libraries_for_system(crate::current_system()));

        // Add extra build tools
        build_tools.extend(custom.extra_build_tools.clone());
    }

    (libs, build_tools)
}

/// Resolve a full build environment for a package via `nix print-dev-env`.
///
/// Uses `buildPythonPackage` with `inputsFrom` to get the complete set of
/// environment variables that Nix provides — CC wrapper, PKG_CONFIG_PATH,
/// NIX_LDFLAGS, NIX_CFLAGS_COMPILE, etc.
///
/// When `source_dir` is provided and the package requires cargo, detects MSRV
/// from Cargo.toml and resolves a newer Rust toolchain via rust-overlay if needed.
fn resolve_package_dev_env(
    package_name: &str,
    source_dir: Option<&Path>,
) -> anyhow::Result<nixpkgs::ResolvedBuildEnv> {
    let cwd = env::current_dir()?;
    let project_dir = crate::nix_config::find_project_root(&cwd).unwrap_or(cwd);
    let uv_nix_config = crate::config::find_config(&project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();

    let custom_config = uv_nix_config.get_package_config(package_name);
    let (libs, build_tools) = build_effective_entry(package_name, custom_config);

    let source = if let Some(ref custom_nixpkgs) = custom_config.and_then(|c| c.nixpkgs.as_ref()) {
        nixpkgs::NixpkgsSource::ExplicitPin {
            flake_ref: custom_nixpkgs.to_string(),
        }
    } else {
        nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config)
    };
    let nixpkgs_key = nixpkgs::nixpkgs_cache_key(&source);

    // Check if this package needs a newer Rust toolchain
    let rust_toolchain = if build_tools.contains(&"cargo".to_string()) {
        resolve_rust_if_needed(source_dir, &source, &project_dir)?
    } else {
        None
    };

    let entry_json = serde_json::to_string(&(&libs, &build_tools)).unwrap_or_default();
    let rust_key = rust_toolchain
        .as_ref()
        .map(|t| t.bin_path.to_string_lossy().to_string())
        .unwrap_or_default();
    let cache_key = {
        let mut hasher = Sha256::new();
        hasher.update(b"dev-env-v2\0");
        hasher.update(nixpkgs_key.as_bytes());
        hasher.update(b"\0");
        hasher.update(package_name.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry_json.as_bytes());
        hasher.update(b"\0");
        hasher.update(rust_key.as_bytes());
        format!("dev-env-{:x}", hasher.finalize())
    };

    // Check cache (skip if UV_NIX_NO_CACHE is set)
    if env::var_os("UV_NIX_NO_CACHE").is_none() {
        if let Some(cached) = load_dev_env_cache(&cache_key) {
            debug!("Cache hit for dev env: {package_name}");
            return Ok(cached);
        }
    }

    crate::status("Resolving", &format!("build env for {package_name}"));

    let mut env = nixpkgs::resolve_build_env(&libs, &build_tools, package_name, &source)?;

    // If we resolved a rust-overlay toolchain, prepend its bin to PATH
    if let Some(ref toolchain) = rust_toolchain {
        let toolchain_bin = toolchain.bin_path.to_string_lossy().to_string();
        if let Some(path) = env.vars.get_mut("PATH") {
            *path = format!("{toolchain_bin}:{path}");
        } else {
            env.vars.insert("PATH".to_string(), toolchain_bin.clone());
        }
        crate::status("Using", &format!("rust-overlay toolchain at {}", toolchain.bin_path.display()));
    }

    if let Err(err) = save_dev_env_cache(&cache_key, &env) {
        warn!("Failed to write dev env cache for {package_name}: {err}");
    }
    crate::status("Resolved", &format!("build env for {package_name}"));
    Ok(env)
}

/// Check if a Rust-based package needs a newer toolchain than nixpkgs provides.
/// Returns the resolved toolchain if an upgrade is needed, None if no Cargo.toml found.
fn resolve_rust_if_needed(
    source_dir: Option<&Path>,
    nixpkgs_source: &nixpkgs::NixpkgsSource,
    project_dir: &std::path::Path,
) -> anyhow::Result<Option<crate::rust_overlay::ResolvedRustToolchain>> {
    let Some(source_dir) = source_dir else {
        return Ok(None);
    };
    debug!("Checking for Cargo.toml MSRV in: {}", source_dir.display());
    let Some(msrv) = crate::rust_overlay::detect_msrv(source_dir) else {
        return Ok(None);
    };

    crate::status("Detected", &format!("rust-version = {msrv} from Cargo.toml"));

    let nixpkgs_rustc = crate::rust_overlay::nixpkgs_rustc_version(nixpkgs_source)?;
    debug!("nixpkgs rustc: {nixpkgs_rustc}");

    match crate::rust_overlay::check_rust_requirement(&msrv, &nixpkgs_rustc) {
        crate::rust_overlay::RustRequirement::Satisfied => {
            debug!("nixpkgs rustc {nixpkgs_rustc} satisfies MSRV {msrv}");
            Ok(None)
        }
        crate::rust_overlay::RustRequirement::NeedsOverlay { msrv } => {
            let toolchain = crate::rust_overlay::resolve_rust_toolchain(
                &msrv, nixpkgs_source, project_dir,
            )?;
            Ok(Some(toolchain))
        }
    }
}

/// Load cached dev env from ~/.cache/uv-nix/<key>.json.
fn load_dev_env_cache(cache_key: &str) -> Option<nixpkgs::ResolvedBuildEnv> {
    let cache_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?
        .join("uv-nix");

    let path = cache_dir.join(format!("{cache_key}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Save dev env to ~/.cache/uv-nix/<key>.json.
fn save_dev_env_cache(
    cache_key: &str,
    env: &nixpkgs::ResolvedBuildEnv,
) -> anyhow::Result<()> {
    let cache_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .ok_or_else(|| anyhow::anyhow!("Cannot determine cache directory"))?
        .join("uv-nix");

    std::fs::create_dir_all(&cache_dir)?;
    let path = cache_dir.join(format!("{cache_key}.json"));
    let content = serde_json::to_string_pretty(env)?;
    std::fs::write(&path, content)?;
    debug!("Cached dev env at {}", path.display());
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LibrarySpec;

    #[test]
    fn test_prepend_env_empty() {
        let mut env_vars = HashMap::new();
        prepend_env(
            &mut env_vars,
            "TEST_VAR",
            &OsString::from("/nix/store/foo/lib"),
        );
        assert_eq!(
            env_vars.get(&OsString::from("TEST_VAR")),
            Some(&OsString::from("/nix/store/foo/lib"))
        );
    }

    #[test]
    fn test_prepend_env_existing() {
        let mut env_vars = HashMap::new();
        env_vars.insert(OsString::from("TEST_VAR"), OsString::from("/existing/path"));
        prepend_env(
            &mut env_vars,
            "TEST_VAR",
            &OsString::from("/nix/store/foo/lib"),
        );
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
        assert!(
            psycopg2
                .build_tools
                .contains(&"libpq.pg_config".to_string())
        );

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
            extra_libraries: vec![LibrarySpec::all_platforms("openssl")],
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
        // With no package name, returns base build env
        let env = get_nix_build_env(None, None).unwrap_or_default();
        assert!(env.len() < 100);
    }
}
