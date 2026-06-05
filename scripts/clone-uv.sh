#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
UV_DIR="$PROJECT_DIR/uv"
UV_VERSION="$(jq -r .version "$PROJECT_DIR/data/uv.json")"

# Clone uv if missing, or fetch if version doesn't match
if [ ! -d "$UV_DIR/.git" ]; then
    echo "==> Cloning uv $UV_VERSION..."
    mkdir -p "$UV_DIR"
    git init "$UV_DIR"
    git -C "$UV_DIR" remote add origin https://github.com/astral-sh/uv.git
    git -C "$UV_DIR" fetch --depth 1 origin tag "$UV_VERSION"
    git -C "$UV_DIR" checkout "$UV_VERSION"
else
    current="$(git -C "$UV_DIR" describe --tags --exact-match 2>/dev/null || echo "")"
    if [ "$current" != "$UV_VERSION" ]; then
        echo "==> Switching uv to $UV_VERSION..."
        git -C "$UV_DIR" checkout . 2>/dev/null || true
        git -C "$UV_DIR" clean -fd crates/uv-nix 2>/dev/null || true
        git -C "$UV_DIR" fetch --depth 1 origin tag "$UV_VERSION"
        git -C "$UV_DIR" checkout "$UV_VERSION"
    else
        echo "==> uv $UV_VERSION already cloned"
    fi
fi
