# nix/lib.nix
# Resolves data/default-libs.json into actual nixpkgs derivations.
# Used by uv-nix-binaries.nix to provide buildInputs (autoPatchelfHook)
# for pre-built binary packages.
{ lib, pkgs }:

let
  data = builtins.fromJSON (builtins.readFile ../data/default-libs.json);

  # Resolve a nixpkgs attribute path string to the actual package
  resolveAttr = attr:
    lib.getAttrFromPath (lib.splitString "." attr) pkgs;

  # Shared libraries (all platforms)
  sharedLibs = map resolveAttr data.shared;

  # Platform-specific libraries
  linuxLibs = map resolveAttr data.linux;
  darwinLibs = map resolveAttr data.darwin;

  # Combined platform-aware library list
  platformLibs =
    if pkgs.stdenv.isLinux then linuxLibs
    else if pkgs.stdenv.isDarwin then darwinLibs
    else [];

  defaultLibs = sharedLibs ++ platformLibs;

in {
  inherit resolveAttr defaultLibs sharedLibs platformLibs linuxLibs darwinLibs;

  # For backwards compatibility
  inherit data;
}
