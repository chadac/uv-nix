{ lib, pkgs }:
let
  data = builtins.fromJSON (builtins.readFile ../data/default-libs.json);
  resolveAttr = attr:
    lib.getAttrFromPath (lib.splitString "." attr) pkgs;
  defaultLibs = map resolveAttr data;
in {
  inherit resolveAttr defaultLibs;
}
