# nix/bin.nix
# Pre-built uv-nix binary package from GitHub releases.
#
# Reads data/bin.json for version, release tag, and per-platform hashes.
# Updated automatically by the update-binary-registry workflow.
{ pkgs }:

let
  lib = pkgs.lib;
  system = pkgs.stdenv.hostPlatform.system;

  uvNixLib = import ./lib.nix { inherit lib pkgs; };

  meta = builtins.fromJSON (builtins.readFile ../data/bin.json);
  asset = meta.assets.${system} or null;
  url = "https://github.com/chadac/uv-nix/releases/download/${meta.releaseTag}/${asset.name}";

in if asset == null then null
else pkgs.stdenv.mkDerivation {
  pname = "uv-nix-bin";
  version = "${meta.version}-nix";

  src = builtins.fetchurl {
    inherit url;
    sha256 = asset.hash;
  };

  dontUnpack = true;

  nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [
    pkgs.autoPatchelfHook
  ];

  buildInputs = uvNixLib.defaultLibs ++ [
    pkgs.rust-jemalloc-sys
  ] ++ lib.optionals pkgs.stdenv.isLinux [
    pkgs.stdenv.cc.cc.lib
  ];

  installPhase = ''
    runHook preInstall
    install -Dm755 $src $out/bin/uv
    runHook postInstall
  '';

  meta = {
    description = "uv Python package manager with Nix integration (pre-built)";
    homepage = "https://github.com/chadac/uv-nix";
    mainProgram = "uv";
  };
}
