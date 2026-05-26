{
  description = "uv with Nix integration subcommand";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    systems.url = "github:nix-systems/default";
    cached-exec = {
      url = "github:chadac/cached-exec";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, flake-parts, nixpkgs, systems, cached-exec, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = import systems;

      perSystem = { pkgs, system, lib, ... }:
        let
          package = pkgs.callPackage ./. {};
          binaries = import ./nix/uv-nix-binaries.nix { inherit pkgs; };
        in {
          packages = {
            default = package;
            src = package;
          } // lib.optionalAttrs (binaries.latest or null != null) {
            bin = binaries.latest;
          } // binaries;

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
