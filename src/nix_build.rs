use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::debug;

use crate::nixpkgs::{self, NixpkgsSource};

/// The Python ctypes hook, embedded at compile time.
const CTYPES_HOOK_PY: &str = include_str!("../data/ctypes_hook.py");

/// Default runtime library list, embedded at compile time.
const DEFAULT_LIBS_JSON: &str = include_str!("../data/default-libs.json");

/// Per-Python-package build dependency registry, embedded at compile time.
const PACKAGE_BUILD_LIBS_JSON: &str = include_str!("../data/package-build-libs.json");

/// Import a directory into the Nix store, returning its store path.
pub fn nix_store_add(dir: &Path) -> anyhow::Result<PathBuf> {
    debug!("Adding to nix store: {}", dir.display());

    let output = Command::new("nix-store")
        .arg("--add")
        .arg(dir)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix-store --add failed: {}", stderr.trim());
    }

    let store_path = String::from_utf8(output.stdout)?
        .trim()
        .to_string();
    debug!("Added to nix store: {store_path}");
    Ok(PathBuf::from(store_path))
}

/// Build a patched Python derivation using an inline Nix expression.
///
/// The expression embeds the default library list and ctypes hook, so no
/// external `.nix` files are needed at runtime.
fn nix_build_patched(
    store_path: &Path,
    source: &NixpkgsSource,
) -> anyhow::Result<PathBuf> {
    debug!(
        "Building patched Python from {} via inline nix expression",
        store_path.display(),
    );

    let pkgs_expr = nixpkgs::nixpkgs_import_expr(source);
    let store_path_str = store_path.to_string_lossy();

    // Collect all unique attrs (runtime + package build deps) so the patched
    // Python has all libs available via RPATH and the ctypes hook.
    let runtime_attrs: Vec<String> = serde_json::from_str(DEFAULT_LIBS_JSON)?;
    let package_map: std::collections::HashMap<String, Vec<String>> =
        serde_json::from_str(PACKAGE_BUILD_LIBS_JSON)?;

    let mut all_attrs = std::collections::BTreeSet::new();
    for attr in &runtime_attrs {
        all_attrs.insert(attr.clone());
    }
    for attrs in package_map.values() {
        for attr in attrs {
            all_attrs.insert(attr.clone());
        }
    }

    let attr_exprs: Vec<String> = all_attrs
        .iter()
        .map(|attr| {
            format!(
                "(pkgs.lib.getAttrFromPath (pkgs.lib.splitString \".\" \"{attr}\") pkgs)",
            )
        })
        .collect();
    let libs_list = attr_exprs.join("\n    ");

    // Escape the ctypes hook content for embedding in a Nix string.
    // Use two single-quotes (Nix '' strings) with proper escaping.
    let escaped_hook = CTYPES_HOOK_PY
        .replace("''", "'''")
        .replace("${", "''${");

    let expr = format!(
        r#"let
  pkgs = {pkgs_expr};
  defaultLibs = [
    {libs_list}
  ];
  interpreter = pkgs.lib.strings.trim pkgs.stdenv.cc.bintools.dynamicLinker;
  rpath = pkgs.lib.makeLibraryPath defaultLibs;
  libPaths = builtins.concatStringsSep "\n" (map (p: "${{pkgs.lib.getLib p}}/lib") defaultLibs);
  ctypesHook = builtins.toFile "ctypes_hook.py" ''{escaped_hook}'';
in pkgs.stdenvNoCC.mkDerivation {{
  name = "patched-python";
  src = builtins.storePath "{store_path_str}";
  nativeBuildInputs = [ pkgs.patchelf ];
  dontConfigure = true;
  dontBuild = true;
  dontUnpack = true;
  installPhase = ''
    cp -r $src $out && chmod -R u+w $out
    find $out -type f | while read f; do
      if head -c 4 "$f" | grep -qP '^\x7fELF'; then
        patchelf --set-rpath "${{rpath}}" "$f" 2>/dev/null || true
        patchelf --set-interpreter "${{interpreter}}" "$f" 2>/dev/null || true
      fi
    done
    for sp in $out/lib/python*/site-packages; do
      [ -d "$sp" ] && cp ${{ctypesHook}} "$sp/_uv_nix_ctypes_hook.py" \
        && echo "import _uv_nix_ctypes_hook" > "$sp/uv-nix.pth" \
        && echo "${{libPaths}}" > "$sp/_uv_nix_libs.conf"
    done
  '';
}}"#
    );

    let output = crate::nix_command()
        .arg("build")
        .arg("--no-link")
        .arg("--print-out-paths")
        .arg("--impure")
        .arg("--expr")
        .arg(&expr)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix build failed: {}", stderr.trim());
    }

    let result_path = String::from_utf8(output.stdout)?
        .trim()
        .to_string();
    debug!("Built patched Python: {result_path}");
    Ok(PathBuf::from(result_path))
}

/// Full flow: add to store, build patched derivation, return store path.
pub fn nix_patch_python(
    python_install_dir: &Path,
    source: &NixpkgsSource,
) -> anyhow::Result<PathBuf> {
    crate::status("Building", "patched Python derivation (nix)");
    let store_path = nix_store_add(python_install_dir)?;
    let result = nix_build_patched(&store_path, source)?;
    crate::status("Built", &format!("{}", result.display()));
    Ok(result)
}
