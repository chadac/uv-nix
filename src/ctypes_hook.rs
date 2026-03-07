use std::fs;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

/// The Python hook module, embedded at compile time.
const CTYPES_HOOK_PY: &str = include_str!("../data/ctypes_hook.py");

/// Locate the `site-packages` directory inside a Python installation.
///
/// Globs for `lib/python*/site-packages/` which is the standard layout
/// for standalone Python builds (e.g., those installed by `uv python install`).
pub fn find_site_packages(python_dir: &Path) -> Option<PathBuf> {
    let lib_dir = python_dir.join("lib");
    let entries = fs::read_dir(&lib_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("python") {
            let sp = entry.path().join("site-packages");
            if sp.is_dir() {
                return Some(sp);
            }
        }
    }
    None
}

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

    // Write the library paths config
    let conf_path = site_packages.join("_uv_nix_libs.conf");
    let contents: String = lib_paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&conf_path, contents + "\n")?;
    debug!("Installed libs config: {}", conf_path.display());

    Ok(())
}

/// Install the ctypes hook into a Python installation directory.
///
/// Finds site-packages and writes the hook files. Logs a warning and
/// returns Ok(()) if site-packages cannot be found.
pub fn install_hook_for_python(python_dir: &Path, lib_paths: &[PathBuf]) {
    let Some(site_packages) = find_site_packages(python_dir) else {
        warn!(
            "Could not find site-packages in {}, skipping ctypes hook",
            python_dir.display()
        );
        return;
    };

    if let Err(err) = install_ctypes_hook(&site_packages, lib_paths) {
        warn!("Failed to install ctypes hook: {err}");
    }
}
