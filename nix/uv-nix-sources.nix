# nix/uv-nix-sources.nix
# Pre-built uv-nix binary sources from GitHub releases
#
# Usage:
#   binSources = import ./uv-nix-sources.nix { inherit (pkgs) fetchurl; };
#   latestBin = binSources.latest "x86_64-linux";  # { version, url, hash, nixpkgsRev }
#   specificBin = binSources.get "0.10.8" "x86_64-linux";
{ fetchurl }:

let
  # GitHub release URL base
  releaseBase = "https://github.com/chadac/uv-nix/releases/download";

  # Map Nix system to release asset suffix
  # Note: Only x86_64-linux and aarch64-darwin are currently built by CI
  # aarch64-linux would require ARM Linux runners or QEMU emulation
  # x86_64-darwin would require Intel Mac runners (not available on GHA free tier)
  systemToAsset = {
    "x86_64-linux" = "linux-x86_64";
    "aarch64-linux" = "linux-aarch64";  # Not currently built
    "x86_64-darwin" = "darwin-x86_64";  # Not currently built
    "aarch64-darwin" = "darwin-arm64";
  };

  # Map of version -> binary info
  # Each version tracks:
  #   - nixpkgsRev: the nixpkgs commit used for building (important for ABI compat)
  #   - hashes: map of system -> hash (SRI format)
  #
  # Binaries are published via CI to GitHub Releases as raw executables.
  # Hashes are populated after each release build.
  #
  # To add a new version:
  # 1. Push a tag (e.g., git tag v0.10.9 && git push origin v0.10.9)
  # 2. Wait for CI to create the release
  # 3. Get hashes: nix hash to-sri sha256:$(curl -sL <url> | sha256sum | cut -d' ' -f1)
  # 4. Add entry below
  binaries = {
    # "0.10.9" = {
    #   nixpkgsRev = "aca4d95fce4914b3892661bcb80b8087293536c6";
    #   hashes = {
    #     "x86_64-linux" = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    #     "aarch64-linux" = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    #     "aarch64-darwin" = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    #   };
    # };
  };

  # Build URL for a version and system
  mkUrl = version: system:
    let asset = systemToAsset.${system} or null;
    in if asset == null then null
       else "${releaseBase}/v${version}/uv-nix-${version}-${asset}";

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
      hash = if versionData == null then null else versionData.hashes.${system} or null;
      url = mkUrl version system;
    in if hash == null || url == null then null
       else {
         inherit version system url hash;
         inherit (versionData) nixpkgsRev;
       };

  # Check if binary exists for version/system
  exists = version: system:
    (binaries.${version}.hashes.${system} or null) != null;

  # Get latest binary info for a system (returns null if none available)
  latest = system:
    if latestVersion == null then null
    else let
      hash = binaries.${latestVersion}.hashes.${system} or null;
      url = mkUrl latestVersion system;
    in if hash == null || url == null then null
       else {
         version = latestVersion;
         inherit url hash;
         nixpkgsRev = binaries.${latestVersion}.nixpkgsRev;
       };

  # List versions available for a specific system
  versionsForSystem = system:
    builtins.filter (v: (binaries.${v}.hashes.${system} or null) != null) allVersions;
}
