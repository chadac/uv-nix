//! CLI command implementations for `uv nix` subcommands.
//!
//! These functions are designed to be called from the uv crate with a Printer,
//! matching uv's output style conventions.
//!
//! Output conventions (matching uv):
//! - Status/progress messages → stderr (dimmed)
//! - Data output (info, JSON) → stdout
//! - Completion messages → stderr (with colors)

use std::fmt::Write;
use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;
use serde::Serialize;
use tracing::{debug, warn};

use crate::build_env::{get_effective_package_config, EffectivePackageConfig};
use crate::patchelf::{self, PatchConfig};

/// Output streams for CLI commands, matching uv's Printer pattern.
pub struct CliOutput<'a, O: Write, E: Write> {
    /// Standard output (for data/results).
    pub stdout: &'a mut O,
    /// Standard error (for status/progress messages).
    pub stderr: &'a mut E,
}

/// Options for the `uv nix patch` command.
#[derive(Debug, Clone)]
pub struct PatchOptions {
    /// Path to the virtual environment.
    pub path: PathBuf,
    /// Only patch the Python interpreter.
    pub only_python: bool,
    /// Only patch installed packages.
    pub only_packages: bool,
    /// Specific packages to patch (None = all).
    pub packages: Option<Vec<String>>,
    /// Custom patchelf/install_name_tool path.
    pub patchelf: Option<PathBuf>,
    /// Custom interpreter path (Linux only).
    pub interpreter: Option<PathBuf>,
    /// Additional RPATH entries.
    pub rpath: Option<String>,
}

/// Options for the `uv nix info` command.
#[derive(Debug, Clone)]
pub struct InfoOptions {
    /// Path to the virtual environment.
    pub path: PathBuf,
    /// Show verbose output.
    pub verbose: bool,
    /// Output as JSON.
    pub json: bool,
    /// Show build configuration for a specific package.
    pub package: Option<String>,
}

/// Options for the `uv nix rebuild` command.
#[derive(Debug, Clone)]
pub struct RebuildOptions {
    /// Path to the virtual environment.
    pub path: PathBuf,
    /// Specific packages to rebuild (None = all).
    pub packages: Option<Vec<String>>,
    /// Force rebuild even if up-to-date.
    pub force: bool,
}

/// Information about a patched package.
#[derive(Debug, Clone, Serialize)]
pub struct PatchedPackageInfo {
    /// Package name.
    pub name: String,
    /// Path to the package directory.
    pub path: PathBuf,
    /// Number of patched binaries.
    pub binary_count: usize,
    /// List of patched binary paths (relative to package dir).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub binaries: Vec<String>,
    /// RPATH entries found on binaries.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rpath_entries: Vec<String>,
}

/// Information about the virtual environment's Nix patches.
#[derive(Debug, Clone, Serialize)]
pub struct VenvNixInfo {
    /// Path to the virtual environment.
    pub venv_path: PathBuf,
    /// Whether the Python interpreter is patched.
    pub python_patched: bool,
    /// Python interpreter path.
    pub python_path: PathBuf,
    /// RPATH entries on Python interpreter.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub python_rpath: Vec<String>,
    /// Patched packages.
    pub packages: Vec<PatchedPackageInfo>,
    /// Total number of patched binaries.
    pub total_binaries: usize,
}

/// Handler for `uv nix patch`.
///
/// Takes a `CliOutput` with stdout (for data) and stderr (for status messages).
pub fn nix_patch<O: Write, E: Write>(
    out: &mut CliOutput<'_, O, E>,
    opts: PatchOptions,
) -> anyhow::Result<()> {
    let venv_path = opts.path.canonicalize().unwrap_or(opts.path.clone());

    if !venv_path.exists() {
        anyhow::bail!("Virtual environment not found: {}", venv_path.display());
    }

    // Validate it's a venv
    let python_path = find_python_binary(&venv_path)?;
    let site_packages = find_site_packages(&venv_path)?;

    let config = PatchConfig::from_overrides(opts.patchelf, opts.interpreter, opts.rpath);

    let mut patched_count = 0;

    // Patch Python interpreter
    if !opts.only_packages {
        let _ = writeln!(
            out.stderr,
            "{}",
            "Patching Python interpreter...".dimmed()
        );
        match patchelf::patch_binary(&python_path, &config) {
            Ok(()) => {
                patched_count += 1;
                debug!("Patched Python binary: {}", python_path.display());
            }
            Err(e) => {
                warn!("Failed to patch Python binary: {}", e);
            }
        }

        // Also patch libpython if it exists
        let lib_dir = python_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("lib"))
            .unwrap_or_default();
        if lib_dir.exists() {
            let libpython_binaries = patchelf::find_native_binaries(&lib_dir, config.is_darwin);
            for bin in libpython_binaries {
                if let Err(e) = patchelf::patch_binary(&bin, &config) {
                    debug!("Failed to patch {}: {}", bin.display(), e);
                } else {
                    patched_count += 1;
                }
            }
        }
    }

    // Patch packages
    if !opts.only_python {
        let packages_to_patch = if let Some(ref pkg_names) = opts.packages {
            // Find specific packages
            find_packages_by_name(&site_packages, pkg_names)
        } else {
            // Find all packages with native binaries
            find_all_native_packages(&site_packages, config.is_darwin)
        };

        for (pkg_name, pkg_path) in &packages_to_patch {
            let _ = writeln!(out.stderr, "{}", format!("Patching {pkg_name}...").dimmed());
            let binaries = patchelf::find_native_binaries(pkg_path, config.is_darwin);
            for bin in &binaries {
                if let Err(e) = patchelf::patch_binary(bin, &config) {
                    debug!("Failed to patch {}: {}", bin.display(), e);
                } else {
                    patched_count += 1;
                }
            }
        }

        let s = if patched_count == 1 { "y" } else { "ies" };
        let pkg_s = if packages_to_patch.len() == 1 { "" } else { "s" };
        let _ = writeln!(
            out.stderr,
            "{} {} {} {}",
            "Patched".green().bold(),
            format!("{patched_count} binar{s}").bold(),
            "in",
            format!("{} package{pkg_s}", packages_to_patch.len()).bold()
        );
    }

    Ok(())
}

/// Handler for `uv nix info`.
pub fn nix_info<O: Write, E: Write>(
    out: &mut CliOutput<'_, O, E>,
    opts: InfoOptions,
) -> anyhow::Result<()> {
    // If a specific package is requested, show its build config instead
    if let Some(ref package_name) = opts.package {
        return nix_info_package(out, package_name, opts.json);
    }

    let venv_path = opts.path.canonicalize().unwrap_or(opts.path.clone());

    if !venv_path.exists() {
        anyhow::bail!("Virtual environment not found: {}", venv_path.display());
    }

    let info = collect_venv_info(&venv_path, opts.verbose)?;

    if opts.json {
        // JSON goes to stdout
        let _ = writeln!(out.stdout, "{}", serde_json::to_string_pretty(&info)?);
    } else {
        // Human-readable output also goes to stdout (it's data, not status)
        print_info_text(out.stdout, &info, opts.verbose);
    }

    Ok(())
}

/// Handler for `uv nix info --package <name>`.
///
/// Shows the effective build configuration for a specific package.
fn nix_info_package<O: Write, E: Write>(
    out: &mut CliOutput<'_, O, E>,
    package_name: &str,
    json: bool,
) -> anyhow::Result<()> {
    let config = get_effective_package_config(package_name);

    if json {
        let _ = writeln!(out.stdout, "{}", serde_json::to_string_pretty(&config)?);
    } else {
        print_package_config_text(out.stdout, &config);
    }

    Ok(())
}

/// Print package build configuration in text format.
fn print_package_config_text<W: Write>(out: &mut W, config: &EffectivePackageConfig) {
    let _ = writeln!(
        out,
        "{} {}",
        "Package:".bold(),
        config.name.cyan()
    );
    let _ = writeln!(out);

    // Custom config indicator
    if config.has_custom_config {
        let _ = writeln!(
            out,
            "{} {}",
            "Custom config:".bold(),
            "yes (from pyproject.toml)".green()
        );
    } else {
        let _ = writeln!(
            out,
            "{} {}",
            "Custom config:".bold(),
            "no (using defaults)".dimmed()
        );
    }
    let _ = writeln!(out);

    // Nixpkgs source
    let _ = writeln!(out, "{}", "Nixpkgs source:".bold());
    let _ = writeln!(out, "  {}", config.nixpkgs_source.dimmed());
    let _ = writeln!(out);

    // Libraries
    let _ = writeln!(out, "{}", "Libraries:".bold());
    if config.libraries.is_empty() {
        let _ = writeln!(out, "  {}", "(none)".dimmed());
    } else {
        for lib in &config.libraries {
            let _ = writeln!(out, "  {}", lib.cyan());
        }
    }
    let _ = writeln!(out);

    // Build tools
    let _ = writeln!(out, "{}", "Build tools:".bold());
    if config.build_tools.is_empty() {
        let _ = writeln!(out, "  {}", "(none)".dimmed());
    } else {
        for tool in &config.build_tools {
            let _ = writeln!(out, "  {}", tool.cyan());
        }
    }
}

/// Handler for `uv nix rebuild`.
pub fn nix_rebuild<O: Write, E: Write>(
    out: &mut CliOutput<'_, O, E>,
    opts: RebuildOptions,
) -> anyhow::Result<()> {
    let venv_path = opts.path.canonicalize().unwrap_or(opts.path.clone());

    if !venv_path.exists() {
        anyhow::bail!("Virtual environment not found: {}", venv_path.display());
    }

    // Get fresh config (re-resolves nixpkgs, etc.)
    let config = PatchConfig::from_env();
    let site_packages = find_site_packages(&venv_path)?;

    let packages_to_rebuild = if let Some(ref pkg_names) = opts.packages {
        find_packages_by_name(&site_packages, pkg_names)
    } else {
        find_all_native_packages(&site_packages, config.is_darwin)
    };

    if packages_to_rebuild.is_empty() {
        let _ = writeln!(
            out.stderr,
            "{}",
            "No packages with native binaries found".dimmed()
        );
        return Ok(());
    }

    let pkg_s = if packages_to_rebuild.len() == 1 {
        ""
    } else {
        "s"
    };
    let _ = writeln!(
        out.stderr,
        "{}",
        format!(
            "Rebuilding {} package{pkg_s}...",
            packages_to_rebuild.len()
        )
        .dimmed()
    );

    let mut rebuilt_count = 0;
    for (pkg_name, pkg_path) in &packages_to_rebuild {
        let _ = writeln!(
            out.stderr,
            "{}",
            format!("Rebuilding {pkg_name}...").dimmed()
        );
        let binaries = patchelf::find_native_binaries(pkg_path, config.is_darwin);
        for bin in &binaries {
            if let Err(e) = patchelf::patch_binary(bin, &config) {
                warn!("Failed to patch {}: {}", bin.display(), e);
            } else {
                rebuilt_count += 1;
            }
        }
    }

    let s = if rebuilt_count == 1 { "y" } else { "ies" };
    let _ = writeln!(
        out.stderr,
        "{} {}",
        "Rebuilt".green().bold(),
        format!("{rebuilt_count} binar{s}").bold()
    );
    Ok(())
}

// =============================================================================
// Helper functions
// =============================================================================

/// Find the Python binary in a virtual environment.
fn find_python_binary(venv: &Path) -> anyhow::Result<PathBuf> {
    let bin_dir = venv.join("bin");
    if !bin_dir.exists() {
        anyhow::bail!("No bin directory found in venv: {}", venv.display());
    }

    // Look for python3 or python
    for name in ["python3", "python"] {
        let python = bin_dir.join(name);
        if python.exists() {
            return Ok(python);
        }
    }

    anyhow::bail!("No Python binary found in {}", bin_dir.display())
}

/// Find the site-packages directory in a virtual environment.
fn find_site_packages(venv: &Path) -> anyhow::Result<PathBuf> {
    let lib_dir = venv.join("lib");
    if !lib_dir.exists() {
        anyhow::bail!("No lib directory found in venv: {}", venv.display());
    }

    // Look for lib/python3.X/site-packages
    for entry in std::fs::read_dir(&lib_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("python") {
            let site_packages = entry.path().join("site-packages");
            if site_packages.exists() {
                return Ok(site_packages);
            }
        }
    }

    anyhow::bail!("No site-packages directory found in {}", lib_dir.display())
}

/// Find packages by name in site-packages.
fn find_packages_by_name(site_packages: &Path, names: &[String]) -> Vec<(String, PathBuf)> {
    let mut result = Vec::new();
    let name_set: std::collections::HashSet<_> = names
        .iter()
        .map(|n| n.to_lowercase().replace('-', "_"))
        .collect();

    if let Ok(entries) = std::fs::read_dir(site_packages) {
        for entry in entries.flatten() {
            let entry_name = entry.file_name();
            let entry_str = entry_name.to_string_lossy();

            // Normalize package name (foo_bar, foo-bar -> foo_bar)
            let normalized = entry_str
                .split('-')
                .next()
                .unwrap_or(&entry_str)
                .to_lowercase()
                .replace('-', "_");

            if name_set.contains(&normalized) {
                result.push((entry_str.to_string(), entry.path()));
            }
        }
    }

    result
}

/// Find all packages with native binaries in site-packages.
fn find_all_native_packages(site_packages: &Path, is_darwin: bool) -> Vec<(String, PathBuf)> {
    let mut result = Vec::new();

    if let Ok(entries) = std::fs::read_dir(site_packages) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let binaries = patchelf::find_native_binaries(&path, is_darwin);
            if !binaries.is_empty() {
                let name = entry.file_name().to_string_lossy().to_string();
                result.push((name, path));
            }
        }
    }

    result
}

/// Collect information about a venv's Nix patches.
fn collect_venv_info(venv: &Path, verbose: bool) -> anyhow::Result<VenvNixInfo> {
    let python_path = find_python_binary(venv)?;
    let site_packages = find_site_packages(venv)?;

    let config = PatchConfig::from_env();

    // Check if Python is patched (has Nix store paths in RPATH)
    let python_rpath = get_rpath(&python_path, &config);
    let python_patched = python_rpath.iter().any(|p| p.contains("/nix/store"));

    // Collect package info
    let mut packages = Vec::new();
    let mut total_binaries = 0;

    for entry in std::fs::read_dir(&site_packages)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let binaries = patchelf::find_native_binaries(&path, config.is_darwin);
        if binaries.is_empty() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let binary_count = binaries.len();
        total_binaries += binary_count;

        let (bin_names, rpath_entries) = if verbose {
            let bin_names: Vec<String> = binaries
                .iter()
                .filter_map(|b| b.strip_prefix(&path).ok())
                .map(|p| p.to_string_lossy().to_string())
                .collect();

            // Get RPATH from first binary
            let rpath = binaries
                .first()
                .map(|b| get_rpath(b, &config))
                .unwrap_or_default();

            (bin_names, rpath)
        } else {
            (Vec::new(), Vec::new())
        };

        packages.push(PatchedPackageInfo {
            name,
            path,
            binary_count,
            binaries: bin_names,
            rpath_entries,
        });
    }

    Ok(VenvNixInfo {
        venv_path: venv.to_path_buf(),
        python_patched,
        python_path,
        python_rpath,
        packages,
        total_binaries,
    })
}

/// Get RPATH entries from a binary.
fn get_rpath(path: &Path, config: &PatchConfig) -> Vec<String> {
    if config.is_darwin {
        // Use otool on macOS
        let output = std::process::Command::new("otool")
            .args(["-l", path.to_str().unwrap_or("")])
            .output();

        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // Parse LC_RPATH entries from otool output
            let mut in_rpath = false;
            let mut rpaths = Vec::new();
            for line in stdout.lines() {
                if line.contains("LC_RPATH") {
                    in_rpath = true;
                } else if in_rpath && line.trim().starts_with("path ") {
                    let path_str = line
                        .trim()
                        .strip_prefix("path ")
                        .and_then(|s| s.split_whitespace().next())
                        .unwrap_or("");
                    if !path_str.is_empty() {
                        rpaths.push(path_str.to_string());
                    }
                    in_rpath = false;
                }
            }
            return rpaths;
        }
    } else {
        // Use patchelf on Linux
        let output = std::process::Command::new(&config.patcher)
            .args(["--print-rpath", path.to_str().unwrap_or("")])
            .output();

        if let Ok(out) = output {
            let rpath = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !rpath.is_empty() {
                return rpath.split(':').map(String::from).collect();
            }
        }
    }

    Vec::new()
}

/// Print info in text format matching uv's output style.
fn print_info_text<W: Write>(out: &mut W, info: &VenvNixInfo, verbose: bool) {
    let _ = writeln!(
        out,
        "{} {}",
        "Virtual environment:".bold(),
        info.venv_path.display()
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "{}", "Python interpreter:".bold());
    let _ = writeln!(
        out,
        "  Path: {}",
        info.python_path.display().to_string().cyan()
    );
    let patched_str = if info.python_patched {
        "yes".green().to_string()
    } else {
        "no".yellow().to_string()
    };
    let _ = writeln!(out, "  Patched: {}", patched_str);
    if verbose && !info.python_rpath.is_empty() {
        let _ = writeln!(out, "  RPATH:");
        for entry in &info.python_rpath {
            let _ = writeln!(out, "    {}", entry.dimmed());
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "{} {}",
        "Packages with native binaries:".bold(),
        info.packages.len()
    );
    let _ = writeln!(
        out,
        "{} {}",
        "Total native binaries:".bold(),
        info.total_binaries
    );
    let _ = writeln!(out);

    if !info.packages.is_empty() {
        let _ = writeln!(out, "{}", "Packages:".bold());
        for pkg in &info.packages {
            let _ = writeln!(
                out,
                "  {} {}",
                pkg.name.cyan(),
                format!("({} binaries)", pkg.binary_count).dimmed()
            );
            if verbose {
                for bin in &pkg.binaries {
                    let _ = writeln!(out, "    {}", bin.dimmed());
                }
                if !pkg.rpath_entries.is_empty() {
                    let _ = writeln!(out, "    {}:", "RPATH".dimmed());
                    for entry in &pkg.rpath_entries {
                        let _ = writeln!(out, "      {}", entry.dimmed());
                    }
                }
            }
        }
    }
}
