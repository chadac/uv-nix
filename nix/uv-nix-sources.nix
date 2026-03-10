# nix/uv-nix-sources.nix
# Pre-built uv-nix binary sources from GitHub releases
#
# Usage:
#   binSources = import ./uv-nix-sources.nix { inherit (pkgs) fetchurl; };
#   latestBin = binSources.latest "x86_64-linux";  # { version, url, hash, releaseTag }
#   specificBin = binSources.get "0.10.9" "x86_64-linux";
{ fetchurl }:

let
  # GitHub release URL base
  releaseBase = "https://github.com/chadac/uv-nix/releases/download";

  # Map of version -> binary info
  # Each version tracks:
  #   - releaseTag: the GitHub release tag (e.g., "v0.10.9-nix.1")
  #   - assets: map of system -> { name, hash } for each binary
  #
  # Binaries are published via CI to GitHub Releases as raw executables.
  # Hashes are populated after each release build.
  #
  # To add a new version:
  # 1. Push a tag (e.g., git tag v0.10.9-nix.1 && git push origin v0.10.9-nix.1)
  # 2. Wait for CI to create the release
  # 3. Get hashes using nix-prefetch-url and convert to SRI format:
  #      hash=$(nix-prefetch-url <url>)
  #      nix hash to-sri --type sha256 $hash
  # 4. Add entry below
  binaries = {
    "0.10.9-nix" = {
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
  };

  # Build URL for a version and system
  mkUrl = version: system:
    let
      versionData = binaries.${version} or null;
      assetData = if versionData == null then null else versionData.assets.${system} or null;
    in if assetData == null then null
       else "${releaseBase}/${versionData.releaseTag}/${assetData.name}";

  # Get all available versions (sorted newest first)
  allVersions = builtins.sort (a: b: builtins.compareVersions a b > 0) (builtins.attrNames binaries);

  # Get latest version (or null if none)
  latestVersion = if allVersions == [] then null else builtins.head allVersions;

in {
  inherit binaries allVersions latestVersion;

  # Check if any binaries are available
  hasAny = allVersions != [];

  # Get binary info for a specific version and system (returns null if not found)
  get = version: system:
    let
      versionData = binaries.${version} or null;
      assetData = if versionData == null then null else versionData.assets.${system} or null;
      url = mkUrl version system;
    in if assetData == null || url == null then null
       else {
         inherit version system url;
         inherit (assetData) hash;
         inherit (versionData) releaseTag;
       };

  # Check if binary exists for version/system
  exists = version: system:
    (binaries.${version}.assets.${system} or null) != null;

  # Get latest binary info for a system (returns null if none available)
  latest = system:
    if latestVersion == null then null
    else let
      versionData = binaries.${latestVersion};
      assetData = versionData.assets.${system} or null;
      url = mkUrl latestVersion system;
    in if assetData == null || url == null then null
       else {
         version = latestVersion;
         inherit url;
         inherit (assetData) hash;
         inherit (versionData) releaseTag;
       };

  # List versions available for a specific system
  versionsForSystem = system:
    builtins.filter (v: (binaries.${v}.assets.${system} or null) != null) allVersions;
}
