use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, warn};
use walkdir::WalkDir;

/// Configuration for patching native binaries.
/// 
/// On Linux, uses `patchelf` for ELF binaries.
/// On Darwin, uses `install_name_tool` for Mach-O binaries.
pub struct PatchConfig {
    /// Path to the patcher binary (`patchelf` on Linux, `install_name_tool` on Darwin).
    pub patcher: PathBuf,
    /// Dynamic linker interpreter path (Linux only; None on Darwin).
    pub interpreter: Option<PathBuf>,
    /// RPATH entries to set on patched binaries.
    pub rpath: Vec<PathBuf>,
    /// True if running on Darwin/macOS.
    pub is_darwin: bool,
}

impl PatchConfig {
    /// Read patch configuration from the resolved NixConfig.
    ///
    /// Calls `nix_config::require()` — exits with an error if Nix is not available.
    pub fn from_env() -> Self {
        let nix = crate::nix_config::require();
        let interpreter = if nix.is_darwin || nix.interpreter.as_os_str().is_empty() {
            None
        } else {
            Some(nix.interpreter.clone())
        };
        Self {
            patcher: nix.patcher.clone(),
            interpreter,
            rpath: nix
                .library_path
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            is_darwin: nix.is_darwin,
        }
    }

    /// Build a `PatchConfig` from explicit overrides, falling back to NixConfig.
    pub fn from_overrides(
        patchelf: Option<PathBuf>,
        interpreter: Option<PathBuf>,
        rpath: Option<String>,
    ) -> Self {
        let base = Self::from_env();
        let patcher_path = patchelf.unwrap_or(base.patcher);
        let interp = if base.is_darwin { None } else { interpreter.or(base.interpreter) };
        let rpath_entries = rpath
            .filter(|s| !s.is_empty())
            .map(|s| s.split(':').map(PathBuf::from).collect())
            .unwrap_or(base.rpath);
        Self {
            patcher: patcher_path,
            interpreter: interp,
            rpath: rpath_entries,
            is_darwin: base.is_darwin,
        }
    }
    
    /// Legacy accessor for backwards compatibility.
    pub fn patchelf(&self) -> &PathBuf {
        &self.patcher
    }
}

/// Check if a file starts with the ELF magic bytes (`\x7fELF`).
fn is_elf(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    magic == *b"\x7fELF"
}

/// Check if a file is a Mach-O binary (macOS).
/// 
/// Mach-O files start with one of:
/// - 0xFEEDFACE (32-bit)
/// - 0xFEEDFACF (64-bit)
/// - 0xCAFEBABE (universal/fat binary)
fn is_macho(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    // Check for Mach-O magic numbers (big-endian and little-endian)
    matches!(
        magic,
        [0xFE, 0xED, 0xFA, 0xCE] |  // MH_MAGIC (32-bit BE)
        [0xCE, 0xFA, 0xED, 0xFE] |  // MH_CIGAM (32-bit LE)
        [0xFE, 0xED, 0xFA, 0xCF] |  // MH_MAGIC_64 (64-bit BE)
        [0xCF, 0xFA, 0xED, 0xFE] |  // MH_CIGAM_64 (64-bit LE)
        [0xCA, 0xFE, 0xBA, 0xBE] |  // FAT_MAGIC (universal BE)
        [0xBE, 0xBA, 0xFE, 0xCA]    // FAT_CIGAM (universal LE)
    )
}

/// Find ELF binaries in a directory by checking for `.so` extensions and ELF magic bytes.
pub fn find_elf_binaries(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Match .so files (including .so.1, .so.2.3, etc.) and extensionless executables
        let dominated_by_so = name.contains(".so");
        if dominated_by_so && is_elf(path) {
            results.push(path.to_path_buf());
        } else if !name.contains('.') && is_elf(path) {
            // Extensionless files that are ELF (e.g., python3.12 binary)
            results.push(path.to_path_buf());
        }
    }
    results
}

/// Find Mach-O binaries in a directory (macOS .dylib and .so files).
pub fn find_macho_binaries(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Match .dylib, .so files, and extensionless executables
        let is_dylib = name.contains(".dylib");
        let is_so = name.contains(".so");
        if (is_dylib || is_so) && is_macho(path) {
            results.push(path.to_path_buf());
        } else if !name.contains('.') && is_macho(path) {
            // Extensionless files that are Mach-O (e.g., python3.12 binary)
            results.push(path.to_path_buf());
        }
    }
    results
}

/// Find native binaries in a directory (platform-aware).
pub fn find_native_binaries(dir: &Path, is_darwin: bool) -> Vec<PathBuf> {
    if is_darwin {
        find_macho_binaries(dir)
    } else {
        find_elf_binaries(dir)
    }
}

/// Patch a single native binary (platform-aware).
///
/// On Linux: uses `patchelf` to set RPATH and interpreter.
/// On Darwin: uses `install_name_tool` to add rpath entries.
pub fn patch_binary(path: &Path, config: &PatchConfig) -> anyhow::Result<()> {
    if config.is_darwin {
        patch_macho_binary(path, config)
    } else {
        patch_elf_binary(path, config)
    }
}

/// Run `patchelf` on a single ELF binary to set the RPATH and interpreter.
///
/// RPATH is set first (works on all ELF files), then the interpreter is set
/// separately (only works on executables, silently skipped for shared libraries
/// which lack an `.interp` section).
fn patch_elf_binary(path: &Path, config: &PatchConfig) -> anyhow::Result<()> {
    // Set RPATH: append our paths to the existing RPATH to preserve
    // $ORIGIN-based paths that wheels use for bundled libraries
    if !config.rpath.is_empty() {
        let nix_rpath: String = config
            .rpath
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");

        // Read existing RPATH/RUNPATH
        let existing = Command::new(&config.patcher)
            .arg("--print-rpath")
            .arg(path)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();

        // Build the final RPATH:
        // 1. Keep existing RPATH (preserves $ORIGIN paths from wheels)
        // 2. Ensure $ORIGIN is present (so sibling bundled libs can find each other)
        // 3. Append our Nix library paths
        let mut parts: Vec<String> = Vec::new();
        if !existing.is_empty() {
            parts.push(existing.clone());
        }
        if !existing.contains("$ORIGIN") {
            parts.push("$ORIGIN".to_string());
        }
        parts.push(nix_rpath);
        let rpath_str = parts.join(":");

        let mut cmd = Command::new(&config.patcher);
        cmd.arg("--set-rpath").arg(&rpath_str).arg(path);
        debug!("Running: {:?}", cmd);

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "patchelf --set-rpath failed on {}: {}",
                path.display(),
                stderr.trim()
            );
        }
    }

    // Set interpreter (only works on executables; shared libraries don't have
    // an .interp section, so we ignore that specific failure)
    if let Some(ref interpreter) = config.interpreter {
        let mut cmd = Command::new(&config.patcher);
        cmd.arg("--set-interpreter").arg(interpreter).arg(path);
        debug!("Running: {:?}", cmd);

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = stderr.trim();
            if msg.contains("cannot find section '.interp'") {
                debug!("Skipping --set-interpreter on shared library: {}", path.display());
            } else {
                anyhow::bail!(
                    "patchelf --set-interpreter failed on {}: {}",
                    path.display(),
                    msg
                );
            }
        }
    }

    Ok(())
}

/// Run `install_name_tool` on a single Mach-O binary to add rpath entries.
///
/// On macOS, libraries reference dependencies by install name. We add rpath
/// entries so the dynamic linker can find Nix store libraries.
fn patch_macho_binary(path: &Path, config: &PatchConfig) -> anyhow::Result<()> {
    if config.rpath.is_empty() {
        return Ok(());
    }

    // Add each rpath entry using install_name_tool -add_rpath
    for rpath_entry in &config.rpath {
        let rpath_str = rpath_entry.to_string_lossy();
        
        let mut cmd = Command::new(&config.patcher);
        cmd.arg("-add_rpath").arg(rpath_str.as_ref()).arg(path);
        debug!("Running: {:?}", cmd);

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = stderr.trim();
            // "would duplicate path" means the rpath already exists, which is fine
            if msg.contains("would duplicate path") {
                debug!("Rpath {} already exists in {}", rpath_str, path.display());
            } else {
                // Log but don't fail - some binaries may not be writable
                warn!(
                    "install_name_tool -add_rpath failed on {}: {}",
                    path.display(),
                    msg
                );
            }
        }
    }

    Ok(())
}

/// Patch all native binaries found in a directory (platform-aware).
///
/// On Linux: finds and patches ELF binaries with `patchelf`.
/// On Darwin: finds and patches Mach-O binaries with `install_name_tool`.
pub fn patch_directory(dir: &Path, config: &PatchConfig) -> anyhow::Result<()> {
    let binaries = find_native_binaries(dir, config.is_darwin);
    let binary_type = if config.is_darwin { "Mach-O" } else { "ELF" };
    debug!(
        "Found {} {} binaries to patch in {}",
        binaries.len(),
        binary_type,
        dir.display()
    );
    for binary in &binaries {
        if let Err(err) = patch_binary(binary, config) {
            warn!("Failed to patch {}: {err}", binary.display());
        }
    }
    Ok(())
}
