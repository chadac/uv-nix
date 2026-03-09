# nix/lib.nix
# Library helpers for uv-nix
#
# Provides platform-aware library resolution from default-libs.json
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
