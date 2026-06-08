{
  description = "uv-nix pre-built binary";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f {
        pkgs = import nixpkgs { inherit system; };
      });
    in {
      overlays.default = final: prev: {
        uv-nix-bin = import ./default.nix { pkgs = final; };
      };

      packages = forAllSystems ({ pkgs }:
        let pkg = import ./default.nix { inherit pkgs; };
        in nixpkgs.lib.optionalAttrs (pkg != null) {
          default = pkg;
        }
      );
    };
}
