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

# Check local Rust crate compiles
check:
    cargo check

# Run tests inside a Docker container (isolated, no host side effects)
test-fast: build
    #!/usr/bin/env bash
    set -euo pipefail

    UV_BIN="$(pwd)/uv/target/debug/uv"
    NIX_BIN_DIR="$(dirname "$(readlink -f "$(which nix)")")"
    CA_BUNDLE="$(ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1)"
    GIT_BIN="$(readlink -f "$(which git)")"
    GIT_BIN_DIR="$(dirname "$GIT_BIN")"
    GIT_CORE_DIR="$(git --exec-path)"
    PYTEST_BIN="$(nix-shell -p python3 python3Packages.pytest --run 'which pytest')"
    PYTEST_DIR="$(dirname "$(readlink -f "$PYTEST_BIN")")"
    PYTHON_BIN="$(nix-shell -p python3 --run 'which python3')"
    PYTHON_DIR="$(dirname "$(readlink -f "$PYTHON_BIN")")"

    docker run --rm \
        --workdir /work \
        --network host \
        -v "$(pwd):/work:ro" \
        -v "$UV_BIN:/usr/local/bin/uv:ro" \
        -v "/nix:/nix" \
        -v "$NIX_BIN_DIR:/nix-bin:ro" \
        -v "$GIT_BIN_DIR:/git-bin:ro" \
        -v "$GIT_CORE_DIR:/git-core:ro" \
        -e "UV_BIN=/usr/local/bin/uv" \
        -e "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin:$PYTEST_DIR:$PYTHON_DIR" \
        -e "NIX_REMOTE=daemon" \
        -e "NIX_SSL_CERT_FILE=$CA_BUNDLE" \
        -e "SSL_CERT_FILE=$CA_BUNDLE" \
        -e "GIT_SSL_CAINFO=$CA_BUNDLE" \
        -e "GIT_EXEC_PATH=/git-core" \
        busybox \
        pytest tests/ -v -x -m 'not docker and not slow'

# Run all tests including slow ones
test-all: build
    #!/usr/bin/env bash
    set -euo pipefail

    UV_BIN="$(pwd)/uv/target/debug/uv"
    NIX_BIN_DIR="$(dirname "$(readlink -f "$(which nix)")")"
    CA_BUNDLE="$(ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1)"
    GIT_BIN="$(readlink -f "$(which git)")"
    GIT_BIN_DIR="$(dirname "$GIT_BIN")"
    GIT_CORE_DIR="$(git --exec-path)"
    PYTEST_BIN="$(nix-shell -p python3 python3Packages.pytest --run 'which pytest')"
    PYTEST_DIR="$(dirname "$(readlink -f "$PYTEST_BIN")")"
    PYTHON_BIN="$(nix-shell -p python3 --run 'which python3')"
    PYTHON_DIR="$(dirname "$(readlink -f "$PYTHON_BIN")")"

    docker run --rm \
        --workdir /work \
        --network host \
        -v "$(pwd):/work:ro" \
        -v "$UV_BIN:/usr/local/bin/uv:ro" \
        -v "/nix:/nix" \
        -v "$NIX_BIN_DIR:/nix-bin:ro" \
        -v "$GIT_BIN_DIR:/git-bin:ro" \
        -v "$GIT_CORE_DIR:/git-core:ro" \
        -e "UV_BIN=/usr/local/bin/uv" \
        -e "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin:$PYTEST_DIR:$PYTHON_DIR" \
        -e "NIX_REMOTE=daemon" \
        -e "NIX_SSL_CERT_FILE=$CA_BUNDLE" \
        -e "SSL_CERT_FILE=$CA_BUNDLE" \
        -e "GIT_SSL_CAINFO=$CA_BUNDLE" \
        -e "GIT_EXEC_PATH=/git-core" \
        busybox \
        pytest tests/ -v -x

# Spawn an interactive Docker container with uv + nix on PATH
# Usage: just docker [image]
docker image="busybox": build
    #!/usr/bin/env bash
    set -euo pipefail

    UV_BIN="$(pwd)/uv/target/debug/uv"
    NIX_BIN_DIR="$(dirname "$(readlink -f "$(which nix)")")"
    CA_BUNDLE="$(ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1)"
    GIT_BIN="$(readlink -f "$(which git)")"
    GIT_BIN_DIR="$(dirname "$GIT_BIN")"
    GIT_CORE_DIR="$(git --exec-path)"

    exec docker run --rm -it \
        --workdir /work \
        --network host \
        -v "$(pwd):/work:ro" \
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
