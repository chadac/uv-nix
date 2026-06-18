{
  description = "uv-nix pre-built binary";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs, ... }:
    let
      lib = nixpkgs.lib;

      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = f: lib.genAttrs systems (system: f {
        pkgs = import nixpkgs { inherit system; };
      });

      # Discover every release file in ./releases dynamically, so new releases
      # are vended automatically without editing this flake.
      releaseFiles =
        lib.filterAttrs (name: type: type == "regular" && lib.hasSuffix ".nix" name)
          (builtins.readDir ./releases);

      # version string (e.g. "0.11.21") -> path to its release file
      releases = lib.mapAttrs'
        (name: _: lib.nameValuePair (lib.removeSuffix ".nix" name) (./releases + "/${name}"))
        releaseFiles;

      # Highest version wins as the default.
      latestVersion =
        lib.last (lib.sort (a: b: builtins.compareVersions a b < 0)
          (builtins.attrNames releases));

      # Dots are attribute-path separators in `nix run`/`nix build`, so the
      # output name uses dashes: 0.11.21 -> 0-11-21.
      versionToAttr = v: builtins.replaceStrings [ "." ] [ "-" ] v;
    in {
      overlays.default = final: prev: {
        uv-nix-bin = import ./default.nix { pkgs = final; };
      };

      packages = forAllSystems ({ pkgs }:
        let
          # One package per release, keyed by version so you can run e.g.
          #   nix build github:chadac/uv-nix/bin#0-11-21
          # Releases with no asset for the current system are dropped.
          versioned = lib.filterAttrs (_: pkg: pkg != null)
            (lib.mapAttrs' (version: release: lib.nameValuePair
              (versionToAttr version)
              (import ./default.nix { inherit pkgs release; }))
              releases);

          latest = import ./default.nix {
            inherit pkgs;
            release = releases.${latestVersion};
          };
        in
          versioned // lib.optionalAttrs (latest != null) { default = latest; }
      );
    };
}
