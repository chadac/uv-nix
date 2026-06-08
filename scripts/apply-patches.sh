#!/usr/bin/env bash
set -euxo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UV_DIR="$PROJECT_DIR/uv"

# Ensure uv is cloned at the correct version
bash "$SCRIPT_DIR/clone-uv.sh"

# Reset to clean state (target/ is gitignored, unaffected)
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

echo "==> Done."
