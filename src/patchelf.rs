use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
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
    /// Extra path prefixes considered safe on macOS (not rewritten).
    pub safe_prefixes: Vec<String>,
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

        let safe_prefixes = load_safe_prefixes();

        Self {
            patcher: nix.patcher.clone(),
            interpreter,
            rpath: nix
                .rpath
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            is_darwin: nix.is_darwin,
            safe_prefixes,
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
        let interp = if base.is_darwin {
            None
        } else {
            interpreter.or(base.interpreter)
        };
        let rpath_entries = rpath
            .filter(|s| !s.is_empty())
            .map(|s| s.split(':').map(PathBuf::from).collect())
            .unwrap_or(base.rpath);
        Self {
            patcher: patcher_path,
            interpreter: interp,
            rpath: rpath_entries,
            is_darwin: base.is_darwin,
            safe_prefixes: base.safe_prefixes,
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
        [0xBE, 0xBA, 0xFE, 0xCA] // FAT_CIGAM (universal LE)
    )
}

/// Check if a file is a native binary (ELF on Linux, Mach-O on Darwin).
pub fn is_native_binary(path: &Path, is_darwin: bool) -> bool {
    if is_darwin {
        is_macho(path)
    } else {
        is_elf(path)
    }
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

        // Skip if already patched (Nix store paths present in RPATH)
        if existing.contains("/nix/store") {
            debug!("Already patched, skipping: {}", path.display());
            return Ok(());
        }

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
                debug!(
                    "Skipping --set-interpreter on shared library: {}",
                    path.display()
                );
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

/// Patch a Mach-O binary by rewriting non-system library references to nix store paths.
///
/// For each linked library:
/// - System references (`/usr/lib/*`, `/System/Library/*`) are left alone
/// - Relative references (`@loader_path/*`, `@rpath/*`, `@executable_path/*`) are left alone
/// - References already in `/nix/store/` are left alone
/// - Everything else is rewritten via `install_name_tool -change` to the nix store equivalent
///
/// Also adds rpaths for any `@rpath/`-based references.
fn patch_macho_binary(path: &Path, config: &PatchConfig) -> anyhow::Result<()> {
    if config.rpath.is_empty() {
        return Ok(());
    }

    // Check if already patched by scanning the binary for /nix/store strings.
    if let Ok(bytes) = fs::read(path)
        && bytes.windows(11).any(|w| w == b"/nix/store/")
    {
        debug!("Already patched, skipping: {}", path.display());
        return Ok(());
    }

    let refs = get_macho_references(path)?;

    let mut changes: Vec<(String, String)> = Vec::new();
    let mut has_rpath_refs = false;

    for ref_path in &refs {
        if ref_path.starts_with("@rpath/") {
            has_rpath_refs = true;
            continue;
        }
        if is_safe_reference(ref_path, &config.safe_prefixes) {
            continue;
        }
        if let Some(nix_path) = find_in_rpath_dirs(ref_path, &config.rpath) {
            changes.push((ref_path.clone(), nix_path));
        } else {
            anyhow::bail!(
                "Non-system library reference in {} cannot be resolved: {}\n\
                 Add the library to [tool.uv-nix] extra-libraries or safe-prefixes in pyproject.toml",
                path.display(),
                ref_path
            );
        }
    }

    if changes.is_empty() && !has_rpath_refs {
        return Ok(());
    }

    let mut cmd = Command::new(&config.patcher);
    for (old, new) in &changes {
        cmd.arg("-change").arg(old).arg(new);
    }
    if has_rpath_refs {
        let mut seen = std::collections::HashSet::new();
        for rpath_entry in &config.rpath {
            if seen.insert(rpath_entry) {
                cmd.arg("-add_rpath").arg(rpath_entry);
            }
        }
    }
    cmd.arg(path);
    debug!("Running: {:?}", cmd);

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        if msg.contains("would duplicate path") && changes.is_empty() {
            debug!("Rpaths already exist in {}, skipping", path.display());
        } else if msg.contains("would duplicate path") {
            let mut cmd2 = Command::new(&config.patcher);
            for (old, new) in &changes {
                cmd2.arg("-change").arg(old).arg(new);
            }
            cmd2.arg(path);
            let out2 = cmd2.output()?;
            if !out2.status.success() {
                let err = String::from_utf8_lossy(&out2.stderr);
                anyhow::bail!(
                    "install_name_tool -change failed on {}: {}",
                    path.display(),
                    err.trim()
                );
            }
        } else {
            anyhow::bail!("install_name_tool failed on {}: {}", path.display(), msg);
        }
    }

    Ok(())
}

/// Patch a single binary with specific rpaths (not the global config rpaths).
///
/// Used by the targeted patching path where each binary gets only the
/// rpath entries it actually needs based on soname analysis.
/// When `needs_origin` is true, ensures `$ORIGIN` is in the rpath even if
/// `rpaths` is empty (for binaries with bundled sibling dependencies).
pub fn patch_binary_targeted(
    path: &Path,
    rpaths: &[PathBuf],
    needs_origin: bool,
    config: &PatchConfig,
) -> anyhow::Result<()> {
    // Build a temporary config with the targeted rpaths
    let targeted = PatchConfig {
        patcher: config.patcher.clone(),
        interpreter: config.interpreter.clone(),
        rpath: rpaths.to_vec(),
        is_darwin: config.is_darwin,
        safe_prefixes: config.safe_prefixes.clone(),
    };
    if config.is_darwin {
        patch_macho_binary(path, &targeted)
    } else if rpaths.is_empty() && needs_origin {
        // Binary only needs $ORIGIN for bundled sibling libs — ensure it's set
        ensure_origin_rpath(path, config)
    } else {
        patch_elf_binary(path, &targeted)
    }
}

/// Ensure `$ORIGIN` is in a binary's rpath (for bundled sibling dependencies).
///
/// Only modifies the binary if `$ORIGIN` is not already present.
fn ensure_origin_rpath(path: &Path, config: &PatchConfig) -> anyhow::Result<()> {
    let existing = Command::new(&config.patcher)
        .arg("--print-rpath")
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if existing.contains("$ORIGIN") {
        return Ok(());
    }

    let new_rpath = if existing.is_empty() {
        "$ORIGIN".to_string()
    } else {
        format!("{existing}:$ORIGIN")
    };

    let mut cmd = Command::new(&config.patcher);
    cmd.arg("--set-rpath").arg(&new_rpath).arg(path);
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
    Ok(())
}

/// Parse `otool -L` output to get the list of linked library paths.
///
/// The first entry is always the library's own install name (LC_ID_DYLIB),
/// so we skip it unconditionally.
fn get_macho_references(path: &Path) -> anyhow::Result<Vec<String>> {
    let output = Command::new("otool").arg("-L").arg(path).output()?;
    if !output.status.success() {
        anyhow::bail!(
            "otool -L failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut refs = Vec::new();
    let mut first = true;

    for line in stdout.lines().skip(1) {
        let trimmed = line.trim();
        let ref_path = if let Some(pos) = trimmed.find(" (compatibility") {
            &trimmed[..pos]
        } else if let Some(pos) = trimmed.find(" (") {
            &trimmed[..pos]
        } else {
            continue;
        };

        // First entry is always the library's own install name (LC_ID_DYLIB)
        if first {
            first = false;
            continue;
        }

        refs.push(ref_path.to_string());
    }

    Ok(refs)
}

/// Check if a library reference is safe (should not be rewritten).
fn is_safe_reference(path: &str, extra_safe_prefixes: &[String]) -> bool {
    const BUILTIN_SAFE_PREFIXES: &[&str] = &[
        "/usr/lib/",
        "/System/Library/",
        "@loader_path/",
        "@rpath/",
        "@executable_path/",
        "/nix/store/",
        "/DLC/",
    ];

    for prefix in BUILTIN_SAFE_PREFIXES {
        if path.starts_with(prefix) {
            return true;
        }
    }
    for prefix in extra_safe_prefixes {
        if path.starts_with(prefix.as_str()) {
            return true;
        }
    }
    false
}

/// Search rpath directories for a library matching the given reference.
fn find_in_rpath_dirs(ref_path: &str, rpath_dirs: &[PathBuf]) -> Option<String> {
    let filename = Path::new(ref_path).file_name()?.to_str()?;
    for dir in rpath_dirs {
        let candidate = dir.join(filename);
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// Load safe prefixes from project config, if available.
fn load_safe_prefixes() -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let project_dir = crate::nix_config::find_project_root(&cwd).unwrap_or(cwd);
    crate::config::find_config(&project_dir)
        .map(|(c, _)| c.safe_prefixes)
        .unwrap_or_default()
}

/// Patch a list of native binaries (platform-aware).
///
/// This is the core patching loop, separated from directory scanning
/// so callers can time each stage independently. Collects all errors
/// and returns the first one encountered (after attempting all binaries).
///
/// Uses a local rayon thread pool to avoid conflicts with the host
/// application's global pool configuration.
pub fn patch_binaries(binaries: &[PathBuf], config: &PatchConfig) -> anyhow::Result<()> {
    let pool = rayon::ThreadPoolBuilder::new().build().unwrap_or_else(|_| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
    });

    let errors: Vec<_> = pool.install(|| {
        binaries
            .par_iter()
            .filter_map(|binary| patch_binary(binary, config).err().map(|e| (binary, e)))
            .collect()
    });

    if let Some((path, err)) = errors.first() {
        if errors.len() > 1 {
            warn!("{} additional binaries failed to patch", errors.len() - 1);
        }
        anyhow::bail!("Failed to patch {}: {err}", path.display());
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
    patch_binaries(&binaries, config)
}
