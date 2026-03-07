use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::debug;

/// Cached result of resolving extra library attrs to paths.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Cache key (hash of nixpkgs source + attrs).
    key: String,
    /// The resolved colon-separated library path string.
    library_path: String,
}

/// Compute a cache key from the nixpkgs source identifier and the attr list.
fn cache_key(nixpkgs_key: &str, attrs: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(nixpkgs_key.as_bytes());
    for attr in attrs {
        hasher.update(b"\0");
        hasher.update(attr.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Path to the cache file within a project.
fn cache_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".uv-nix").join("cache").join("resolved-libs.json")
}

/// Look up cached resolved library paths.
///
/// Returns `Some(library_path)` if the cache key matches.
pub fn lookup(
    project_dir: &Path,
    nixpkgs_key: &str,
    attrs: &[String],
) -> Option<String> {
    let key = cache_key(nixpkgs_key, attrs);
    let path = cache_path(project_dir);
    let content = std::fs::read_to_string(&path).ok()?;
    let entry: CacheEntry = serde_json::from_str(&content).ok()?;

    if entry.key == key {
        debug!("Cache hit for extra libraries");
        Some(entry.library_path)
    } else {
        debug!("Cache miss (key mismatch)");
        None
    }
}

/// Store resolved library paths in the cache.
pub fn store(
    project_dir: &Path,
    nixpkgs_key: &str,
    attrs: &[String],
    library_path: &str,
) -> anyhow::Result<()> {
    let key = cache_key(nixpkgs_key, attrs);
    let path = cache_path(project_dir);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let entry = CacheEntry {
        key,
        library_path: library_path.to_string(),
    };

    let content = serde_json::to_string_pretty(&entry)?;
    std::fs::write(&path, content)?;
    debug!("Cached resolved library paths at {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let attrs = vec!["libGL".to_string(), "zlib".to_string()];
        let nixpkgs_key = "flake-lock:abc123";
        let library_path = "/nix/store/xxx/lib:/nix/store/yyy/lib";

        // Should miss initially
        assert!(lookup(dir.path(), nixpkgs_key, &attrs).is_none());

        // Store
        store(dir.path(), nixpkgs_key, &attrs, library_path).unwrap();

        // Should hit
        let result = lookup(dir.path(), nixpkgs_key, &attrs).unwrap();
        assert_eq!(result, library_path);

        // Different key should miss
        assert!(lookup(dir.path(), "different-key", &attrs).is_none());

        // Different attrs should miss
        let other_attrs = vec!["libGL".to_string()];
        assert!(lookup(dir.path(), nixpkgs_key, &other_attrs).is_none());
    }
}
