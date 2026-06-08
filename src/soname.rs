//! Soname detection and resolution for targeted binary patching.
//!
//! Instead of applying a global RPATH to every binary, this module:
//! 1. Reads each binary's needed shared libraries (readelf -d / otool -L)
//! 2. Maps sonames to nixpkgs attrs via a pre-compiled cache
//! 3. Falls back to nix eval for unknown sonames
//! 4. Returns per-binary rpath sets containing only what's actually needed
//!
//! The pre-compiled soname map lives at `data/soname-map.json` and is
//! regenerated via `just generate-soname-map`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Pre-compiled soname map (embedded at compile time)
// ---------------------------------------------------------------------------

/// Embedded pre-compiled soname map from `data/soname-map.json`.
const SONAME_MAP_JSON: &str = include_str!("../data/soname-map.json");

/// Pre-compiled mapping of soname patterns to nixpkgs attrs.
///
/// Keys are soname strings (e.g. `libz.so.1` on Linux, `libz.1.dylib` on macOS).
/// Values are nixpkgs attr strings (e.g. `zlib`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SonameMap {
    #[serde(default)]
    pub linux: HashMap<String, String>,
    #[serde(default)]
    pub darwin: HashMap<String, String>,
}

impl SonameMap {
    /// Load the embedded pre-compiled soname map.
    pub fn load_embedded() -> anyhow::Result<Self> {
        let map: SonameMap = serde_json::from_str(SONAME_MAP_JSON)?;
        Ok(map)
    }

    /// Get the platform-appropriate soname map.
    pub fn for_platform(&self, is_darwin: bool) -> &HashMap<String, String> {
        if is_darwin { &self.darwin } else { &self.linux }
    }
}

// ---------------------------------------------------------------------------
// System library allowlist (no rpath needed)
// ---------------------------------------------------------------------------

/// Linux soname prefixes that are kernel-injected virtual DSOs — not real
/// libraries and cannot come from Nix. Everything else (glibc, libstdc++,
/// libgcc_s, etc.) should be resolved from the Nix store.
const LINUX_SYSTEM_LIBS: &[&str] = &["linux-vdso.so", "linux-gate.so"];

/// macOS install name prefixes provided by the system — no rpath needed.
const DARWIN_SYSTEM_LIBS: &[&str] = &[
    "/usr/lib/libSystem",
    "/usr/lib/libc++",
    "/usr/lib/libobjc",
    "/usr/lib/libz",
    "/usr/lib/libresolv",
    "/System/Library/Frameworks/",
];

/// Check if a soname is a known system library that doesn't need an rpath.
pub fn is_system_lib(soname: &str, is_darwin: bool) -> bool {
    let patterns: &[&str] = if is_darwin {
        DARWIN_SYSTEM_LIBS
    } else {
        LINUX_SYSTEM_LIBS
    };
    patterns.iter().any(|pat| soname.contains(pat))
}

// ---------------------------------------------------------------------------
// Soname detection (reads binary headers)
// ---------------------------------------------------------------------------

/// Needed shared libraries extracted from a native binary.
#[derive(Debug, Clone)]
pub struct NeededLibs {
    /// The binary path.
    pub binary: PathBuf,
    /// Sonames that need Nix resolution (system libs filtered out).
    pub needed: Vec<String>,
    /// Sonames resolvable via $ORIGIN/@loader_path (bundled in wheel).
    pub origin_resolvable: Vec<String>,
}

/// Read needed shared libraries from an ELF binary via `patchelf --print-needed`.
///
/// Returns one soname per line. Classifies each as system, origin-resolvable,
/// or needs-resolution.
fn read_elf_needed(
    binary: &Path,
    patchelf: &Path,
    site_packages: &Path,
) -> anyhow::Result<NeededLibs> {
    let output = std::process::Command::new(patchelf)
        .arg("--print-needed")
        .arg(binary)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "patchelf --print-needed failed on {}: {}",
            binary.display(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let binary_dir = binary.parent().unwrap_or(Path::new("."));

    let mut needed = Vec::new();
    let mut origin_resolvable = Vec::new();

    for line in stdout.lines() {
        let soname = line.trim();
        if soname.is_empty() {
            continue;
        }

        if is_system_lib(soname, false) {
            debug!("  system lib (skip): {soname}");
            continue;
        }

        // Check if resolvable via $ORIGIN (bundled in same dir or .libs subdir)
        if binary_dir.join(soname).exists()
            || binary_dir.join("..").join(soname).exists()
            || find_in_libs_dir(site_packages, soname)
        {
            debug!("  origin-resolvable (skip): {soname}");
            origin_resolvable.push(soname.to_string());
            continue;
        }

        debug!("  needs resolution: {soname}");
        needed.push(soname.to_string());
    }

    Ok(NeededLibs {
        binary: binary.to_path_buf(),
        needed,
        origin_resolvable,
    })
}

/// Check if a soname exists in any `*.libs` directory under site-packages.
///
/// Many wheels bundle shared libs in e.g. `numpy.libs/libopenblas64_.so`.
/// The binary references them via $ORIGIN/../numpy.libs/ rpath.
/// We search all `*.libs` dirs at the site-packages root because a binary
/// in one package (e.g. scipy) may reference a vendored lib bundled by
/// another package (e.g. numpy.libs/libscipy_openblas64_.so).
fn find_in_libs_dir(site_packages: &Path, soname: &str) -> bool {
    if let Ok(entries) = std::fs::read_dir(site_packages) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".libs") && entry.path().join(soname).exists() {
                return true;
            }
        }
    }
    false
}

/// Read needed shared libraries from a Mach-O binary via `otool -L`.
///
/// Parses install names from load commands. The first indented line is the
/// binary's own install name (skip it). Classifies rest as: system
/// (/usr/lib, /System), @rpath/@loader_path (bundled), or needs-resolution.
fn read_macho_needed(binary: &Path, site_packages: &Path) -> anyhow::Result<NeededLibs> {
    let output = std::process::Command::new("otool")
        .arg("-L")
        .arg(binary)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("otool -L failed on {}: {}", binary.display(), stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();

    // First line is the binary path, skip it
    lines.next();

    let binary_dir = binary.parent().unwrap_or(Path::new("."));
    let mut needed = Vec::new();
    let mut origin_resolvable = Vec::new();
    let mut is_first = true;

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: "/path/to/lib.dylib (compatibility version X, current version Y)"
        let install_name = match line.split(" (compatibility").next() {
            Some(name) => name.trim(),
            None => continue,
        };

        // First indented entry is often the binary's own install name
        if is_first {
            is_first = false;
            // If it matches the binary's own name, skip it
            if let Some(bin_name) = binary.file_name().and_then(|n| n.to_str())
                && install_name.ends_with(bin_name)
            {
                continue;
            }
        }

        if is_system_lib(install_name, true) {
            debug!("  system lib (skip): {install_name}");
            continue;
        }

        // @rpath and @loader_path references are bundled/wheel-internal
        if install_name.starts_with("@rpath/") || install_name.starts_with("@loader_path/") {
            // Extract the filename portion
            let lib_name = install_name.rsplit('/').next().unwrap_or(install_name);

            // Check if resolvable locally
            if binary_dir.join(lib_name).exists() || find_in_libs_dir(site_packages, lib_name) {
                debug!("  origin-resolvable (skip): {install_name}");
                origin_resolvable.push(install_name.to_string());
                continue;
            }

            // @rpath reference that isn't locally bundled — needs a nix rpath
            // Extract just the dylib filename for soname map lookup
            debug!("  needs resolution: {install_name}");
            needed.push(lib_name.to_string());
            continue;
        }

        // Absolute path not in system dirs — extract filename for lookup
        let lib_name = install_name.rsplit('/').next().unwrap_or(install_name);
        debug!("  needs resolution: {install_name}");
        needed.push(lib_name.to_string());
    }

    Ok(NeededLibs {
        binary: binary.to_path_buf(),
        needed,
        origin_resolvable,
    })
}

/// Read needed shared libraries from a native binary (platform-aware).
pub fn read_needed_libs(
    binary: &Path,
    patcher: &Path,
    is_darwin: bool,
    site_packages: &Path,
) -> anyhow::Result<NeededLibs> {
    if is_darwin {
        read_macho_needed(binary, site_packages)
    } else {
        read_elf_needed(binary, patcher, site_packages)
    }
}

// ---------------------------------------------------------------------------
// Soname resolution (map lookup + nix eval fallback)
// ---------------------------------------------------------------------------

/// Result of resolving a binary's needed libs to Nix packages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedBinary {
    /// Sonames the binary needs (filtered, no system libs).
    pub needed: Vec<String>,
    /// Nixpkgs attrs that provide the needed libs.
    pub nix_libs: Vec<String>,
    /// Concrete rpath entries (Nix store paths) added to the binary.
    pub rpaths_added: Vec<String>,
}

/// Resolve a binary's needed sonames to nixpkgs attrs and rpath entries.
///
/// Lookup chain:
/// 1. Pre-compiled soname map → nix attr
/// 2. Nix eval fallback for unmapped sonames
/// 3. Error with instructions if still unresolved
///
/// `rpath_by_attr` maps nix attr names → their `/nix/store/.../lib` paths.
pub fn resolve_binary(
    needed: &NeededLibs,
    soname_map: &HashMap<String, String>,
    rpath_by_attr: &HashMap<String, PathBuf>,
) -> anyhow::Result<ResolvedBinary> {
    let mut nix_libs = Vec::new();
    let mut rpaths_added = Vec::new();
    let mut seen_attrs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unresolved = Vec::new();

    for soname in &needed.needed {
        // Step 1: Look up soname in the pre-compiled map
        if let Some(attr) = soname_map.get(soname.as_str()) {
            if seen_attrs.insert(attr.clone()) {
                nix_libs.push(attr.clone());
                if let Some(store_path) = rpath_by_attr.get(attr.as_str()) {
                    rpaths_added.push(store_path.to_string_lossy().to_string());
                } else {
                    debug!("  attr '{attr}' mapped but no store path in rpath_by_attr");
                }
            }
            continue;
        }

        // Step 2: Try fuzzy match — strip version suffix from soname
        // e.g. "libz.so.1.3.1" should match "libz.so.1" in the map
        if let Some((attr, _)) = soname_map
            .iter()
            .find(|(key, _)| soname.starts_with(key.as_str()) || key.starts_with(soname.as_str()))
        {
            let attr = attr.clone();
            if seen_attrs.insert(attr.clone()) {
                nix_libs.push(attr.clone());
                if let Some(store_path) = rpath_by_attr.get(attr.as_str()) {
                    rpaths_added.push(store_path.to_string_lossy().to_string());
                }
            }
            continue;
        }

        unresolved.push(soname.clone());
    }

    // Step 3: Try rpath scan fallback for any unresolved sonames
    if !unresolved.is_empty() {
        let scan_results = resolve_sonames_via_rpath_scan(&unresolved, rpath_by_attr);
        let mut still_unresolved = Vec::new();

        for soname in &unresolved {
            if let Some(attr) = scan_results.get(soname.as_str()) {
                if seen_attrs.insert(attr.clone()) {
                    nix_libs.push(attr.clone());
                    if let Some(store_path) = rpath_by_attr.get(attr.as_str()) {
                        rpaths_added.push(store_path.to_string_lossy().to_string());
                    }
                }
            } else {
                still_unresolved.push(soname.clone());
            }
        }

        if !still_unresolved.is_empty() {
            let mut msg = format!(
                "Could not resolve shared libraries for:\n  {}\n\nUnresolved libraries:\n",
                needed.binary.display()
            );
            for lib in &still_unresolved {
                msg.push_str(&format!("  - {lib}\n"));
            }
            msg.push_str(
                "\nThese libraries are not in the pre-compiled soname map and were not found\n\
                 in any resolved Nix library paths or bundled *.libs directories.\n\n\
                 To fix this, add the required nixpkgs attrs to your pyproject.toml:\n\n\
                 [tool.uv-nix]\n\
                 extra-libraries = [\"<nixpkgs-attr>\"]\n\n\
                 Then run `just generate-soname-map` to update the soname cache.",
            );
            anyhow::bail!(msg);
        }
    }

    Ok(ResolvedBinary {
        needed: needed.needed.clone(),
        nix_libs,
        rpaths_added,
    })
}

/// Fallback: resolve unknown sonames by scanning the already-resolved rpath dirs.
///
/// For each attr's lib path, lists .so/.dylib files and checks if any match
/// the unresolved sonames. Returns soname → attr for any matches found.
fn resolve_sonames_via_rpath_scan(
    sonames: &[String],
    rpath_by_attr: &HashMap<String, PathBuf>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();

    for (attr, lib_path) in rpath_by_attr {
        if !lib_path.exists() {
            continue;
        }
        let entries = match std::fs::read_dir(lib_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            for soname in sonames {
                if result.contains_key(soname.as_str()) {
                    continue;
                }
                // Exact match or prefix match (soname may have extra version digits)
                if *soname == *name_str
                    || name_str.starts_with(soname.as_str())
                    || soname.starts_with(name_str.as_ref())
                {
                    debug!(
                        "  rpath scan: {soname} → {attr} (found {name_str} in {})",
                        lib_path.display()
                    );
                    result.insert(soname.clone(), attr.clone());
                }
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Manifest (persisted to .venv/share/uv-nix/patches.json)
// ---------------------------------------------------------------------------

/// The full patch manifest for a virtual environment.
///
/// Persisted at `<venv>/share/uv-nix/patches.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchManifest {
    /// The nixpkgs revision used for resolution.
    pub nixpkgs_rev: String,
    /// Per-package patch information, keyed by normalized package name.
    pub packages: BTreeMap<String, PackagePatchInfo>,
}

/// Patch information for a single installed package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagePatchInfo {
    /// Package version string.
    pub version: String,
    /// Per-binary patch details, keyed by relative path from site-packages.
    pub patches: BTreeMap<String, ResolvedBinary>,
}

impl PatchManifest {
    /// Load an existing manifest from disk, or return an empty one.
    pub fn load_or_default(venv: &Path) -> Self {
        let path = Self::manifest_path(venv);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|err| {
                warn!("Failed to parse patches.json, starting fresh: {err}");
                Self::empty("")
            }),
            Err(_) => Self::empty(""),
        }
    }

    /// Save the manifest to disk.
    pub fn save(&self, venv: &Path) -> anyhow::Result<()> {
        let path = Self::manifest_path(venv);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        debug!("Saved patch manifest to {}", path.display());
        Ok(())
    }

    /// Update (or insert) a package entry.
    pub fn upsert_package(&mut self, name: String, info: PackagePatchInfo) {
        self.packages.insert(name, info);
    }

    /// Remove a package entry.
    pub fn remove_package(&mut self, name: &str) {
        self.packages.remove(name);
    }

    /// Path to the manifest file within a venv.
    fn manifest_path(venv: &Path) -> PathBuf {
        venv.join("share").join("uv-nix").join("patches.json")
    }

    fn empty(nixpkgs_rev: &str) -> Self {
        Self {
            nixpkgs_rev: nixpkgs_rev.to_string(),
            packages: BTreeMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level: plan patches for a set of installed packages
// ---------------------------------------------------------------------------

/// Per-binary patch plan produced by `plan_patches`.
#[derive(Debug)]
pub struct BinaryPatchPlan {
    /// Absolute path to the binary.
    pub binary: PathBuf,
    /// Only the rpath entries this binary actually needs.
    pub rpaths: Vec<PathBuf>,
    /// Whether this binary has origin-resolvable deps (needs $ORIGIN in rpath).
    pub needs_origin: bool,
    /// Resolution details (for manifest).
    pub resolved: ResolvedBinary,
}

/// A group of native binaries belonging to a single package.
#[derive(Debug)]
pub struct PackageBinaries {
    /// Normalized package name (e.g. "numpy").
    pub name: String,
    /// Package version (e.g. "2.4.6").
    pub version: String,
    /// Absolute paths to native binaries in this package.
    pub binaries: Vec<PathBuf>,
}

/// Analyze installed packages and produce per-binary patch plans + manifest update.
///
/// This is the main entry point called from `post_install_patch`.
///
/// `packages` groups binaries by package (name + version + binary paths).
/// `patcher` is the path to patchelf (Linux) or install_name_tool (Darwin).
/// `rpath_by_attr` maps nix attr names to resolved `/nix/store/.../lib` paths.
pub fn plan_patches(
    site_packages: &Path,
    packages: &[PackageBinaries],
    patcher: &Path,
    is_darwin: bool,
    rpath_by_attr: &HashMap<String, PathBuf>,
    nixpkgs_rev: &str,
) -> anyhow::Result<(Vec<BinaryPatchPlan>, PatchManifest)> {
    let soname_map = SonameMap::load_embedded()?;
    let platform_map = soname_map.for_platform(is_darwin);

    // Find venv root: site_packages is <venv>/lib/pythonX.Y/site-packages
    let venv_root = site_packages
        .parent() // pythonX.Y
        .and_then(|p| p.parent()) // lib
        .and_then(|p| p.parent()) // venv
        .unwrap_or(site_packages);

    let mut manifest = PatchManifest::load_or_default(venv_root);
    manifest.nixpkgs_rev = nixpkgs_rev.to_string();

    let mut all_plans = Vec::new();

    for pkg in packages {
        let mut pkg_patches = BTreeMap::new();

        for binary in &pkg.binaries {
            let needed = match read_needed_libs(binary, patcher, is_darwin, site_packages) {
                Ok(n) => n,
                Err(err) => {
                    warn!("Failed to read needed libs for {}: {err}", binary.display());
                    continue;
                }
            };

            if needed.needed.is_empty() && needed.origin_resolvable.is_empty() {
                debug!("  {} has no libs needing resolution", binary.display());
                continue;
            }

            let resolved = match resolve_binary(&needed, platform_map, rpath_by_attr) {
                Ok(r) => r,
                Err(err) => {
                    warn!("Failed to resolve {}: {err}", binary.display());
                    continue;
                }
            };

            let rpaths: Vec<PathBuf> = resolved.rpaths_added.iter().map(PathBuf::from).collect();

            // Relative path for manifest key
            let rel_path = binary
                .strip_prefix(site_packages)
                .unwrap_or(binary)
                .to_string_lossy()
                .to_string();

            pkg_patches.insert(rel_path, resolved.clone());

            all_plans.push(BinaryPatchPlan {
                binary: binary.clone(),
                rpaths,
                needs_origin: !needed.origin_resolvable.is_empty(),
                resolved,
            });
        }

        manifest.upsert_package(
            pkg.name.clone(),
            PackagePatchInfo {
                version: pkg.version.clone(),
                patches: pkg_patches,
            },
        );
    }

    Ok((all_plans, manifest))
}

// ---------------------------------------------------------------------------
// Soname map generation (offline tool for `just generate-soname-map`)
// ---------------------------------------------------------------------------

/// Generate the soname map by resolving all known lib attrs via nix,
/// then listing their .so/.dylib files.
///
/// Output goes to stdout as JSON for `data/soname-map.json`.
/// Must be run on each target platform (linux + darwin) and merged.
/// Generate the soname map for the current platform.
///
/// 1. Collects all lib attrs from default-libs.json + package-build-libs.json
/// 2. Resolves each attr to a Nix store path via `nix eval`
/// 3. Lists .so/.dylib files in each store path's `lib/` dir
/// 4. Maps soname filename → attr
///
/// Requires a nixpkgs source for resolution. Uses the project's flake.lock
/// or auto-resolves.
pub fn generate_soname_map_for_platform(
    source: &crate::nixpkgs::NixpkgsSource,
    is_darwin: bool,
) -> anyhow::Result<HashMap<String, String>> {
    let (_runtime_attrs, all_lib_attrs) = crate::nix_config::collect_all_lib_attrs()?;

    let pkgs_expr = crate::nixpkgs::nixpkgs_import_expr(source);

    // Build a nix expression that resolves all attrs to store paths
    let attr_resolve = |attr: &str| -> String {
        format!(
            "(builtins.toString (pkgs.lib.getAttrFromPath (pkgs.lib.splitString \".\" \"{attr}\") pkgs))"
        )
    };

    let mut soname_map = HashMap::new();

    for attr in &all_lib_attrs {
        let expr = format!("let pkgs = {pkgs_expr}; in {}", attr_resolve(attr));

        let mut cmd = crate::nix_command();
        cmd.arg("eval").arg("--raw");
        if crate::nixpkgs::requires_impure(source) {
            cmd.arg("--impure");
        }
        cmd.arg("--expr").arg(&expr);

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to resolve {attr}: {}", stderr.trim());
            continue;
        }

        let store_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let lib_dir = PathBuf::from(&store_path).join("lib");

        if !lib_dir.exists() {
            debug!("  {attr}: no lib/ directory at {}", lib_dir.display());
            continue;
        }

        let entries = match std::fs::read_dir(&lib_dir) {
            Ok(e) => e,
            Err(err) => {
                debug!("  {attr}: failed to read lib/ dir: {err}");
                continue;
            }
        };

        let ext = if is_darwin { ".dylib" } else { ".so" };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Only include shared library files
            if !name_str.contains(ext) {
                continue;
            }

            // Skip symlinks that point to the same directory (version aliases)
            // We want the canonical soname (e.g. libz.so.1, not libz.so)
            if entry.path().is_symlink() {
                // Include versioned symlinks (libz.so.1 → libz.so.1.3.1)
                // but skip unversioned dev links (libz.so → libz.so.1)
                if !is_darwin && name_str.ends_with(".so") {
                    continue;
                }
                if is_darwin && name_str.ends_with(".dylib") && !name_str.contains('.') {
                    continue;
                }
            }

            soname_map.insert(name_str.to_string(), attr.clone());
        }
    }

    Ok(soname_map)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_soname_map_loads() {
        let map = SonameMap::load_embedded().expect("Failed to load embedded soname map");
        #[cfg(target_os = "linux")]
        assert!(
            !map.linux.is_empty(),
            "linux soname map should not be empty"
        );
        #[cfg(target_os = "macos")]
        assert!(
            !map.darwin.is_empty(),
            "darwin soname map should not be empty"
        );
        let _ = &map;
    }

    #[test]
    fn test_soname_map_has_common_entries() {
        let map = SonameMap::load_embedded().unwrap();
        #[cfg(target_os = "linux")]
        assert!(
            map.linux.values().any(|v| v == "zlib"),
            "linux map should contain zlib"
        );
        #[cfg(target_os = "macos")]
        assert!(
            map.darwin.values().any(|v| v == "zlib"),
            "darwin map should contain zlib"
        );
        let _ = &map;
    }

    #[test]
    fn test_system_lib_detection_linux() {
        // Only kernel vDSOs are system libs
        assert!(is_system_lib("linux-vdso.so.1", false));
        assert!(is_system_lib("linux-gate.so.1", false));
        // Everything else should be resolved from Nix
        assert!(!is_system_lib("libc.so.6", false));
        assert!(!is_system_lib("libm.so.6", false));
        assert!(!is_system_lib("libpthread.so.0", false));
        assert!(!is_system_lib("ld-linux-x86-64.so.2", false));
        assert!(!is_system_lib("libstdc++.so.6", false));
        assert!(!is_system_lib("libgcc_s.so.1", false));
        assert!(!is_system_lib("libz.so.1", false));
        assert!(!is_system_lib("libssl.so.3", false));
    }

    #[test]
    fn test_system_lib_detection_darwin() {
        assert!(is_system_lib("/usr/lib/libSystem.B.dylib", true));
        assert!(is_system_lib("/usr/lib/libc++.1.dylib", true));
        assert!(is_system_lib(
            "/System/Library/Frameworks/Security.framework/foo",
            true
        ));
        assert!(!is_system_lib("/nix/store/xxx/lib/libz.1.dylib", true));
    }

    #[test]
    fn test_manifest_empty() {
        let m = PatchManifest::empty("abc123");
        assert_eq!(m.nixpkgs_rev, "abc123");
        assert!(m.packages.is_empty());
    }

    #[test]
    fn test_manifest_upsert() {
        let mut m = PatchManifest::empty("abc123");
        m.upsert_package(
            "numpy".to_string(),
            PackagePatchInfo {
                version: "2.4.6".to_string(),
                patches: BTreeMap::new(),
            },
        );
        assert_eq!(m.packages.len(), 1);
        assert_eq!(m.packages["numpy"].version, "2.4.6");

        // Upsert replaces
        m.upsert_package(
            "numpy".to_string(),
            PackagePatchInfo {
                version: "2.5.0".to_string(),
                patches: BTreeMap::new(),
            },
        );
        assert_eq!(m.packages.len(), 1);
        assert_eq!(m.packages["numpy"].version, "2.5.0");
    }

    #[test]
    fn test_manifest_roundtrip() {
        let mut m = PatchManifest::empty("abc123");
        let mut patches = BTreeMap::new();
        patches.insert(
            "numpy/core/_multiarray.so".to_string(),
            ResolvedBinary {
                needed: vec!["libz.so.1".to_string()],
                nix_libs: vec!["zlib".to_string()],
                rpaths_added: vec!["/nix/store/xxx-zlib/lib".to_string()],
            },
        );
        m.upsert_package(
            "numpy".to_string(),
            PackagePatchInfo {
                version: "2.4.6".to_string(),
                patches,
            },
        );

        let json = serde_json::to_string_pretty(&m).unwrap();
        let loaded: PatchManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.nixpkgs_rev, "abc123");
        assert_eq!(loaded.packages["numpy"].version, "2.4.6");
        assert_eq!(
            loaded.packages["numpy"].patches["numpy/core/_multiarray.so"].nix_libs,
            vec!["zlib"]
        );
    }

    #[test]
    fn test_manifest_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path().join(".venv");
        std::fs::create_dir_all(&venv).unwrap();

        let mut m = PatchManifest::empty("rev123");
        m.upsert_package(
            "pandas".to_string(),
            PackagePatchInfo {
                version: "2.3.0".to_string(),
                patches: BTreeMap::new(),
            },
        );

        m.save(&venv).unwrap();

        let loaded = PatchManifest::load_or_default(&venv);
        assert_eq!(loaded.nixpkgs_rev, "rev123");
        assert_eq!(loaded.packages["pandas"].version, "2.3.0");
    }
}
