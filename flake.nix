{
  description = "uv with Nix integration subcommand";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    systems.url = "github:nix-systems/default";
    crane.url = "github:ipetkov/crane";
    cached-exec = {
      url = "github:chadac/cached-exec";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, flake-parts, nixpkgs, systems, crane, cached-exec, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = import systems;

      flake.overlays.default = final: prev: {
        uv-nix = final.callPackage ./. { craneLib = crane.mkLib final; };
        uv-nix-bin = import ./nix/bin.nix { pkgs = final; };
      };

      perSystem = { pkgs, system, lib, ... }:
        let
          package = pkgs.callPackage ./. { craneLib = crane.mkLib pkgs; };
          binPkg = import ./nix/bin.nix { inherit pkgs; };
        in {
          packages = {
            default = package;
          } // lib.optionalAttrs (binPkg != null) {
            bin = binPkg;
          };

          devShells.default = pkgs.mkShell {
            buildInputs = [
              pkgs.rustc
              pkgs.cargo
              pkgs.just
              pkgs.python3
              # Build dependencies for uv
              pkgs.bzip2
              pkgs.xz
              pkgs.zstd
              pkgs.openssl
              pkgs.pkg-config
              # Dev tools
              pkgs.jq
              pkgs.gnused
              cached-exec.packages.${system}.default
            ] ++ lib.optionals pkgs.stdenv.isDarwin [
              pkgs.apple-sdk
              pkgs.libiconv
            ];

            shellHook = ''
              export CARGO_HOME="$PWD/.cargo"
              export PATH="$PWD/uv/target/debug:$PATH"
            '';
          };
        };
    };
}
