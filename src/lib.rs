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
pub mod nixgen;
pub mod nixpkgs;
pub mod patchelf;
pub mod rust_overlay;
pub mod soname;

/// Returns the Nix system string for the current platform (e.g., "x86_64-linux", "aarch64-darwin").
pub fn current_system() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-linux"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-linux"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-darwin"
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        "unknown"
    }
}

// Re-export CLI types for ergonomic use from uv crate
pub use cli::{CliOutput, InfoOptions, PatchOptions};
pub use cli::{nix_info, nix_patch};
pub use nixgen::{GenOptions, nix_gen};

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

/// Prompt the user for confirmation, matching uv's style.
///
/// Format: `? {message} [y/n] › {default}`
/// Returns the default if not running in an interactive terminal.
pub fn confirm(message: &str, default: bool) -> bool {
    use console::{Key, Term, style};
    use std::io::IsTerminal;

    if !std::io::stderr().is_terminal() {
        return default;
    }

    let term = Term::stderr();
    let prompt = format!(
        "{} {} {} {} {}",
        style("?").yellow(),
        style(message).bold(),
        style("[y/n]").black().bright(),
        style("›").black().bright(),
        style(if default { "yes" } else { "no" }).cyan(),
    );

    let _ = term.write_str(&prompt);
    let _ = term.hide_cursor();
    let _ = term.flush();

    let response = loop {
        match term.read_key() {
            Ok(Key::Char('y' | 'Y')) => break true,
            Ok(Key::Char('n' | 'N')) => break false,
            Ok(Key::Enter) => break default,
            Ok(Key::CtrlC) => {
                let _ = term.show_cursor();
                let _ = term.write_str("\n");
                let _ = term.flush();
                std::process::exit(130);
            }
            _ => {}
        }
    };

    let report = format!(
        "{} {} {} {}",
        style("✔").green(),
        style(message).bold(),
        style("·").black().bright(),
        style(if response { "yes" } else { "no" }).cyan(),
    );
    let _ = term.clear_line();
    let _ = term.write_line(&report);
    let _ = term.show_cursor();
    let _ = term.flush();

    response
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

/// Check if timing instrumentation is enabled via `UV_NIX_TIMING=1`.
fn timing_enabled() -> bool {
    std::env::var("UV_NIX_TIMING").is_ok_and(|v| v == "1")
}

/// Called automatically after wheel installs to patch native binaries for Nix compatibility.
///
/// Only patches binaries belonging to the packages that were just installed (identified
/// by their dist-info prefixes, e.g. `["numpy-2.4.6", "pandas-2.3.0"]`). This ensures
/// each binary is patched exactly once from a pristine state — no accumulated rpaths.
///
/// Uses soname analysis to determine per-binary rpath sets: each binary only gets
/// rpath entries for the shared libraries it actually needs. Results are persisted
/// to `.venv/share/uv-nix/patches.json`.
///
/// When `UV_NIX_TIMING=1` is set, emits a structured timing line to stderr:
/// `uv-nix-timing: nix_resolve=Xms find_binaries=Xms (N files) patch=Xms total=Xms`
pub fn post_install_patch(
    site_packages: &Path,
    installed_packages: &[String],
) -> anyhow::Result<()> {
    use std::time::Instant;

    if installed_packages.is_empty() {
        return Ok(());
    }

    let timing = timing_enabled();
    let t_total = Instant::now();

    // Stage 1: Nix config resolution
    let t0 = Instant::now();
    let nix = nix_config::require();
    let patch_config = patchelf::PatchConfig::from_env();

    // Resolve extra libraries from [tool.uv-nix] in pyproject.toml
    let mut rpath_by_attr = nix.rpath_map.clone();
    if let Some(extra_rpath) = resolve_extra_libraries(site_packages) {
        for (i, path) in extra_rpath.split(':').filter(|s| !s.is_empty()).enumerate() {
            rpath_by_attr.insert(format!("_extra_{i}"), PathBuf::from(path));
        }
    }

    // Get nixpkgs rev for manifest
    let nixpkgs_rev = {
        let cwd = std::env::current_dir().unwrap_or_default();
        let project_dir = nix_config::find_project_root(&cwd).unwrap_or(cwd);
        let uv_nix_config = config::find_config(&project_dir)
            .map(|(c, _)| c)
            .unwrap_or_default();
        let source = nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config);
        nixpkgs::nixpkgs_cache_key(&source)
    };

    let nix_resolve_ms = t0.elapsed().as_millis();

    // Stage 2: Find native binaries from RECORD files, grouped by package
    let t1 = Instant::now();
    let pkg_binaries = collect_package_binaries_from_records(
        site_packages,
        installed_packages,
        patch_config.is_darwin,
    );
    let find_ms = t1.elapsed().as_millis();
    let n_binaries: usize = pkg_binaries.iter().map(|p| p.binaries.len()).sum();

    if n_binaries == 0 {
        if timing {
            let total_ms = t_total.elapsed().as_millis();
            eprintln!(
                "uv-nix-timing: nix_resolve={}ms find_binaries={}ms (0 files) patch=0ms total={}ms",
                nix_resolve_ms, find_ms, total_ms
            );
        }
        return Ok(());
    }

    debug!(
        "Patching {} binaries from {} packages in {}",
        n_binaries,
        installed_packages.len(),
        site_packages.display()
    );

    // Stage 3: Plan targeted patches via soname analysis
    let t2 = Instant::now();
    let (plans, manifest) = match soname::plan_patches(
        site_packages,
        &pkg_binaries,
        &nix.patcher,
        patch_config.is_darwin,
        &rpath_by_attr,
        &nixpkgs_rev,
    ) {
        Ok(result) => result,
        Err(err) => {
            // Soname resolution failed — fall back to global rpath patching
            status_warn(&format!(
                "Soname resolution failed, using global rpath: {err}"
            ));
            let all_binaries: Vec<PathBuf> = pkg_binaries
                .iter()
                .flat_map(|p| p.binaries.iter().cloned())
                .collect();
            patchelf::patch_binaries(&all_binaries, &patch_config)?;
            if timing {
                let total_ms = t_total.elapsed().as_millis();
                eprintln!(
                    "uv-nix-timing: nix_resolve={}ms find_binaries={}ms ({} files) patch={}ms total={}ms (fallback)",
                    nix_resolve_ms,
                    find_ms,
                    n_binaries,
                    t2.elapsed().as_millis(),
                    total_ms
                );
            }
            return Ok(());
        }
    };

    // Stage 4: Apply targeted patches
    for plan in &plans {
        if let Err(err) = patchelf::patch_binary_targeted(
            &plan.binary,
            &plan.rpaths,
            plan.needs_origin,
            &patch_config,
        ) {
            debug!("Failed to patch {}: {err}", plan.binary.display());
        }
    }
    let patch_ms = t2.elapsed().as_millis();

    // Stage 5: Save manifest
    let venv_root = site_packages
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or(site_packages);
    if let Err(err) = manifest.save(venv_root) {
        debug!("Failed to save patch manifest: {err}");
    }

    let total_ms = t_total.elapsed().as_millis();

    if timing {
        eprintln!(
            "uv-nix-timing: nix_resolve={}ms find_binaries={}ms ({} files) patch={}ms total={}ms",
            nix_resolve_ms, find_ms, n_binaries, patch_ms, total_ms
        );
    }

    Ok(())
}

/// Collect native binaries from RECORD files, grouped by package.
///
/// For each dist-info prefix (e.g. "numpy-2.4.6"), reads its RECORD file,
/// filters to native binaries, and returns a `PackageBinaries` struct.
fn collect_package_binaries_from_records(
    site_packages: &Path,
    installed_packages: &[String],
    is_darwin: bool,
) -> Vec<soname::PackageBinaries> {
    let mut result = Vec::new();

    for dist_prefix in installed_packages {
        let record_path = site_packages
            .join(format!("{dist_prefix}.dist-info"))
            .join("RECORD");

        let content = match fs::read_to_string(&record_path) {
            Ok(c) => c,
            Err(err) => {
                debug!("Could not read RECORD for {dist_prefix}: {err}");
                continue;
            }
        };

        // Parse dist-info prefix: "numpy-2.4.6" → name="numpy", version="2.4.6"
        let (name, version) = parse_dist_prefix(dist_prefix);

        let mut binaries = Vec::new();

        for line in content.lines() {
            let rel_path = match line.split(',').next() {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };

            if rel_path.contains(".dist-info/") {
                continue;
            }

            let abs_path = site_packages.join(rel_path);

            if !abs_path.is_file() {
                continue;
            }

            let fname = match abs_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            if is_darwin {
                let is_dylib = fname.contains(".dylib");
                let is_so = fname.contains(".so");
                let is_extensionless = !fname.contains('.');
                if (is_dylib || is_so || is_extensionless)
                    && patchelf::is_native_binary(&abs_path, true)
                {
                    binaries.push(abs_path);
                }
            } else {
                let is_so = fname.contains(".so");
                let is_extensionless = !fname.contains('.');
                if (is_so || is_extensionless) && patchelf::is_native_binary(&abs_path, false) {
                    binaries.push(abs_path);
                }
            }
        }

        if !binaries.is_empty() {
            result.push(soname::PackageBinaries {
                name,
                version,
                binaries,
            });
        }
    }

    result
}

/// Parse a dist-info prefix like "numpy-2.4.6" into (name, version).
///
/// The name portion uses underscores (dist-info normalization), and the version
/// is everything after the last hyphen before the version starts (digits).
fn parse_dist_prefix(prefix: &str) -> (String, String) {
    // Find the split point: last hyphen followed by a digit
    if let Some(idx) = prefix.rfind('-').and_then(|i| {
        if prefix[i + 1..].starts_with(|c: char| c.is_ascii_digit()) {
            Some(i)
        } else {
            None
        }
    }) {
        let name = prefix[..idx].replace('_', "-");
        let version = prefix[idx + 1..].to_string();
        (name, version)
    } else {
        (prefix.replace('_', "-"), String::new())
    }
}

/// Resolve extra library paths from `[tool.uv-nix]` in pyproject.toml.
///
/// Searches upward from the given path for a pyproject.toml with extra-libraries
/// configured, then resolves them via nix eval (with caching).
pub fn resolve_extra_libraries(start: &Path) -> Option<String> {
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
    if let Some(name) = python_dir.file_name().and_then(|n| n.to_str())
        && name.contains("musl")
    {
        return true;
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

    let python_name = python_dir.file_name().unwrap_or_default().to_string_lossy();
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
