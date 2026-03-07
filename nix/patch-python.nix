{ stdenvNoCC
, stdenv
, lib
, pkgs
, patchelf
}:

{ pythonSrc }:

let
  uvNixLib = import ./lib.nix { inherit lib pkgs; };
  interpreter = stdenv.cc.bintools.dynamicLinker;
  rpath = lib.makeLibraryPath uvNixLib.defaultLibs;
in
stdenvNoCC.mkDerivation {
  name = "patched-python";
  src = pythonSrc;

  nativeBuildInputs = [ patchelf ];

  dontConfigure = true;
  dontBuild = true;

  # pythonSrc is a store path (directory), so unpackPhase would try to
  # unpack it as a tarball. We skip it and handle copying in installPhase.
  dontUnpack = true;

  installPhase = ''
    runHook preInstall

    cp -r $src $out
    chmod -R u+w $out

    find $out -type f | while read f; do
      if head -c 4 "$f" | grep -qP '^\x7fELF'; then
        # Set RPATH first, then interpreter (order matters — patchelf bug #524)
        patchelf --set-rpath "${rpath}" "$f" 2>/dev/null || true
        patchelf --set-interpreter "${interpreter}" "$f" 2>/dev/null || true
      fi
    done

    # Install ctypes hook into site-packages so dlopen() finds Nix libraries
    for sp in $out/lib/python*/site-packages; do
      if [ -d "$sp" ]; then
        cp ${../data/ctypes_hook.py} "$sp/_uv_nix_ctypes_hook.py"
        echo "import _uv_nix_ctypes_hook" > "$sp/uv-nix.pth"
        printf '%s\n' ${lib.concatMapStringsSep " " (p: ''"${lib.getLib p}/lib"'') uvNixLib.defaultLibs} > "$sp/_uv_nix_libs.conf"
      fi
    done

    runHook postInstall
  '';
}
