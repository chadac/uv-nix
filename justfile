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

# Persistent test cache dir (Python install + nix config survive across runs)
test-cache := ".cache/test"

# Run tests inside a Docker container (isolated, no host side effects)
test *ARGS="-m 'not docker and not slow'": build
    #!/usr/bin/env bash
    set -euo pipefail

    UV_BIN="$(pwd)/uv/target/debug/uv"
    NIX_BIN_DIR="$(dirname "$(readlink -f "$(which nix)")")"
    CA_BUNDLE="$(ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1)"
    GIT_BIN_DIR="$(dirname "$(readlink -f "$(which git)")")"
    GIT_CORE_DIR="$(git --exec-path)"
    PYTEST="$(nix-shell -p python3 python3Packages.pytest python3Packages.pytest-xdist --run 'readlink -f $(which pytest)')"

    # Persistent cache for Python installs + nix config
    mkdir -p "{{test-cache}}/python" "{{test-cache}}/uv-nix-config"

    docker run --rm \
        --workdir /work \
        --network host \
        -v "$(pwd):/work:ro" \
        -v "$UV_BIN:/usr/local/bin/uv:ro" \
        -v "/nix:/nix" \
        -v "$NIX_BIN_DIR:/nix-bin:ro" \
        -v "$GIT_BIN_DIR:/git-bin:ro" \
        -v "$GIT_CORE_DIR:/git-core:ro" \
        -v "$(pwd)/{{test-cache}}/python:/tmp/uv-nix-test-python" \
        -v "$(pwd)/{{test-cache}}/uv-nix-config:/root/.cache/uv-nix" \
        -e "UV_BIN=/usr/local/bin/uv" \
        -e "UV_NIX_TEST_PYTHON_DIR=/tmp/uv-nix-test-python" \
        -e "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin" \
        -e "NIX_REMOTE=daemon" \
        -e "NIX_SSL_CERT_FILE=$CA_BUNDLE" \
        -e "SSL_CERT_FILE=$CA_BUNDLE" \
        -e "GIT_SSL_CAINFO=$CA_BUNDLE" \
        -e "GIT_EXEC_PATH=/git-core" \
        busybox \
        "$PYTEST" tests/ -v -n auto {{ARGS}}

# Clear persistent test cache (forces re-download of Python etc.)
test-clean:
    rm -rf {{test-cache}}

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
