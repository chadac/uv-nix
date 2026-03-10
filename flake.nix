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
          # Import source definitions
          sources = import ./nix/sources.nix { inherit (pkgs) fetchFromGitHub; };
          binSources = import ./nix/uv-nix-sources.nix { inherit (pkgs) fetchurl; };

          # Import library helpers
          uvNixLib = import ./nix/lib.nix { inherit lib pkgs; };

          # Import build function
          buildUv = pkgs.callPackage ./. {};

          # Filter self to only include files needed for uv-nix build
          # This prevents cache invalidation when unrelated files change (e.g., CI configs)
          uvNixSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./src
              ./data
              ./patches
            ];
          };

          # Build a source version
          mkSourceBuild = { version, src }: buildUv {
            uvSrc = src;
            inherit version;
            inherit uvNixSrc;
          };

          # Build a pre-built binary package with autoPatchelfHook
          mkBinaryPackage = version:
            let
              binInfo = binSources.get version system;
            in if binInfo == null then null
               else pkgs.stdenv.mkDerivation {
                 pname = "uv-nix-bin";
                 inherit version;

                 # Fetch raw binary from GitHub release
                 src = pkgs.fetchurl {
                   inherit (binInfo) url hash;
                   executable = true;
                 };

                 dontUnpack = true;

                 nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [
                   pkgs.autoPatchelfHook
                 ];

                 buildInputs = lib.optionals pkgs.stdenv.isLinux (
                   uvNixLib.defaultLibs ++ [
                     pkgs.stdenv.cc.cc.lib  # libstdc++
                   ]
                 );

                 installPhase = ''
                   runHook preInstall
                   install -Dm755 $src $out/bin/uv
                   runHook postInstall
                 '';

                 meta = {
                   description = "uv Python package manager with Nix integration (pre-built)";
                   mainProgram = "uv";
                 };
               };

          # Helper to convert version dots to dashes for attribute names
          versionToAttr = v: builtins.replaceStrings ["."] ["-"] v;

          # Generate packages for all source versions
          sourcePackages = builtins.listToAttrs (map (v: {
            name = "build-${versionToAttr v.version}";
            value = mkSourceBuild v;
          }) sources.all);

          # Generate packages for all binary versions (filter out nulls)
          binaryPackages = builtins.listToAttrs (
            builtins.filter (x: x.value != null) (
              map (version: {
                name = "bin-${versionToAttr version}";
                value = mkBinaryPackage version;
              }) binSources.allVersions
            )
          );

          # Latest versions
          latestSource = mkSourceBuild sources.latest;
          latestBinary = if binSources.latestVersion != null
                         then mkBinaryPackage binSources.latestVersion
                         else null;

        in {
          packages = sourcePackages // binaryPackages // {
            # Aliases for latest
            default = if latestBinary != null then latestBinary else latestSource;
            build = latestSource;
          } // lib.optionalAttrs (latestBinary != null) {
            bin = latestBinary;
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

      # Flake-level outputs (not per-system)
      flake = {
        # Expose lib per-system (for patchPython)
        lib = builtins.listToAttrs (map (system: {
          name = system;
          value = let
            pkgs = nixpkgs.legacyPackages.${system};
          in {
            patchPython = pkgs.callPackage ./nix/patch-python.nix { inherit pkgs; };
          };
        }) (import systems));
      };
    };
}
