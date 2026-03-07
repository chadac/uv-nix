# Build the patched uv binary using cargo (fast, for development)
build:
    bash scripts/apply-patches.sh
    cd uv && cargo build --package uv --no-default-features --features "uv-distribution/static,test-defaults"

# Copy the full workspace Cargo.lock (needed before nix build)
sync-lockfile:
    cp uv/Cargo.lock Cargo.lock

# Full nix build (produces self-contained binary)
nix-build: sync-lockfile
    nix build

# Run host tests with cargo-built binary (fast iteration, no Docker)
test-fast: build
    #!/usr/bin/env bash
    set -euo pipefail
    export UV_BIN="$(pwd)/uv/target/debug/uv"
    nix-shell -p python3 python3Packages.pytest python3Packages.docker \
        --run "pytest tests/ -v -m 'not docker and not slow'"

# Run all tests including Docker isolation tests (requires Docker/Podman)
test-docker: build
    #!/usr/bin/env bash
    set -euo pipefail
    export UV_BIN="$(pwd)/uv/target/debug/uv"
    nix-shell -p python3 python3Packages.pytest python3Packages.docker \
        --run "pytest tests/ -v -m 'not slow'"

# Run all tests including slow ones (pandas, scipy) + Docker
test-all: build
    #!/usr/bin/env bash
    set -euo pipefail
    export UV_BIN="$(pwd)/uv/target/debug/uv"
    nix-shell -p python3 python3Packages.pytest python3Packages.docker \
        --run "pytest tests/ -v"

# Run tests against the nix-built binary
test: nix-build
    #!/usr/bin/env bash
    set -euo pipefail
    export UV_BIN="$(pwd)/result/bin/uv"
    nix-shell -p python3 python3Packages.pytest python3Packages.docker \
        --run "pytest tests/ -v -m 'not docker'"

# Check local Rust crate compiles
check:
    cargo check

# Spawn an interactive Docker container with uv + nix on PATH and /nix mounted
# Usage: just docker [image]
docker image="alpine:latest": build
    #!/usr/bin/env bash
    set -euo pipefail

    UV_BIN="$(pwd)/uv/target/debug/uv"
    NIX_BUILD_BIN="$(readlink -f "$(which nix-build)")"
    NIX_BIN_DIR="$(dirname "$NIX_BUILD_BIN")"

    # Mount /nix (store + daemon socket), nix binaries, and uv binary
    docker_args=(
        run --rm -it
        --workdir /work
        -v "$(pwd):/work:ro"
        -v "$UV_BIN:/usr/local/bin/uv:ro"
        -v "/nix:/nix"
        -v "$NIX_BIN_DIR:/nix-bin:ro"
        -e "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/nix-bin"
        -e "NIX_REMOTE=daemon"
    )

    docker_args+=("{{image}}")

    exec docker "${docker_args[@]}"
