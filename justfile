# Build the patched uv binary using cargo (fast, for development)
build:
    bash scripts/apply-patches.sh
    #!/usr/bin/env bash
    set -euo pipefail
    cached-exec \
        Cargo.toml \
        $(find src/ -type f) \
        $(find data/ -type f) \
        $(find patches/ -type f -name '*.patch') \
        data/uv.json \
        -- \
        cargo build --manifest-path uv/Cargo.toml --package uv --no-default-features --features "uv-distribution/static,test-defaults"

# Force rebuild (ignores cache)
build-force:
    bash scripts/apply-patches.sh
    #!/usr/bin/env bash
    set -euo pipefail
    cached-exec -f \
        Cargo.toml \
        $(find src/ -type f) \
        $(find data/ -type f) \
        $(find patches/ -type f -name '*.patch') \
        data/uv.json \
        -- \
        cargo build --manifest-path uv/Cargo.toml --package uv --no-default-features --features "uv-distribution/static,test-defaults"

# Full nix build (produces self-contained binary)
nix-build:
    nix build

# Check local Rust crate compiles
check:
    cargo check

# =============================================================================
# Test commands
# =============================================================================

# Run wheel install tests (default)
test: test-wheel

# Run wheel install tests
test-wheel: build
    UV_BIN="$(pwd)/uv/target/debug/uv" cargo test --test wheel_install -- --test-threads=4

# Run source build tests (slow)
test-source: build
    UV_BIN="$(pwd)/uv/target/debug/uv" cargo test --test source_build -- --ignored --test-threads=2

# Run Python patching tests
test-patch: build
    UV_BIN="$(pwd)/uv/target/debug/uv" cargo test --test python_patch

# Run Docker-based integration tests (pytest)
test-docker: build
    cd tests/docker && UV_BIN="$(pwd)/../../uv/target/debug/uv" uv run pytest -v -n auto -m 'not slow and not source_build'

# Run a specific package test
test-pkg PKG: build
    UV_BIN="$(pwd)/uv/target/debug/uv" cargo test --test wheel_install {{PKG}}

# Run wheel tests sequentially (for debugging)
test-seq: build
    UV_BIN="$(pwd)/uv/target/debug/uv" cargo test --test wheel_install -- --test-threads=1

# Run all test suites
test-all: test-wheel test-source test-patch test-docker

# Clear test venv cache
test-clean:
    rm -rf /tmp/uv-nix-tests

# =============================================================================
# Docker utilities
# =============================================================================

# Spawn an interactive Docker container with uv + nix on PATH
# Usage: just docker [image]
docker image="busybox": build
    #!/usr/bin/env bash
    set -euo pipefail

    UV_BIN="$(pwd)/uv/target/debug/uv"
    NIX_BIN_DIR="$(dirname "$(readlink -f "$(which nix)")")"
    CA_BUNDLE="$(ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1)"
    GIT_BIN_DIR="$(dirname "$(readlink -f "$(which git)")")"
    GIT_CORE_DIR="$(git --exec-path)"

    exec docker run --rm -it \
        --workdir /work \
        --network host \
        -v "$(pwd):/work" \
        -v "$UV_BIN:/usr/local/bin/uv:ro" \
        -v "/nix:/nix" \
        -v "$NIX_BIN_DIR:/nix-bin:ro" \
        -v "$GIT_BIN_DIR:/git-bin:ro" \
        -v "$GIT_CORE_DIR:/git-core:ro" \
        -e "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/nix-bin:/git-bin" \
        -e "NIX_REMOTE=daemon" \
        -e "NIX_SSL_CERT_FILE=$CA_BUNDLE" \
        -e "SSL_CERT_FILE=$CA_BUNDLE" \
        -e "GIT_SSL_CAINFO=$CA_BUNDLE" \
        -e "GIT_EXEC_PATH=/git-core" \
        "{{image}}"
