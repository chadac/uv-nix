# nix/uv-nix-binaries.nix
# Pre-built uv-nix binary packages from GitHub releases
#
# Returns an attrset of { bin-<version> = <derivation>; }
# plus `latest` (the newest binary derivation, or null).
#
# To add a new version:
# 1. Push a tag (e.g., git tag v0.10.9-nix.1 && git push origin v0.10.9-nix.1)
# 2. Wait for CI to create the release
# 3. Get hashes using nix-prefetch-url and convert to SRI format:
#      hash=$(nix-prefetch-url <url>)
#      nix hash to-sri --type sha256 $hash
# 4. Add entry below
{ pkgs }:

let
  lib = pkgs.lib;
  system = pkgs.stdenv.hostPlatform.system;

  uvNixLib = import ./lib.nix { inherit lib pkgs; };

  releaseBase = "https://github.com/chadac/uv-nix/releases/download";

  binaries = {
    "0.10.9" = {
      releaseTag = "v0.10.9-nix.1";
      assets = {
        "x86_64-linux" = {
          name = "uv-nix-0.10.9-linux-x86_64";
          hash = "sha256-N4cdXeLnBjSrFuwenzZVudHTcn5QNT/TYGsLZiYWZEM=";
        };
        "aarch64-linux" = {
          name = "uv-nix-0.10.9-nix-linux-aarch64";
          hash = "sha256-BB67XHdSqO+uFk9HCHEp9V9WA+qe3zqMX2cciFPxCK4=";
        };
        "aarch64-darwin" = {
          name = "uv-nix-0.10.9-darwin-arm64";
          hash = "sha256-zS9PkBFSLQLF5ihE+PU/OFFYFieTHitt2ghdhwUrqe4=";
        };
      };
    };
    "0.10.8" = {
      releaseTag = "v0.10.9-nix.1";
      assets = {
        "x86_64-linux" = {
          name = "uv-nix-0.10.8-linux-x86_64";
          hash = "sha256-UZ189GQYFQzCGKTQNearVsCxjd5Ta5zR7pmadtq5Mec=";
        };
        "aarch64-linux" = {
          name = "uv-nix-0.10.8-nix-linux-aarch64";
          hash = "sha256-a8NgT5tC+FU3M6b/iJG3u9EUwAVfThG6AcCe7Vy4acs=";
        };
        "aarch64-darwin" = {
          name = "uv-nix-0.10.8-darwin-arm64";
          hash = "sha256-DbIkIIby3inOGzVB0gRmeRfcDWZIcQs24YZcBpXheLk=";
        };
      };
    };
    "0.10.7" = {
      releaseTag = "v0.10.9-nix.1";
      assets = {
        "x86_64-linux" = {
          name = "uv-nix-0.10.7-linux-x86_64";
          hash = "sha256-NEzZ8kHVHs3WBPR1gNIjtoxYC7fQcQZxSGa4Cag290M=";
        };
        "aarch64-linux" = {
          name = "uv-nix-0.10.7-nix-linux-aarch64";
          hash = "sha256-LMdsONejtU39DfLVyjSpOxdXqhilPQt883pEWoZe/Gk=";
        };
        "aarch64-darwin" = {
          name = "uv-nix-0.10.7-darwin-arm64";
          hash = "sha256-RFuU7svp/EX6IwBu4mr4pQvmd3XBjLyga4l+PklDtKU=";
        };
      };
    };
    "0.10.6" = {
      releaseTag = "v0.10.9-nix.1";
      assets = {
        "x86_64-linux" = {
          name = "uv-nix-0.10.6-linux-x86_64";
          hash = "sha256-G4joXE1RR8huVT77hCZNSAZR9vLqQQi4Zc3JcRCwH78=";
        };
        "aarch64-linux" = {
          name = "uv-nix-0.10.6-nix-linux-aarch64";
          hash = "sha256-qxGiGQxGcQ64h3JfOzHDCxzEIuLu6UYeku6wWphEfjQ=";
        };
        "aarch64-darwin" = {
          name = "uv-nix-0.10.6-darwin-arm64";
          hash = "sha256-DqzxFVizC1SLVCbXie/Cs3v5Az3Btoq0NfzYIyklaNY=";
        };
      };
    };
    "0.10.5" = {
      releaseTag = "v0.10.9-nix.1";
      assets = {
        "x86_64-linux" = {
          name = "uv-nix-0.10.5-linux-x86_64";
          hash = "sha256-GTlaGUW61rcck4rclZbYSZRm2+56ZxJsLasL2qdS1wQ=";
        };
        "aarch64-linux" = {
          name = "uv-nix-0.10.5-nix-linux-aarch64";
          hash = "sha256-RWl+hlNZHjzYPXC+mYvFf3y2W0QKW0B3d3ZurGWsUts=";
        };
        "aarch64-darwin" = {
          name = "uv-nix-0.10.5-darwin-arm64";
          hash = "sha256-SaRTzFnAB3nFrfaYzGXVhXq8llF1nFxR7zH01OgLpfg=";
        };
      };
    };
  };

  mkUrl = version:
    let
      versionData = binaries.${version} or null;
      assetData = if versionData == null then null else versionData.assets.${system} or null;
    in if assetData == null then null
       else "${releaseBase}/${versionData.releaseTag}/${assetData.name}";

  mkBinaryPackage = version:
    let
      versionData = binaries.${version} or null;
      assetData = if versionData == null then null else versionData.assets.${system} or null;
      url = mkUrl version;
    in if assetData == null || url == null then null
       else pkgs.stdenv.mkDerivation {
         pname = "uv-nix-bin";
         inherit version;

         src = builtins.fetchurl {
           inherit url;
           sha256 = assetData.hash;
         };

         dontUnpack = true;

         nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [
           pkgs.autoPatchelfHook
         ];

         buildInputs = lib.optionals pkgs.stdenv.isLinux (
           uvNixLib.defaultLibs ++ [
             pkgs.stdenv.cc.cc.lib
             pkgs.rust-jemalloc-sys
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

  versionToAttr = v: builtins.replaceStrings ["."] ["-"] v;

  allVersions = builtins.sort (a: b: builtins.compareVersions a b > 0) (builtins.attrNames binaries);
  latestVersion = if allVersions == [] then null else builtins.head allVersions;

  packages = builtins.listToAttrs (
    builtins.filter (x: x.value != null) (
      map (version: {
        name = "bin-${versionToAttr version}";
        value = mkBinaryPackage version;
      }) allVersions
    )
  );

  latest = if latestVersion != null then mkBinaryPackage latestVersion else null;

in packages // lib.optionalAttrs (latest != null) { inherit latest; }
