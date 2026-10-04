use std::fs;
use std::path::{Path, PathBuf};

use tracing::debug;

/// The Python hook module, embedded at compile time.
const CTYPES_HOOK_PY: &str = include_str!("../data/ctypes_hook.py");

/// Install the ctypes hook files into a `site-packages` directory.
///
/// Writes three files:
/// - `_uv_nix_ctypes_hook.py` — the monkey-patching module
/// - `uv-nix.pth` — triggers auto-import on Python startup
/// - `_uv_nix_libs.conf` — line-delimited library paths
pub fn install_ctypes_hook(site_packages: &Path, lib_paths: &[PathBuf]) -> anyhow::Result<()> {
    // Write the hook module
    let hook_path = site_packages.join("_uv_nix_ctypes_hook.py");
    fs::write(&hook_path, CTYPES_HOOK_PY)?;
    debug!("Installed ctypes hook: {}", hook_path.display());

    // Write the .pth file that triggers the import
    let pth_path = site_packages.join("uv-nix.pth");
    fs::write(&pth_path, "import _uv_nix_ctypes_hook\n")?;
    debug!("Installed pth file: {}", pth_path.display());

    // Write/merge the library paths config (preserves paths from prior installs)
    let conf_path = site_packages.join("_uv_nix_libs.conf");
    let mut all_paths: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    if conf_path.exists()
        && let Ok(existing) = fs::read_to_string(&conf_path)
    {
        for line in existing.lines() {
            let line = line.trim();
            if !line.is_empty() && seen.insert(line.to_string()) {
                all_paths.push(line.to_string());
            }
        }
    }
    for p in lib_paths {
        let s = p.to_string_lossy().into_owned();
        if seen.insert(s.clone()) {
            all_paths.push(s);
        }
    }
    fs::write(&conf_path, all_paths.join("\n") + "\n")?;
    debug!(
        "Installed libs config: {} ({} paths)",
        conf_path.display(),
        all_paths.len()
    );

    Ok(())
}
