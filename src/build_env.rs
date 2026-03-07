use std::collections::HashMap;
use std::env;
use std::ffi::OsString;

use tracing::{debug, warn};

use crate::nixpkgs;

/// Build a map of environment variables to inject into source distribution builds.
///
/// Reads default paths from the resolved NixConfig and merges per-project
/// `[tool.uv-nix] extra-libraries` resolved via nix eval.
pub fn get_nix_build_env() -> HashMap<OsString, OsString> {
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
        // library_path includes both runtime libs AND per-package build deps.
        prepend_env(&mut env_vars, "LIBRARY_PATH", &OsString::from(&nix.library_path));
        prepend_env(&mut env_vars, "C_INCLUDE_PATH", &OsString::from(&nix.include_path));
        prepend_env(&mut env_vars, "PKG_CONFIG_PATH", &OsString::from(&nix.pkg_config_path));
        if !nix.bin_path.is_empty() {
            prepend_env(&mut env_vars, "PATH", &OsString::from(&nix.bin_path));
        }
        env_vars.insert(
            OsString::from("PKG_CONFIG"),
            OsString::from(&nix.pkg_config),
        );
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
    fn test_get_nix_build_env_no_vars() {
        // With no UV_NIX_* vars set, should return empty (or only extras)
        let env = get_nix_build_env();
        // In test context, UV_NIX_* vars are not set, so result depends on CWD
        // At minimum, the function should not panic
        assert!(env.len() < 100); // sanity check
    }
}
