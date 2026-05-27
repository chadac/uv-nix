use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tracing::debug;

use std::process::Command;

pub mod build_env;
pub mod cache;
pub mod cli;
pub mod config;
pub mod ctypes_hook;
pub mod nix_config;
pub mod nixpkgs;
pub mod patchelf;

/// Returns the Nix system string for the current platform (e.g., "x86_64-linux", "aarch64-darwin").
pub fn current_system() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { "x86_64-linux" }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { "aarch64-linux" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "x86_64-darwin" }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "aarch64-darwin" }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    { "unknown" }
}

// Re-export CLI types for ergonomic use from uv crate
pub use cli::{CliOutput, InfoOptions, PatchOptions, RebuildOptions};
pub use cli::{nix_info, nix_patch, nix_rebuild};

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

/// Print a status message to stderr matching uv's output style.
///
/// Format: `  {verb:>12} {message}` — right-aligned green verb, then description.
pub fn status(verb: &str, message: &str) {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprintln!("\x1b[1;32m{verb:>12}\x1b[0m {message}");
    } else {
        eprintln!("{verb:>12} {message}");
    }
}

/// Print a warning message to stderr matching uv's output style.
///
/// Format: `  warning: {message}` — yellow "warning:" prefix.
pub fn status_warn(message: &str) {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprintln!("\x1b[1;33m     warning\x1b[0m: {message}");
    } else {
        eprintln!("     warning: {message}");
    }
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
    status("Patching", &format!("ELF binaries in {}", site_packages.display()));
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
        status_warn(&format!("Failed to patch site-packages: {err}"));
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

    // Get library names for the current system
    let libs = uv_nix_config.extra_libraries_for_system(current_system());

    if libs.is_empty() {
        return None;
    }

    // Check cache first
    if let Some(cached) = cache::lookup(&project_dir, &nix_key, &libs) {
        return Some(cached);
    }

    // Cache miss — resolve via nix eval
    match nixpkgs::resolve_library_paths(&libs, &source) {
        Ok(paths) => {
            if let Err(err) = cache::store(&project_dir, &nix_key, &libs, &paths) {
                status_warn(&format!("Failed to cache resolved library paths: {err}"));
            }
            Some(paths)
        }
        Err(err) => {
            status_warn(&format!("Failed to resolve extra libraries: {err}"));
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
    // (only applicable on Linux — Darwin doesn't have ELF interpreters)
    #[cfg(target_os = "linux")]
    {
        let nix = nix_config::require();
        let patcher = &nix.patcher;
        let bin_dir = python_dir.join("bin");
        if let Ok(entries) = fs::read_dir(&bin_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("python3") && !name_str.contains('-') {
                    let output = std::process::Command::new(patcher)
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
/// Uses nix to resolve library paths and patchelf/install_name_tool, then patches
/// ELF/Mach-O binaries in place and installs the ctypes hook.
pub fn post_python_install_patch(python_dir: &Path) {
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

    let config = patchelf::PatchConfig::from_env();

    let python_name = python_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    status("Patching", &format!("{python_name} (nix)"));

    if let Err(err) = patchelf::patch_directory(python_dir, &config) {
        status_warn(&format!("Failed to patch ELF binaries: {err}"));
        return;
    }

    ctypes_hook::install_hook_for_python(python_dir, &config.rpath);

    status("Patched", &format!("{python_name}"));
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
