# nix/sources.nix
# UV source versions for building from source
#
# Usage:
#   sources = import ./sources.nix { inherit (pkgs) fetchFromGitHub; };
#   latestSrc = sources.latest;  # { version, src }
#   specificSrc = sources.find "0.10.8";  # { version, src }
{ fetchFromGitHub }:

let
  # List of supported versions, newest first
  versions = [
    {
      version = "0.10.9";
      hash = "sha256:0015qhyxwp5khl1c4m4dsjq4p3sqbzy8v8ca96spvqcvav0dw1i0";
    }
    {
      version = "0.10.8";
      hash = "sha256:02bcqbqng36pvmkkll9m3hhprcbs3bqfs6l7lg44ka39lwn37lli";
    }
    {
      version = "0.10.7";
      hash = "sha256:05ssx8vkhig2q3s7y36p7cdyc87q3jdh267rl3p0iyn6gn3lkm2v";
    }
  ];

  # Fetch a single version
  fetchVersion = { version, hash }: {
    inherit version;
    src = fetchFromGitHub {
      owner = "astral-sh";
      repo = "uv";
      rev = version;
      inherit hash;
    };
  };

  allVersions = map fetchVersion versions;

in {
  # All versions as a list of { version, src } records
  all = allVersions;

  # Get the latest version
  latest = builtins.head allVersions;

  # Get N most recent versions
  recent = n: builtins.genList (i: builtins.elemAt allVersions i) (builtins.min n (builtins.length allVersions));

  # Find a specific version (returns null if not found)
  find = v:
    let matches = builtins.filter (x: x.version == v) allVersions;
    in if matches == [] then null else builtins.head matches;

  # List of version strings
  versionList = map (v: v.version) versions;

  # Raw version metadata (for introspection)
  versionMeta = versions;
}
