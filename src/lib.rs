use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use std::process::Command;

pub mod build_env;
pub mod cache;
pub mod config;
pub mod ctypes_hook;
pub mod nix_build;
pub mod nix_config;
pub mod nixpkgs;
pub mod patchelf;

/// Create a `nix` command with the required experimental features enabled.
///
/// All `nix` subcommands (build, eval, etc.) need `nix-command`.
/// Expressions using `builtins.getFlake` also need `flakes`.
pub fn nix_command() -> Command {
    let mut cmd = Command::new("nix");
    cmd.arg("--extra-experimental-features")
        .arg("nix-command flakes");
    cmd
}

/// Handler for `uv nix hello`.
pub fn nix_hello(name: Option<String>) -> anyhow::Result<()> {
    let greeting = match name {
        Some(ref n) => format!("Hello, {n}! uv-nix is working."),
        None => "Hello from uv-nix! The nix subcommand is working.".to_string(),
    };
    writeln!(std::io::stdout(), "{greeting}")?;
    Ok(())
}

/// Called automatically after wheel installs to patch `.so` files for NixOS compatibility.
pub fn post_install_patch(site_packages: &Path) {
    let mut patch_config = patchelf::PatchConfig::from_env();

    // Resolve extra libraries from [tool.uv-nix] in pyproject.toml
    if let Some(extra_rpath) = resolve_extra_libraries(site_packages) {
        for path in extra_rpath.split(':').filter(|s| !s.is_empty()) {
            patch_config.rpath.push(PathBuf::from(path));
        }
    }

    debug!(
        "Patching ELF binaries in site-packages: {}",
        site_packages.display()
    );
    if let Err(err) = patchelf::patch_directory(site_packages, &patch_config) {
        warn!("Failed to patch site-packages: {err}");
    }
}

/// Resolve extra library paths from `[tool.uv-nix]` in pyproject.toml.
///
/// Searches upward from the given path for a pyproject.toml with extra-libraries
/// configured, then resolves them via nix eval (with caching).
fn resolve_extra_libraries(start: &Path) -> Option<String> {
    let (uv_nix_config, project_dir) = config::find_config(start)?;

    if uv_nix_config.extra_libraries.is_empty() {
        return None;
    }

    debug!(
        "Found {} extra libraries in pyproject.toml",
        uv_nix_config.extra_libraries.len()
    );

    let source = nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config);
    let nix_key = nixpkgs::nixpkgs_cache_key(&source);

    // Check cache first
    if let Some(cached) = cache::lookup(&project_dir, &nix_key, &uv_nix_config.extra_libraries) {
        return Some(cached);
    }

    // Cache miss — resolve via nix eval
    match nixpkgs::resolve_library_paths(&uv_nix_config.extra_libraries, &source) {
        Ok(paths) => {
            if let Err(err) =
                cache::store(&project_dir, &nix_key, &uv_nix_config.extra_libraries, &paths)
            {
                warn!("Failed to cache resolved library paths: {err}");
            }
            Some(paths)
        }
        Err(err) => {
            warn!("Failed to resolve extra libraries: {err}");
            None
        }
    }
}

/// Check if a Python installation is musl-linked (e.g., Alpine).
///
/// Our patching sets glibc-based interpreter and RPATH, which would break
/// musl-linked binaries. Detected via directory name convention first,
/// then by checking the ELF interpreter of the Python binary.
fn is_musl_python(python_dir: &Path) -> bool {
    // Fast check: uv's managed Python naming convention includes "musl"
    if let Some(name) = python_dir.file_name().and_then(|n| n.to_str()) {
        if name.contains("musl") {
            return true;
        }
    }

    // Fallback: check the ELF interpreter of the Python binary
    {
        let nix = nix_config::require();
        let patchelf = &nix.patchelf;
        let bin_dir = python_dir.join("bin");
        if let Ok(entries) = fs::read_dir(&bin_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("python3") && !name_str.contains('-') {
                    let output = std::process::Command::new(&patchelf)
                        .arg("--print-interpreter")
                        .arg(entry.path())
                        .output();
                    if let Ok(out) = output {
                        let interp = String::from_utf8_lossy(&out.stdout);
                        if interp.contains("musl") {
                            return true;
                        }
                    }
                    break;
                }
            }
        }
    }

    false
}

/// Called automatically after `uv python install` to patch the Python interpreter.
///
/// `python_dir` is the installation directory (e.g., `cpython-3.12.13-linux-x86_64-gnu/`)
/// containing `bin/`, `lib/`, etc.
///
/// If `nix-build` is on PATH, uses a Nix derivation to produce a patched
/// copy in `/nix/store/` and replaces the directory with a symlink.
/// Falls back to in-place patchelf if the derivation approach fails.
pub fn post_python_install_patch(python_dir: &Path) {
    // Skip if already a symlink (previously patched)
    if python_dir.read_link().is_ok() {
        return;
    }

    // Skip musl-linked Python — our glibc paths would break it
    if is_musl_python(python_dir) {
        debug!(
            "Skipping patching for musl-linked Python: {}",
            python_dir.display()
        );
        return;
    }

    // Nix is required — require() exits with error if not available
    let _nix = nix_config::require();

    // Try the Nix derivation approach first
    let cwd = std::env::current_dir().unwrap_or_default();
    let project_dir = nix_config::find_project_root(&cwd).unwrap_or(cwd);
    let uv_nix_cfg = config::find_config(&project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();
    let source = nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_cfg);

    debug!(
        "Patching Python via Nix derivation: {}",
        python_dir.display()
    );
    match nix_build::nix_patch_python(python_dir, &source) {
        Ok(store_path) => {
            if let Err(err) = fs::remove_dir_all(python_dir) {
                warn!("Failed to remove python dir: {err}");
                return;
            }
            if let Err(err) = symlink(&store_path, python_dir) {
                warn!("Failed to create symlink: {err}");
                return;
            }
            debug!(
                "Patched Python linked: {} -> {}",
                python_dir.display(),
                store_path.display()
            );
            return;
        }
        Err(err) => {
            warn!("Nix patching failed, trying patchelf fallback: {err}");
        }
    }

    // Fallback: copy-then-patch with patchelf, then replace original with symlink
    let config = patchelf::PatchConfig::from_env();

    let patched_dir = python_dir.with_extension("nix");

    // If the patched copy already exists (e.g., interrupted previous run), remove it
    if patched_dir.exists() {
        if let Err(err) = fs::remove_dir_all(&patched_dir) {
            warn!("Failed to remove stale patched dir: {err}");
            return;
        }
    }

    // Copy the entire Python installation
    debug!(
        "Copying Python installation for patching: {} -> {}",
        python_dir.display(),
        patched_dir.display()
    );
    if let Err(err) = copy_dir_recursive(python_dir, &patched_dir) {
        warn!("Failed to copy Python installation: {err}");
        let _ = fs::remove_dir_all(&patched_dir);
        return;
    }

    // Patch the copy
    debug!(
        "Patching ELF binaries in copied Python installation: {}",
        patched_dir.display()
    );
    if let Err(err) = patchelf::patch_directory(&patched_dir, &config) {
        warn!("Failed to patch Python installation: {err}");
        let _ = fs::remove_dir_all(&patched_dir);
        return;
    }

    // Install ctypes hook into the patched copy
    ctypes_hook::install_hook_for_python(&patched_dir, &config.rpath);

    // Replace the original with a symlink to the patched copy
    if let Err(err) = fs::remove_dir_all(python_dir) {
        warn!("Failed to remove original python dir: {err}");
        return;
    }
    if let Err(err) = symlink(&patched_dir, python_dir) {
        warn!("Failed to create symlink to patched dir: {err}");
        return;
    }
    debug!(
        "Patched Python linked: {} -> {}",
        python_dir.display(),
        patched_dir.display()
    );
}

/// Recursively copy a directory and its contents, preserving permissions.
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_symlink() {
            let link_target = fs::read_link(entry.path())?;
            // Remove existing file if somehow present
            let _ = fs::remove_file(&target);
            symlink(&link_target, &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Handler for `uv nix patch-env` — manually patch a virtual environment.
pub fn patch_env(
    path: &Path,
    patchelf: Option<PathBuf>,
    interpreter: Option<PathBuf>,
    rpath: Option<String>,
) -> anyhow::Result<()> {
    let config = patchelf::PatchConfig::from_overrides(patchelf, interpreter, rpath);
    patchelf::patch_directory(path, &config)
}

/// Handler for `uv nix patch-python` — manually patch a Python installation.
pub fn patch_python(
    path: &Path,
    patchelf: Option<PathBuf>,
    interpreter: Option<PathBuf>,
    rpath: Option<String>,
) -> anyhow::Result<()> {
    let config = patchelf::PatchConfig::from_overrides(patchelf, interpreter, rpath);
    patchelf::patch_directory(path, &config)?;

    // Install ctypes hook so dlopen() can find Nix libraries
    ctypes_hook::install_hook_for_python(path, &config.rpath);

    Ok(())
}
