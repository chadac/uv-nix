# default.nix
# Core build logic for uv-nix from source
#
# Usage:
#   uv = pkgs.callPackage ./. {};
{ lib
, stdenv
, rustPlatform
, installShellFiles
, rust-jemalloc-sys
, fetchFromGitHub
, doCheck ? false
}:

let
  uvMeta = builtins.fromJSON (builtins.readFile ./data/uv.json);

  uvSrc = fetchFromGitHub {
    owner = "astral-sh";
    repo = "uv";
    rev = uvMeta.version;
    hash = uvMeta.hash;
  };

  uvNixSrc = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./src
      ./data
      ./patches
    ];
  };

  # Create patched source combining uv + uv-nix
  patchedSrc = stdenv.mkDerivation {
    name = "uv-nix-src-${uvMeta.version}";
    src = uvSrc;
    phases = [ "unpackPhase" "patchPhase" "installPhase" ];

    postUnpack = ''
      mkdir -p $sourceRoot/crates/uv-nix/src
      mkdir -p $sourceRoot/crates/uv-nix/data
      cp ${uvNixSrc}/Cargo.toml $sourceRoot/crates/uv-nix/Cargo.toml
      cp -r ${uvNixSrc}/src/* $sourceRoot/crates/uv-nix/src/
      cp -r ${uvNixSrc}/data/* $sourceRoot/crates/uv-nix/data/
    '';

    # Add uv-nix to workspace BEFORE patches (which reference it)
    prePatch = ''
      # Insert uv-nix into [workspace.dependencies] after uv-normalize (version-agnostic)
      sed -i '/^uv-normalize = /a uv-nix = { version = "0.0.1", path = "crates/uv-nix" }' Cargo.toml
    '';
    patches = [
      "${uvNixSrc}/patches/01-uv-cli-nix-commands.patch"
      "${uvNixSrc}/patches/02-uv-crate-nix-dispatch.patch"
      "${uvNixSrc}/patches/03-uv-python-nix-hook.patch"
      "${uvNixSrc}/patches/04-uv-dispatch-nix-build-env.patch"
    ];

    installPhase = ''
      cp -r . $out
    '';
  };

in rustPlatform.buildRustPackage {
  pname = "uv";
  version = "${uvMeta.version}-nix";

  src = patchedSrc;

  cargoLock = {
    lockFile = "${patchedSrc}/Cargo.lock";
  };

  buildInputs = [
    rust-jemalloc-sys
  ];

  nativeBuildInputs = [
    installShellFiles
  ];

  cargoBuildFlags = [
    "--package"
    "uv"
  ];

  inherit doCheck;

  meta = with lib; {
    description = "uv Python package manager with Nix integration";
    homepage = "https://github.com/chadac/uv-nix";
    license = with licenses; [ asl20 mit ];
    mainProgram = "uv";
  };
}
