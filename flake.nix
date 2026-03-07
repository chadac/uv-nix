{
  description = "uv with Nix integration subcommand";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    uv-src = {
      url = "github:astral-sh/uv/0.10.8";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, uv-src }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in {
      lib = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in {
          # Callable function: takes { pythonSrc } and returns a patched derivation
          patchPython = pkgs.callPackage ./nix/patch-python.nix { inherit pkgs; };
        }
      );

      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};

          patchedSrc = pkgs.stdenv.mkDerivation {
            name = "uv-nix-src";
            src = uv-src;
            phases = [ "unpackPhase" "patchPhase" "installPhase" ];

            postUnpack = ''
              mkdir -p $sourceRoot/crates/uv-nix/src
              mkdir -p $sourceRoot/crates/uv-nix/data
              cp ${./Cargo.toml} $sourceRoot/crates/uv-nix/Cargo.toml
              cp -r ${./src}/* $sourceRoot/crates/uv-nix/src/
              cp -r ${./data}/* $sourceRoot/crates/uv-nix/data/
            '';

            patches = [
              ./patches/01-workspace-add-uv-nix.patch
              ./patches/02-uv-cli-nix-commands.patch
              ./patches/03-uv-crate-nix-dispatch.patch
              ./patches/04-uv-python-nix-hook.patch
              ./patches/05-uv-dispatch-nix-build-env.patch
            ];

            postPatch = ''
              cp ${./Cargo.lock} Cargo.lock
            '';

            installPhase = ''
              cp -r . $out
            '';
          };

          uvUnwrapped = pkgs.rustPlatform.buildRustPackage {
            pname = "uv";
            version = "0.10.8-nix";

            src = patchedSrc;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            buildInputs = [
              pkgs.rust-jemalloc-sys
            ];

            nativeBuildInputs = [
              pkgs.installShellFiles
            ];

            cargoBuildFlags = [
              "--package"
              "uv"
            ];

            # Tests require Python 3
            doCheck = false;

            meta = with pkgs.lib; {
              description = "uv Python package manager with Nix integration";
              license = with licenses; [ asl20 mit ];
              mainProgram = "uv";
            };
          };

        in {
          default = uvUnwrapped;
        }
      );
    };
}
