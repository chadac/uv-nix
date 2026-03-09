# nix/uv-nix-sources.nix
# Pre-built uv-nix binary sources from GitHub releases
#
# Usage:
#   binSources = import ./uv-nix-sources.nix { inherit (pkgs) fetchurl; };
#   latestBin = binSources.latest "x86_64-linux";  # { version, url, hash, nixpkgsRev }
#   specificBin = binSources.get "0.10.8" "x86_64-linux";
{ fetchurl }:

let
  # Map of version -> binary info
  # Each version tracks:
  #   - nixpkgsRev: the nixpkgs commit used for building (important for ABI compat)
  #   - systems: map of system -> { url, hash }
  #
  # Binaries are published via CI to GitHub Releases.
  # Hashes are populated after each release build.
  binaries = {
    # Example structure (uncomment and fill when binaries are published):
    # "0.10.8" = {
    #   nixpkgsRev = "aca4d95fce4914b3892661bcb80b8087293536c6";
    #   systems = {
    #     "x86_64-linux" = {
    #       url = "https://github.com/chadac/uv-nix/releases/download/v0.10.8/uv-nix-0.10.8-x86_64-linux.tar.gz";
    #       hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    #     };
    #     "aarch64-linux" = {
    #       url = "https://github.com/chadac/uv-nix/releases/download/v0.10.8/uv-nix-0.10.8-aarch64-linux.tar.gz";
    #       hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    #     };
    #     "x86_64-darwin" = {
    #       url = "https://github.com/chadac/uv-nix/releases/download/v0.10.8/uv-nix-0.10.8-x86_64-darwin.tar.gz";
    #       hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    #     };
    #     "aarch64-darwin" = {
    #       url = "https://github.com/chadac/uv-nix/releases/download/v0.10.8/uv-nix-0.10.8-aarch64-darwin.tar.gz";
    #       hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    #     };
    #   };
    # };
  };

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
      systemData = if versionData == null then null else versionData.systems.${system} or null;
    in if systemData == null then null
       else systemData // {
         inherit version system;
         inherit (versionData) nixpkgsRev;
       };

  # Check if binary exists for version/system
  exists = version: system:
    (binaries.${version}.systems.${system} or null) != null;

  # Get latest binary info for a system (returns null if none available)
  latest = system:
    if latestVersion == null then null
    else let
      info = binaries.${latestVersion}.systems.${system} or null;
    in if info == null then null
       else info // {
         version = latestVersion;
         nixpkgsRev = binaries.${latestVersion}.nixpkgsRev;
       };

  # List versions available for a specific system
  versionsForSystem = system:
    builtins.filter (v: (binaries.${v}.systems.${system} or null) != null) allVersions;
}
