#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UV_DIR="$PROJECT_DIR/uv"
STAMP_FILE="$UV_DIR/crates/uv-nix/.applied"

if [ ! -f "$UV_DIR/Cargo.toml" ]; then
    echo "ERROR: uv submodule not initialized. Run:"
    echo "  git submodule update --init"
    exit 1
fi

# Check if any source files are newer than the stamp
needs_update=false
if [ ! -f "$STAMP_FILE" ]; then
    needs_update=true
else
    # Check if any src/, data/, Cargo.toml, or patches changed since last apply
    for f in "$PROJECT_DIR/Cargo.toml" \
             "$PROJECT_DIR/src/"* \
             "$PROJECT_DIR/data/"* \
             "$PROJECT_DIR/patches/"*.patch; do
        if [ "$f" -nt "$STAMP_FILE" ]; then
            needs_update=true
            break
        fi
    done
fi

if [ "$needs_update" = false ]; then
    exit 0
fi

echo "==> Resetting uv submodule to clean state..."
cd "$UV_DIR"
git checkout .
git clean -fd crates/uv-nix 2>/dev/null || true

echo "==> Copying uv-nix crate into uv/crates/uv-nix/..."
mkdir -p "$UV_DIR/crates/uv-nix/src"
mkdir -p "$UV_DIR/crates/uv-nix/data"
cp "$PROJECT_DIR/Cargo.toml" "$UV_DIR/crates/uv-nix/Cargo.toml"
cp -r "$PROJECT_DIR/src/"* "$UV_DIR/crates/uv-nix/src/"
cp -r "$PROJECT_DIR/data/"* "$UV_DIR/crates/uv-nix/data/"

echo "==> Applying patches..."
for patch in "$PROJECT_DIR/patches/"*.patch; do
    echo "    Applying $(basename "$patch")..."
    git apply "$patch"
done

echo "==> Updating Cargo.lock..."
cargo generate-lockfile 2>/dev/null || cargo update -w

touch "$STAMP_FILE"
echo "==> Done."
