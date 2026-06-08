//! Nix expression generation from patch manifests.
//!
//! Reads `.venv/share/uv-nix/patches.json` (written by post-install patching)
//! and generates Nix expressions for use with uv2nix.
//!
//! Two output modes:
//! - Full `package.nix`: complete uv2nix derivation with workspace, overlays, and native lib overrides
//! - Overlay-only (`--overlay-only`): just the override overlay for native library dependencies

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use owo_colors::OwoColorize;

use crate::nix_config::{PACKAGE_BUILD_LIBS_JSON, PackageBuildEntry};
use crate::soname::PatchManifest;

/// Options for `uv nix gen`.
#[derive(Debug, Clone)]
pub struct GenOptions {
    /// Path to the virtual environment (default: .venv).
    pub venv: std::path::PathBuf,
    /// Output file path (default: stdout).
    pub output: Option<std::path::PathBuf>,
    /// Only generate the override overlay, not the full package.nix.
    pub overlay_only: bool,
    /// Prefer wheels over sdists (default: true).
    pub prefer_wheels: bool,
}

/// Per-package native library requirements, derived from patch manifest and curated data.
#[derive(Debug, Clone)]
struct PackageLibs {
    /// Normalized package name (e.g., "numpy").
    name: String,
    /// Nixpkgs attrs for buildInputs (ELF NEEDED + curated libs).
    build_attrs: BTreeSet<String>,
    /// Nixpkgs attrs for propagatedBuildInputs (runtime-only ctypes/dlopen libs).
    runtime_attrs: BTreeSet<String>,
    /// Python build system packages for nativeBuildInputs (e.g., setuptools).
    /// Referenced as `final.<pkg>` in the Python package set, not `pkgs.<pkg>`.
    build_system_attrs: BTreeSet<String>,
}

/// Attrs that are implicitly available in any Nix build environment and don't
/// need explicit `buildInputs` overrides (they'd just be noise).
const SKIP_ATTRS: &[&str] = &["glibc", "stdenv.cc.cc.lib", "util-linux.out"];

/// Check if a string is a valid Nix identifier (can appear unquoted in attr paths).
///
/// Nix identifiers: `[a-zA-Z_][a-zA-Z0-9_'-]*`
fn is_valid_nix_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '\'')
}

/// Render a nixpkgs attr path as `pkgs.foo.bar`, quoting components that aren't
/// valid Nix identifiers with `${"..."}`.
fn render_nix_attr(attr: &str) -> String {
    let parts: Vec<&str> = attr.split('.').collect();
    let mut out = String::from("pkgs");
    for part in parts {
        if is_valid_nix_ident(part) {
            out.push('.');
            out.push_str(part);
        } else {
            out.push_str(".${ \"");
            out.push_str(part);
            out.push_str("\" }");
        }
    }
    out
}

fn should_include_attr(attr: &str) -> bool {
    !attr.starts_with('_') && !SKIP_ATTRS.contains(&attr)
}

/// Platform-filtered libs from a curated package entry.
fn curated_libs(entry: &PackageBuildEntry) -> impl Iterator<Item = &String> {
    let platform_libs: &[String] = if cfg!(target_os = "macos") {
        &entry.libs_darwin
    } else {
        &entry.libs_linux
    };
    entry.libs.iter().chain(platform_libs.iter())
}

/// Load the patch manifest from a venv and extract per-package nixpkgs dependencies.
///
/// Merges ELF-resolved libs from the patch manifest with curated entries from
/// `package-build-libs.json`. Returns a sorted list of packages that have
/// native library requirements.
fn collect_package_libs(manifest: &PatchManifest) -> Vec<PackageLibs> {
    let package_map: HashMap<String, PackageBuildEntry> =
        serde_json::from_str(PACKAGE_BUILD_LIBS_JSON).unwrap_or_default();

    let mut result = Vec::new();

    for (pkg_name, pkg_info) in &manifest.packages {
        let mut build_attrs = BTreeSet::new();
        let mut runtime_attrs = BTreeSet::new();
        let mut build_system_attrs = BTreeSet::new();

        // ELF-resolved libs from patch manifest → buildInputs
        for resolved in pkg_info.patches.values() {
            for attr in &resolved.nix_libs {
                if should_include_attr(attr) {
                    build_attrs.insert(attr.clone());
                }
            }
        }

        // Curated libs from package-build-libs.json
        if let Some(entry) = package_map.get(pkg_name) {
            for attr in curated_libs(entry) {
                if should_include_attr(attr) {
                    build_attrs.insert(attr.clone());
                }
            }
            for attr in &entry.runtime_libs {
                if should_include_attr(attr) {
                    runtime_attrs.insert(attr.clone());
                }
            }
            for attr in &entry.build_system {
                build_system_attrs.insert(attr.clone());
            }
        }

        if !build_attrs.is_empty() || !runtime_attrs.is_empty() || !build_system_attrs.is_empty() {
            result.push(PackageLibs {
                name: pkg_name.clone(),
                build_attrs,
                runtime_attrs,
                build_system_attrs,
            });
        }
    }

    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// Generate the native library override overlay as a Nix expression string.
///
/// Output format:
/// ```nix
/// # Auto-generated by `uv nix gen`. Do not edit.
/// # Native library overrides for uv2nix packages.
/// { pkgs, lib, ... }:
/// final: prev: {
///   numpy = prev.numpy.overrideAttrs (old: {
///     buildInputs = (old.buildInputs or []) ++ [
///       pkgs.openblas
///     ];
///   });
/// }
/// ```
fn render_overlay(packages: &[PackageLibs]) -> String {
    let mut out = String::new();
    out.push_str("# Auto-generated by `uv nix gen`. Do not edit.\n");
    out.push_str("# Native library overrides for uv2nix packages.\n");
    out.push_str("{ pkgs, lib, ... }:\n");
    out.push_str("final: prev: {\n");

    for pkg in packages {
        writeln!(
            out,
            "  {} = prev.{}.overrideAttrs (old: {{",
            pkg.name, pkg.name
        )
        .unwrap();
        if !pkg.build_attrs.is_empty() {
            out.push_str("    buildInputs = (old.buildInputs or []) ++ [\n");
            for attr in &pkg.build_attrs {
                writeln!(out, "      {}", render_nix_attr(attr)).unwrap();
            }
            out.push_str("    ];\n");
        }
        if !pkg.runtime_attrs.is_empty() {
            out.push_str("    propagatedBuildInputs = (old.propagatedBuildInputs or []) ++ [\n");
            for attr in &pkg.runtime_attrs {
                writeln!(out, "      {}", render_nix_attr(attr)).unwrap();
            }
            out.push_str("    ];\n");
        }
        if !pkg.build_system_attrs.is_empty() {
            out.push_str("    nativeBuildInputs = (old.nativeBuildInputs or []) ++ [\n");
            for attr in &pkg.build_system_attrs {
                writeln!(out, "      final.{attr}").unwrap();
            }
            out.push_str("    ];\n");
        }
        out.push_str("  });\n");
    }

    out.push_str("}\n");
    out
}

/// Generate a complete `package.nix` that includes the uv2nix workspace setup
/// and the native library overrides.
///
/// Output format:
/// ```nix
/// # Auto-generated by `uv nix gen`. Do not edit.
/// {
///   pkgs,
///   lib,
///   uv2nix,
///   pyproject-nix,
///   pyproject-build-systems,
///   python ? pkgs.python312,
///   sourcePreference ? "wheel",
///   ...
/// }:
/// let
///   workspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = ./.; };
///   overlay = workspace.mkPyprojectOverlay { inherit sourcePreference; };
///
///   # Native library overrides from uv-nix patch analysis
///   uvNixOverrides = final: prev: { ... };
///
///   pythonSet =
///     (pkgs.callPackage pyproject-nix.build.packages { inherit python; }).overrideScope
///       (lib.composeManyExtensions [
///         pyproject-build-systems.overlays.default
///         overlay
///         uvNixOverrides
///       ]);
/// in {
///   inherit pythonSet;
///   venv = pythonSet.mkVirtualEnv "app-env" workspace.deps.default;
/// }
/// ```
fn render_package_nix(packages: &[PackageLibs], prefer_wheels: bool) -> String {
    let mut out = String::new();
    out.push_str("# Auto-generated by `uv nix gen`. Do not edit.\n");
    let source_pref = if prefer_wheels { "wheel" } else { "sdist" };
    out.push_str("{\n");
    out.push_str("  pkgs,\n");
    out.push_str("  lib,\n");
    out.push_str("  uv2nix,\n");
    out.push_str("  pyproject-nix,\n");
    out.push_str("  pyproject-build-systems,\n");
    out.push_str("  python ? pkgs.python312,\n");
    writeln!(out, "  sourcePreference ? \"{source_pref}\",").unwrap();
    out.push_str("  ...\n");
    out.push_str("}:\n");
    out.push_str("let\n");
    out.push_str("  workspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = ./.; };\n");
    out.push_str("  overlay = workspace.mkPyprojectOverlay { inherit sourcePreference; };\n");
    out.push('\n');

    // Inline the native lib overlay
    out.push_str("  # Native library overrides from uv-nix patch analysis\n");
    out.push_str("  uvNixOverrides = final: prev: {\n");
    for pkg in packages {
        writeln!(
            out,
            "    {} = prev.{}.overrideAttrs (old: {{",
            pkg.name, pkg.name
        )
        .unwrap();
        if !pkg.build_attrs.is_empty() {
            out.push_str("      buildInputs = (old.buildInputs or []) ++ [\n");
            for attr in &pkg.build_attrs {
                writeln!(out, "        {}", render_nix_attr(attr)).unwrap();
            }
            out.push_str("      ];\n");
        }
        if !pkg.runtime_attrs.is_empty() {
            out.push_str("      propagatedBuildInputs = (old.propagatedBuildInputs or []) ++ [\n");
            for attr in &pkg.runtime_attrs {
                writeln!(out, "        {}", render_nix_attr(attr)).unwrap();
            }
            out.push_str("      ];\n");
        }
        if !pkg.build_system_attrs.is_empty() {
            out.push_str("      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [\n");
            for attr in &pkg.build_system_attrs {
                writeln!(out, "        final.{attr}").unwrap();
            }
            out.push_str("      ];\n");
        }
        out.push_str("    });\n");
    }
    out.push_str("  };\n");
    out.push('\n');

    out.push_str("  pythonSet =\n");
    out.push_str(
        "    (pkgs.callPackage pyproject-nix.build.packages { inherit python; }).overrideScope\n",
    );
    out.push_str("      (lib.composeManyExtensions [\n");
    out.push_str("        pyproject-build-systems.overlays.default\n");
    out.push_str("        overlay\n");
    out.push_str("        uvNixOverrides\n");
    out.push_str("      ]);\n");
    out.push_str("in {\n");
    out.push_str("  inherit pythonSet;\n");
    out.push_str("  venv = pythonSet.mkVirtualEnv \"app-env\" workspace.deps.default;\n");
    out.push_str("}\n");
    out
}

/// Entry point for `uv nix gen`.
pub fn nix_gen<O: Write, E: Write>(
    out: &mut crate::cli::CliOutput<'_, O, E>,
    opts: GenOptions,
) -> anyhow::Result<()> {
    let venv_path = opts.venv.canonicalize().unwrap_or(opts.venv.clone());

    if !venv_path.exists() {
        anyhow::bail!("Virtual environment not found: {}", venv_path.display());
    }

    // Load the patch manifest
    let manifest = PatchManifest::load_or_default(&venv_path);
    if manifest.packages.is_empty() {
        anyhow::bail!(
            "No patch data found. Run `uv sync` first to install packages and generate patch data."
        );
    }

    // Extract per-package native lib requirements
    let packages = collect_package_libs(&manifest);

    // Generate the Nix expression
    let nix_expr = if opts.overlay_only {
        render_overlay(&packages)
    } else {
        render_package_nix(&packages, opts.prefer_wheels)
    };

    // Write output
    if let Some(ref output_path) = opts.output {
        std::fs::write(output_path, &nix_expr)?;
        let _ = writeln!(
            out.stderr,
            "{} {}",
            "Generated".green().bold(),
            output_path.display()
        );
    } else {
        let _ = write!(out.stdout, "{nix_expr}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soname::{PackagePatchInfo, ResolvedBinary};
    use std::collections::BTreeMap;

    type PkgSpec<'a> = (&'a str, &'a str, Vec<(&'a str, Vec<&'a str>)>);

    /// Helper: build a PatchManifest from a list of (pkg_name, version, [(binary, [nix_libs])]).
    fn make_manifest(packages: Vec<PkgSpec<'_>>) -> PatchManifest {
        let mut pkgs = BTreeMap::new();
        for (name, version, binaries) in packages {
            let mut patches = BTreeMap::new();
            for (bin_path, libs) in binaries {
                patches.insert(
                    bin_path.to_string(),
                    ResolvedBinary {
                        needed: vec![],
                        nix_libs: libs.into_iter().map(|s| s.to_string()).collect(),
                        rpaths_added: vec![],
                    },
                );
            }
            pkgs.insert(
                name.to_string(),
                PackagePatchInfo {
                    version: version.to_string(),
                    patches,
                },
            );
        }
        PatchManifest {
            nixpkgs_rev: "abc123".to_string(),
            packages: pkgs,
        }
    }

    fn make_pkg(name: &str, build: &[&str], runtime: &[&str]) -> PackageLibs {
        PackageLibs {
            name: name.into(),
            build_attrs: build.iter().map(|s| s.to_string()).collect(),
            runtime_attrs: runtime.iter().map(|s| s.to_string()).collect(),
            build_system_attrs: BTreeSet::new(),
        }
    }

    fn make_pkg_full(
        name: &str,
        build: &[&str],
        runtime: &[&str],
        build_system: &[&str],
    ) -> PackageLibs {
        PackageLibs {
            name: name.into(),
            build_attrs: build.iter().map(|s| s.to_string()).collect(),
            runtime_attrs: runtime.iter().map(|s| s.to_string()).collect(),
            build_system_attrs: build_system.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn collect_package_libs_basic() {
        let manifest = make_manifest(vec![
            (
                "numpy",
                "1.26.0",
                vec![("numpy/core/_multiarray.so", vec!["openblas", "zlib"])],
            ),
            (
                "pandas",
                "2.1.0",
                vec![("pandas/_libs/lib.so", vec!["zlib"])],
            ),
        ]);
        let libs = collect_package_libs(&manifest);
        // numpy and pandas both appear (may have curated libs merged in too)
        let numpy = libs.iter().find(|p| p.name == "numpy").unwrap();
        assert!(numpy.build_attrs.contains("openblas"));
        assert!(numpy.build_attrs.contains("zlib"));
        let pandas = libs.iter().find(|p| p.name == "pandas").unwrap();
        assert!(pandas.build_attrs.contains("zlib"));
    }

    #[test]
    fn collect_package_libs_deduplicates() {
        let manifest = make_manifest(vec![(
            "numpy",
            "1.26.0",
            vec![
                ("numpy/core/_multiarray.so", vec!["openblas", "zlib"]),
                ("numpy/linalg/_umath_linalg.so", vec!["openblas"]),
            ],
        )]);
        let libs = collect_package_libs(&manifest);
        let numpy = libs.iter().find(|p| p.name == "numpy").unwrap();
        assert!(numpy.build_attrs.contains("openblas"));
        assert!(numpy.build_attrs.contains("zlib"));
    }

    #[test]
    fn collect_package_libs_filters_underscore_attrs() {
        let manifest = make_manifest(vec![(
            "foo",
            "1.0.0",
            vec![("foo/bar.so", vec!["zlib", "_internal_thing"])],
        )]);
        let libs = collect_package_libs(&manifest);
        let foo = libs.iter().find(|p| p.name == "foo").unwrap();
        assert!(foo.build_attrs.contains("zlib"));
        assert!(!foo.build_attrs.contains("_internal_thing"));
    }

    #[test]
    fn collect_package_libs_filters_system_attrs() {
        let manifest = make_manifest(vec![(
            "numpy",
            "1.26.0",
            vec![(
                "numpy/core/_multiarray.so",
                vec!["openblas", "glibc", "stdenv.cc.cc.lib", "util-linux.out"],
            )],
        )]);
        let libs = collect_package_libs(&manifest);
        let numpy = libs.iter().find(|p| p.name == "numpy").unwrap();
        assert!(numpy.build_attrs.contains("openblas"));
        assert!(!numpy.build_attrs.contains("glibc"));
        assert!(!numpy.build_attrs.contains("stdenv.cc.cc.lib"));
        assert!(!numpy.build_attrs.contains("util-linux.out"));
    }

    #[test]
    fn collect_package_libs_skips_package_with_only_system_attrs() {
        let manifest = make_manifest(vec![(
            "foo",
            "1.0.0",
            vec![("foo/bar.so", vec!["glibc", "stdenv.cc.cc.lib"])],
        )]);
        let libs = collect_package_libs(&manifest);
        assert!(libs.iter().find(|p| p.name == "foo").is_none());
    }

    #[test]
    fn collect_package_libs_skips_empty() {
        let manifest = make_manifest(vec![
            (
                "numpy",
                "1.26.0",
                vec![("numpy/core/_multiarray.so", vec!["openblas"])],
            ),
            ("pure-python", "1.0.0", vec![]),
        ]);
        let libs = collect_package_libs(&manifest);
        assert!(libs.iter().find(|p| p.name == "numpy").is_some());
        assert!(libs.iter().find(|p| p.name == "pure-python").is_none());
    }

    #[test]
    fn collect_package_libs_sorted() {
        let manifest = make_manifest(vec![
            ("zlib-wrapper", "1.0.0", vec![("z.so", vec!["zlib"])]),
            ("aaa-lib", "1.0.0", vec![("a.so", vec!["openssl"])]),
        ]);
        let libs = collect_package_libs(&manifest);
        let names: Vec<&str> = libs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn collect_package_libs_merges_curated_libs() {
        // matplotlib has curated libs in package-build-libs.json
        let manifest = make_manifest(vec![(
            "matplotlib",
            "3.8.0",
            vec![("matplotlib/_c_internal.so", vec!["zlib"])],
        )]);
        let libs = collect_package_libs(&manifest);
        let mpl = libs.iter().find(|p| p.name == "matplotlib").unwrap();
        // ELF-resolved
        assert!(mpl.build_attrs.contains("zlib"));
        // Curated libs from package-build-libs.json
        assert!(mpl.build_attrs.contains("freetype"));
        assert!(mpl.build_attrs.contains("libpng"));
        // Runtime-libs from package-build-libs.json
        assert!(mpl.runtime_attrs.contains("fontconfig"));
    }

    #[test]
    fn collect_package_libs_runtime_only_package() {
        // pysodium has runtime-libs + build-system, no ELF libs
        let manifest = make_manifest(vec![("pysodium", "0.7.18", vec![])]);
        let libs = collect_package_libs(&manifest);
        let ps = libs.iter().find(|p| p.name == "pysodium").unwrap();
        assert!(ps.build_attrs.is_empty());
        assert!(ps.runtime_attrs.contains("libsodium"));
        assert!(ps.build_system_attrs.contains("setuptools"));
    }

    #[test]
    fn render_overlay_single_package() {
        let packages = vec![make_pkg("numpy", &["openblas"], &[])];
        let output = render_overlay(&packages);
        assert!(output.contains("Auto-generated"));
        assert!(output.contains("final: prev:"));
        assert!(output.contains("numpy = prev.numpy.overrideAttrs"));
        assert!(output.contains("buildInputs"));
        assert!(output.contains("pkgs.openblas"));
        assert!(!output.contains("propagatedBuildInputs"));
    }

    #[test]
    fn render_overlay_with_runtime_attrs() {
        let packages = vec![make_pkg("matplotlib", &["freetype"], &["fontconfig"])];
        let output = render_overlay(&packages);
        assert!(output.contains("buildInputs"));
        assert!(output.contains("pkgs.freetype"));
        assert!(output.contains("propagatedBuildInputs"));
        assert!(output.contains("pkgs.fontconfig"));
    }

    #[test]
    fn render_overlay_runtime_only() {
        let packages = vec![make_pkg("pysodium", &[], &["libsodium"])];
        let output = render_overlay(&packages);
        assert!(!output.contains("buildInputs ="));
        assert!(output.contains("propagatedBuildInputs"));
        assert!(output.contains("pkgs.libsodium"));
    }

    #[test]
    fn render_overlay_with_build_system() {
        let packages = vec![make_pkg_full(
            "pysodium",
            &[],
            &["libsodium"],
            &["setuptools"],
        )];
        let output = render_overlay(&packages);
        assert!(output.contains("propagatedBuildInputs"));
        assert!(output.contains("pkgs.libsodium"));
        assert!(output.contains("nativeBuildInputs"));
        assert!(output.contains("final.setuptools"));
        assert!(!output.contains("pkgs.setuptools"));
    }

    #[test]
    fn render_overlay_build_system_only() {
        let packages = vec![make_pkg_full(
            "legacy-pkg",
            &[],
            &[],
            &["setuptools", "wheel"],
        )];
        let output = render_overlay(&packages);
        assert!(!output.contains("buildInputs ="));
        assert!(!output.contains("propagatedBuildInputs"));
        assert!(output.contains("nativeBuildInputs"));
        assert!(output.contains("final.setuptools"));
        assert!(output.contains("final.wheel"));
    }

    #[test]
    fn render_overlay_multiple_packages() {
        let packages = vec![
            make_pkg("numpy", &["openblas", "zlib"], &[]),
            make_pkg("pandas", &["zlib"], &[]),
        ];
        let output = render_overlay(&packages);
        assert!(output.contains("numpy = prev.numpy.overrideAttrs"));
        assert!(output.contains("pkgs.openblas"));
        assert!(output.contains("pkgs.zlib"));
        assert!(output.contains("pandas = prev.pandas.overrideAttrs"));
    }

    #[test]
    fn render_overlay_empty() {
        let output = render_overlay(&[]);
        assert!(output.contains("final: prev:"));
    }

    #[test]
    fn render_package_nix_contains_uv2nix_boilerplate() {
        let packages = vec![make_pkg("numpy", &["openblas"], &[])];
        let output = render_package_nix(&packages, true);
        assert!(output.contains("uv2nix"));
        assert!(output.contains("pyproject-nix"));
        assert!(output.contains("pyproject-build-systems"));
        assert!(output.contains("loadWorkspace"));
        assert!(output.contains("mkPyprojectOverlay"));
        assert!(output.contains("sourcePreference ? \"wheel\""));
        assert!(output.contains("inherit sourcePreference"));
        assert!(output.contains("composeManyExtensions"));
        assert!(output.contains("mkVirtualEnv"));
        assert!(output.contains("numpy = prev.numpy.overrideAttrs"));
    }

    #[test]
    fn render_package_nix_with_propagated() {
        let packages = vec![make_pkg("matplotlib", &["freetype"], &["fontconfig"])];
        let output = render_package_nix(&packages, true);
        assert!(output.contains("buildInputs"));
        assert!(output.contains("pkgs.freetype"));
        assert!(output.contains("propagatedBuildInputs"));
        assert!(output.contains("pkgs.fontconfig"));
    }

    #[test]
    fn render_package_nix_with_build_system() {
        let packages = vec![make_pkg_full(
            "pysodium",
            &[],
            &["libsodium"],
            &["setuptools"],
        )];
        let output = render_package_nix(&packages, true);
        assert!(output.contains("propagatedBuildInputs"));
        assert!(output.contains("pkgs.libsodium"));
        assert!(output.contains("nativeBuildInputs"));
        assert!(output.contains("final.setuptools"));
        assert!(!output.contains("pkgs.setuptools"));
    }

    #[test]
    fn render_package_nix_prefer_sdist() {
        let output = render_package_nix(&[], false);
        assert!(output.contains("sourcePreference ? \"sdist\""));
    }

    #[test]
    fn render_package_nix_empty_packages() {
        let output = render_package_nix(&[], true);
        assert!(output.contains("uv2nix"));
        assert!(output.contains("mkVirtualEnv"));
    }

    #[test]
    fn is_valid_nix_ident_basic() {
        assert!(is_valid_nix_ident("openssl"));
        assert!(is_valid_nix_ident("zlib"));
        assert!(is_valid_nix_ident("libjpeg_turbo"));
        assert!(is_valid_nix_ident("arrow-cpp"));
        assert!(is_valid_nix_ident("_private"));
        assert!(is_valid_nix_ident("boost188"));
    }

    #[test]
    fn is_valid_nix_ident_invalid() {
        assert!(!is_valid_nix_ident(""));
        assert!(!is_valid_nix_ident("123abc"));
        assert!(!is_valid_nix_ident("foo bar"));
        assert!(!is_valid_nix_ident("foo.bar"));
    }

    #[test]
    fn render_nix_attr_simple() {
        assert_eq!(render_nix_attr("openssl"), "pkgs.openssl");
        assert_eq!(render_nix_attr("arrow-cpp"), "pkgs.arrow-cpp");
    }

    #[test]
    fn render_nix_attr_dotted_path() {
        assert_eq!(render_nix_attr("stdenv.cc.cc.lib"), "pkgs.stdenv.cc.cc.lib");
        assert_eq!(render_nix_attr("gfortran.cc.lib"), "pkgs.gfortran.cc.lib");
    }

    #[test]
    fn render_nix_attr_needs_quoting() {
        assert_eq!(render_nix_attr("foo.123bar"), "pkgs.foo.${ \"123bar\" }");
    }

    #[test]
    fn nix_gen_missing_venv_errors() {
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut out = crate::cli::CliOutput {
            stdout: &mut stdout,
            stderr: &mut stderr,
        };
        let opts = GenOptions {
            venv: "/nonexistent/path/.venv".into(),
            output: None,
            overlay_only: false,
            prefer_wheels: true,
        };
        let result = nix_gen(&mut out, opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn nix_gen_empty_manifest_errors() {
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path().join(".venv");
        std::fs::create_dir_all(venv.join("share/uv-nix")).unwrap();
        std::fs::write(
            venv.join("share/uv-nix/patches.json"),
            r#"{"nixpkgs_rev":"abc","packages":{}}"#,
        )
        .unwrap();

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut out = crate::cli::CliOutput {
            stdout: &mut stdout,
            stderr: &mut stderr,
        };
        let opts = GenOptions {
            venv,
            output: None,
            overlay_only: false,
            prefer_wheels: true,
        };
        let result = nix_gen(&mut out, opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No patch data"));
    }
}
