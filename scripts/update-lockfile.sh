#!/usr/bin/env bash
# Regenerate uv/Cargo.lock and copy it to data/Cargo.lock for Nix builds.
#
# Expects the patched uv workspace to already exist at uv/.
# Called by apply-patches.sh automatically, or run standalone via `just update-lockfile`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UV_DIR="$PROJECT_DIR/uv"

if [ ! -f "$UV_DIR/Cargo.toml" ]; then
    echo "error: uv workspace not found at $UV_DIR" >&2
    echo "       Run 'just build' or 'bash scripts/apply-patches.sh' first." >&2
    exit 1
fi

echo "==> Updating Cargo.lock..."
cargo generate-lockfile --manifest-path "$UV_DIR/Cargo.toml" 2>/dev/null \
    || cargo update --manifest-path "$UV_DIR/Cargo.toml" -w

echo "==> Copying Cargo.lock to data/Cargo.lock..."
cp "$UV_DIR/Cargo.lock" "$PROJECT_DIR/data/Cargo.lock"

echo "==> Done."
