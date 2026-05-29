#!/usr/bin/env bash
# Integration test for `uv nix gen`.
#
# Tests the full pipeline:
# 1. Create a Python project with native dependencies
# 2. Install packages with our patched uv (generates patches.json)
# 3. Run `uv nix gen` to produce a Nix expression
# 4. Build a uv2nix virtualenv from that expression
# 5. Import each library in the Nix-built venv
#
# Requirements:
# - UV_BIN set to the patched uv binary
# - nix available on PATH
# - Network access (for uv pip install + nix build)
set -euo pipefail

UV="${UV_BIN:-uv}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${TEST_WORK_DIR:-$(mktemp -d /tmp/uv-nix-gen-test.XXXXXX)}"
CLEANUP="${TEST_CLEANUP:-1}"

cleanup() {
    if [ "$CLEANUP" = "1" ] && [ -d "$WORK_DIR" ]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

echo "=== uv nix gen integration test ==="
echo "UV binary: $UV"
echo "Work dir:  $WORK_DIR"

# Packages to test. Each entry: "pip-name:import-check"
# Keep this focused on packages with native extensions that are
# commonly available as wheels and cover different library types.
PACKAGES=(
    "numpy:import numpy; print(numpy.__version__)"
    "pyyaml:import yaml; yaml.CSafeLoader; print('ok')"
    "markupsafe:import markupsafe; print(markupsafe.__version__)"
    "orjson:import orjson; print(orjson.dumps({'a': 1}))"
    "cffi:import _cffi_backend; print('ok')"
    "ujson:import ujson; print(ujson.dumps({'a': 1}))"
    "msgpack:import msgpack; print(msgpack.packb({'a': 1}))"
    "regex:import regex; print(regex.search(r'\w+', 'hello').group())"
    "rpds-py:import rpds; print(rpds.HashTrieMap({'a': 1}))"
)

# ---------------------------------------------------------------------------
# Step 1: Create a Python project
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 1: Creating Python project ---"
cd "$WORK_DIR"
"$UV" init --no-progress --python 3.12 test-project
cd test-project

# Remove auto-generated flake files (uv-nix may create these; we'll write our own)
rm -f flake.nix flake.lock

# Add all packages
pkg_args=()
for entry in "${PACKAGES[@]}"; do
    pkg="${entry%%:*}"
    pkg_args+=("$pkg")
done

echo "Adding packages: ${pkg_args[*]}"
"$UV" add --no-progress "${pkg_args[@]}"

# ---------------------------------------------------------------------------
# Step 2: Verify patches.json was generated
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 2: Verifying patches.json ---"
PATCHES_JSON=".venv/share/uv-nix/patches.json"
if [ ! -f "$PATCHES_JSON" ]; then
    echo "FAIL: $PATCHES_JSON not found"
    exit 1
fi
echo "Found $(python3 -c "import json; d=json.load(open('$PATCHES_JSON')); print(len(d['packages']))")" packages in patches.json
# Show summary of nix_libs per package for debugging
python3 -c "
import json
d = json.load(open('$PATCHES_JSON'))
for name, info in d['packages'].items():
    libs = set()
    for p in info['patches'].values():
        libs.update(p.get('nix_libs', []))
    if libs:
        print(f'  {name}: {sorted(libs)}')
    else:
        print(f'  {name}: (no nix_libs)')
"

# ---------------------------------------------------------------------------
# Step 3: Generate Nix expression
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 3: Running uv nix gen ---"
"$UV" nix gen -o venv.nix
echo "Generated venv.nix"
cat venv.nix

# ---------------------------------------------------------------------------
# Step 4: Create flake.nix that uses the generated expression
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 4: Creating flake.nix ---"
cat > flake.nix << 'FLAKE_EOF'
{
  description = "uv-nix gen integration test";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    uv2nix = {
      url = "github:adisbladis/uv2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    pyproject-nix = {
      url = "github:pyproject-nix/pyproject.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    pyproject-build-systems = {
      url = "github:pyproject-nix/build-system-pkgs";
      inputs = {
        pyproject-nix.follows = "pyproject-nix";
        uv2nix.follows = "uv2nix";
        nixpkgs.follows = "nixpkgs";
      };
    };
  };

  outputs = { nixpkgs, uv2nix, pyproject-nix, pyproject-build-systems, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          venvExpr = import ./venv.nix {
            inherit pkgs uv2nix pyproject-nix pyproject-build-systems;
            lib = pkgs.lib;
            python = pkgs.python312;
          };
        in {
          default = venvExpr.venv;
        }
      );
    };
}
FLAKE_EOF

# Initialize git repo (required by uv2nix/flake)
# The .venv is already gitignored by uv init, but venv.nix and flake.nix need to be tracked
git init -q
git add -A
git -c user.name="test" -c user.email="test@test" -c commit.gpgsign=false commit -q -m "test"
echo "Created flake.nix and committed"

# ---------------------------------------------------------------------------
# Step 5: Build the uv2nix virtualenv
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 5: Building uv2nix virtualenv ---"
nix build --no-link --print-out-paths 2>&1
VENV_PATH="$(nix build --no-link --print-out-paths 2>/dev/null)"
echo "Built venv at: $VENV_PATH"

# ---------------------------------------------------------------------------
# Step 6: Import each library
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 6: Testing imports ---"
PYTHON="$VENV_PATH/bin/python"
if [ ! -x "$PYTHON" ]; then
    echo "FAIL: Python not found at $PYTHON"
    exit 1
fi

FAILED=0
for entry in "${PACKAGES[@]}"; do
    pkg="${entry%%:*}"
    check="${entry#*:}"
    echo -n "  $pkg ... "
    if output=$("$PYTHON" -c "$check" 2>&1); then
        echo "OK ($output)"
    else
        echo "FAIL"
        echo "    $output"
        FAILED=$((FAILED + 1))
    fi
done

echo ""
if [ "$FAILED" -gt 0 ]; then
    echo "FAIL: $FAILED package(s) failed to import"
    exit 1
fi
echo "=== All ${#PACKAGES[@]} packages imported successfully ==="
