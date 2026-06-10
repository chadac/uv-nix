#!/usr/bin/env bash
# Heavy integration test for `uv nix gen` with complex packages.
#
# Tests packages that have runtime-libs (ctypes/dlopen), large native
# dependency trees, or require propagatedBuildInputs in the Nix overlay.
#
# These are separated from the lightweight test because they take longer
# to build and may not have wheels on all platforms.
#
# Requirements:
# - UV_BIN set to the patched uv binary
# - nix available on PATH
# - Network access (for uv pip install + nix build)
set -euo pipefail

UV="${UV_BIN:-uv}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${TEST_WORK_DIR:-$(mktemp -d /tmp/uv-nix-gen-heavy.XXXXXX)}"
CLEANUP="${TEST_CLEANUP:-1}"

cleanup() {
    if [ "$CLEANUP" = "1" ] && [ -d "$WORK_DIR" ]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

echo "=== uv nix gen heavy integration test ==="
echo "UV binary: $UV"
echo "Work dir:  $WORK_DIR"

# Packages to test. Each entry: "pip-name:import-check"
# These packages exercise runtime-libs and complex native deps.
PACKAGES=(
    "matplotlib:import matplotlib; matplotlib.use('Agg'); import matplotlib.pyplot as plt; fig, ax = plt.subplots(); print('ok')"
    "pysodium:import pysodium; print('ok')"
    "scipy:import scipy; import scipy.linalg; print(scipy.__version__)"
    "pillow:from PIL import Image; print('ok')"
    "lxml:from lxml import etree; print(etree.LXML_VERSION)"
)

# ---------------------------------------------------------------------------
# Step 1: Create a Python project
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 1: Creating Python project ---"
cd "$WORK_DIR"
"$UV" init --no-progress --python 3.12 test-heavy
cd test-heavy

rm -f flake.nix flake.lock

# Try to add each package; skip packages without available wheels
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

# ---------------------------------------------------------------------------
# Step 3: Generate Nix expression
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 3: Running uv nix gen ---"
"$UV" nix gen -o venv.nix
echo "Generated venv.nix"
cat venv.nix

# ---------------------------------------------------------------------------
# Step 3b: Validate generated expression has expected content
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 3b: Validating generated Nix expression ---"

# matplotlib should have propagatedBuildInputs for fontconfig (runtime-lib)
if grep -q "propagatedBuildInputs" venv.nix; then
    echo "  OK: found propagatedBuildInputs in venv.nix"
else
    echo "  WARN: no propagatedBuildInputs found (may be expected if no runtime-libs resolved)"
fi

# pysodium should appear in the overlay (runtime-only package)
if grep -q "pysodium" venv.nix; then
    echo "  OK: pysodium found in overlay"
else
    echo "  WARN: pysodium not in overlay (may not have resolved)"
fi

# ---------------------------------------------------------------------------
# Step 4: Create flake.nix
# ---------------------------------------------------------------------------
echo ""
echo "--- Step 4: Creating flake.nix ---"
cat > flake.nix << 'FLAKE_EOF'
{
  description = "uv-nix gen heavy integration test";

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

# Build LD_LIBRARY_PATH from the venv's Nix closure.
# propagatedBuildInputs puts runtime libs in the closure, but Python's
# ctypes.util.find_library needs LD_LIBRARY_PATH to discover them.
LIB_PATH=""
for lib_dir in $(find "$VENV_PATH" -name "lib" -path "*/nix/store/*" -type d 2>/dev/null | head -0); do true; done
# Scan the closure for lib directories containing .so files
for store_path in $(nix-store -qR "$VENV_PATH" 2>/dev/null); do
    if [ -d "$store_path/lib" ]; then
        # Only add dirs that actually have shared libraries
        if ls "$store_path/lib/"*.so* &>/dev/null || ls "$store_path/lib/"*.dylib* &>/dev/null; then
            LIB_PATH="${LIB_PATH:+$LIB_PATH:}$store_path/lib"
        fi
    fi
done
if [ -n "$LIB_PATH" ]; then
    echo "Setting LD_LIBRARY_PATH from closure ($(echo "$LIB_PATH" | tr ':' '\n' | wc -l) dirs)"
    export LD_LIBRARY_PATH="$LIB_PATH${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    export DYLD_LIBRARY_PATH="$LIB_PATH${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
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
