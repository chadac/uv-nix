# default.nix
# Core build logic for uv-nix from source
#
# Usage:
#   buildUv = pkgs.callPackage ./. {};
#   uv = buildUv { uvSrc = ...; version = "0.10.8"; };
{ lib
, stdenv
, rustPlatform
, installShellFiles
, rust-jemalloc-sys
}:

{ uvSrc           # Upstream uv source (from sources.nix or flake input)
, version         # Version string (e.g., "0.10.8")
, uvNixSrc ? ./.  # Path to uv-nix sources (defaults to this repo)
, doCheck ? false # Whether to run tests
}:

let
  # Create patched source combining uv + uv-nix
  patchedSrc = stdenv.mkDerivation {
    name = "uv-nix-src-${version}";
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
      "${uvNixSrc}/patches/02-uv-cli-nix-commands.patch"
      "${uvNixSrc}/patches/03-uv-crate-nix-dispatch.patch"
      "${uvNixSrc}/patches/04-uv-python-nix-hook.patch"
      "${uvNixSrc}/patches/05-uv-dispatch-nix-build-env.patch"
    ];

    installPhase = ''
      cp -r . $out
    '';
  };

in rustPlatform.buildRustPackage {
  pname = "uv";
  version = "${version}-nix";

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
