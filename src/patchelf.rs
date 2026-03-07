use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, warn};
use walkdir::WalkDir;

/// Configuration for patching ELF binaries with `patchelf`.
pub struct PatchConfig {
    /// Path to the `patchelf` binary.
    pub patchelf: PathBuf,
    /// Dynamic linker interpreter path (e.g., `/nix/store/.../ld-linux-x86-64.so.2`).
    pub interpreter: Option<PathBuf>,
    /// RPATH entries to set on patched binaries.
    pub rpath: Vec<PathBuf>,
}

impl PatchConfig {
    /// Read patch configuration from the resolved NixConfig.
    ///
    /// Calls `nix_config::require()` — exits with an error if Nix is not available.
    pub fn from_env() -> Self {
        let nix = crate::nix_config::require();
        Self {
            patchelf: nix.patchelf.clone(),
            interpreter: Some(nix.interpreter.clone()),
            rpath: nix
                .library_path
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
        }
    }

    /// Build a `PatchConfig` from explicit overrides, falling back to NixConfig.
    pub fn from_overrides(
        patchelf: Option<PathBuf>,
        interpreter: Option<PathBuf>,
        rpath: Option<String>,
    ) -> Self {
        let base = Self::from_env();
        let patchelf_path = patchelf.unwrap_or(base.patchelf);
        let interp = interpreter.or(base.interpreter);
        let rpath_entries = rpath
            .filter(|s| !s.is_empty())
            .map(|s| s.split(':').map(PathBuf::from).collect())
            .unwrap_or(base.rpath);
        Self {
            patchelf: patchelf_path,
            interpreter: interp,
            rpath: rpath_entries,
        }
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

/// Run `patchelf` on a single binary to set the RPATH and interpreter.
///
/// RPATH is set first (works on all ELF files), then the interpreter is set
/// separately (only works on executables, silently skipped for shared libraries
/// which lack an `.interp` section).
pub fn patch_binary(path: &Path, config: &PatchConfig) -> anyhow::Result<()> {
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
        let existing = Command::new(&config.patchelf)
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

        let mut cmd = Command::new(&config.patchelf);
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
        let mut cmd = Command::new(&config.patchelf);
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

/// Patch all ELF binaries found in a directory.
pub fn patch_directory(dir: &Path, config: &PatchConfig) -> anyhow::Result<()> {
    let binaries = find_elf_binaries(dir);
    debug!(
        "Found {} ELF binaries to patch in {}",
        binaries.len(),
        dir.display()
    );
    for binary in &binaries {
        if let Err(err) = patch_binary(binary, config) {
            warn!("Failed to patch {}: {err}", binary.display());
        }
    }
    Ok(())
}
