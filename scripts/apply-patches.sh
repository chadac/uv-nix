#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UV_DIR="$PROJECT_DIR/uv"
STAMP_FILE="$UV_DIR/crates/uv-nix/.applied"
UV_VERSION="$(jq -r .version "$PROJECT_DIR/data/uv.json")"

# Clone uv if missing, or fetch if version doesn't match
if [ ! -d "$UV_DIR/.git" ]; then
    echo "==> Cloning uv $UV_VERSION..."
    git clone --depth 1 --branch "$UV_VERSION" https://github.com/astral-sh/uv.git "$UV_DIR"
else
    current="$(git -C "$UV_DIR" describe --tags --exact-match 2>/dev/null || echo "")"
    if [ "$current" != "$UV_VERSION" ]; then
        echo "==> Switching uv to $UV_VERSION..."
        git -C "$UV_DIR" fetch --depth 1 origin tag "$UV_VERSION"
        git -C "$UV_DIR" checkout "$UV_VERSION"
    fi
fi

# Check if any source files are newer than the stamp
needs_update=false
if [ ! -f "$STAMP_FILE" ]; then
    needs_update=true
else
    for f in "$PROJECT_DIR/Cargo.toml" \
             "$PROJECT_DIR/data/"* \
             "$PROJECT_DIR/src/"* \
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

echo "==> Resetting uv to clean state..."
cd "$UV_DIR"
git checkout .
git clean -fd crates/uv-nix 2>/dev/null || true

echo "==> Copying uv-nix crate into uv/crates/uv-nix/..."
mkdir -p "$UV_DIR/crates/uv-nix/src"
mkdir -p "$UV_DIR/crates/uv-nix/data"
cp "$PROJECT_DIR/Cargo.toml" "$UV_DIR/crates/uv-nix/Cargo.toml"
cp -r "$PROJECT_DIR/src/"* "$UV_DIR/crates/uv-nix/src/"
cp -r "$PROJECT_DIR/data/"* "$UV_DIR/crates/uv-nix/data/"

echo "==> Adding uv-nix to workspace dependencies..."
sed -i '/^uv-normalize = /a uv-nix = { version = "0.0.1", path = "crates/uv-nix" }' Cargo.toml

echo "==> Applying patches..."
for patch in "$PROJECT_DIR/patches/"*.patch; do
    echo "    Applying $(basename "$patch")..."
    git apply "$patch"
done

bash "$SCRIPT_DIR/update-lockfile.sh"

touch "$STAMP_FILE"
echo "==> Done."
