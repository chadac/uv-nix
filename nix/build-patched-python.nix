# Standalone entry point for nix-build.
# Usage: nix-build nix/build-patched-python.nix --arg pythonSrc 'builtins.storePath "/nix/store/..."'
#
# This imports nixpkgs and calls patch-python.nix with all required dependencies.
{ pythonSrc
, nixpkgs ? <nixpkgs>
}:
let
  pkgs = import nixpkgs {};
  patchPython = pkgs.callPackage ./patch-python.nix { inherit pkgs; };
in
  patchPython { inherit pythonSrc; }
