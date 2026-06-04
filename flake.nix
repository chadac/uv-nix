{
  description = "uv with Nix integration subcommand";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    systems.url = "github:nix-systems/default";
    crane.url = "github:ipetkov/crane";
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    cached-exec = {
      url = "github:chadac/cached-exec";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, flake-parts, nixpkgs, systems, crane, git-hooks, cached-exec, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = import systems;

      flake.overlays.default = final: prev: {
        uv-nix = final.callPackage ./. { craneLib = crane.mkLib final; };
        uv-nix-bin =
          let binaries = import ./nix/uv-nix-binaries.nix { pkgs = final; };
          in binaries.latest or null;
      };

      perSystem = { pkgs, system, lib, ... }:
        let
          package = pkgs.callPackage ./. { craneLib = crane.mkLib pkgs; };
          binaries = import ./nix/uv-nix-binaries.nix { inherit pkgs; };
          pre-commit-check = git-hooks.lib.${system}.run {
            src = ./.;
            excludes = [ "^uv/" ];
            hooks = {
              rustfmt.enable = true;
              clippy = {
                enable = true;
                settings.allFeatures = true;
                settings.denyWarnings = true;
              };
            };
          };
        in {
          packages = {
            default = package;
            src = package;
          } // lib.optionalAttrs (binaries.latest or null != null) {
            bin = binaries.latest;
          } // binaries;

          checks.pre-commit = pre-commit-check;

          devShells.default = pkgs.mkShell {
            buildInputs = pre-commit-check.enabledPackages ++ [
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.just
              pkgs.python3
              pkgs.patchelf
              pkgs.python3Packages.pytest
              pkgs.python3Packages.docker
              pkgs.binutils
              pkgs.bzip2
              pkgs.xz
              pkgs.zstd
              pkgs.openssl
              pkgs.pkg-config
              pkgs.jq
              pkgs.gnused
              cached-exec.packages.${system}.default
            ] ++ lib.optionals pkgs.stdenv.isDarwin [
              pkgs.apple-sdk
              pkgs.libiconv
            ];

            CARGO_HOME = ".cargo";
            shellHook = ''
              export PATH="$PWD/uv/target/debug:$PATH"
              ${pre-commit-check.shellHook}
            '';
          };
        };
    };
}
