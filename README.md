# uv-nix

Patch for uv for hassle-free venv management using Nix.

Using `uv` with Nix dev envs (or just Python in general) can be a
hassle when using libraries that depend on system packages. Nix can
solve this, but tools like
[uv2nix](https://github.com/pyproject-nix/uv2nix) replace `uv` rather
than layering on top -- which means you lose some of the speed
advantages.

This project patches `uv` to Nix-aware by:

* Patching the Python binaries that `uv` provides:
  * Libraries like `zlib` and such use Nix-provided versions
  * It also hooks up the `ctypes` module with uv-nix so `find_library`
    and such work
* Patching Python wheels with patchelf to link directly to Nix-supplied
  libraries. No need for `LD_LIBRARY_PATH` hacks.
* It also updates source builds to use Nix libraries (TODO is to make
  source/wheel builds pure derivations if desired as well)

These all are sourced preferentially from your
`flake.nix`/devenv/flox/devbox configurations as well if you have
them.

*WARNING: USE AT YOUR OWN RISK!* Since this is patching `uv`, it means
that it could at any time break.

## Installation

This can be installed like any other Nix package.

```bash
nix profile install github:chadac/uv-nix
```

WARNING: The default installer is just a repackaged binary! If you
want to do a proper source build, use:

```bash
nix profile install github:chadac/uv-nix#build
```

### One-time use

If you don't want to use uv-nix exclusively but just want to patch an
existing venv/one-time sync a venv, use:

```bash
nix run github:chadac/uv-nix -- patch
```

## Usage

You shouldn't need to change your workflow at all -- this will
integrate with your flake and uv project without any interference.

If you are importing this into an existing project, you can run:

```bash
uv nix rebuild
```

To recreate all the venv with all your Nix-patched artifacts.

## Configuration

### pyproject.toml

You can configure uv-nix behavior via the `[tool.uv-nix]` section in
your `pyproject.toml`:

```toml
[tool.uv-nix]
# Extra nixpkgs packages to include in RPATH and build environment.
# These are nixpkgs attribute paths that will be resolved and linked.
extra-libraries = [
    "libGL",                      # For OpenGL support (PyOpenGL, etc.)
    "cudaPackages.cudatoolkit",   # For CUDA support
    "ffmpeg",                     # For audio/video processing
    # Platform-specific libraries use object syntax:
    { pkg = "libdrm", platforms = ["*-linux"] },
    { pkg = "darwin.apple_sdk.frameworks.Metal", platforms = ["*-darwin"] },
]

# Optional: Pin to a specific nixpkgs commit (overrides auto-detection)
nixpkgs = "github:NixOS/nixpkgs/a3c0b3b21515f74fd2665903d4ce6f4d83838dde"
```

#### `extra-libraries`

A list of nixpkgs attribute paths to include when patching binaries
and during source builds. Each entry can be:

- A simple string (e.g., `"libGL"`) - applies to all platforms
- An object with `pkg` and `platforms` fields for platform-specific libraries

Platform patterns:
- `"*-linux"` - matches all Linux systems (x86_64-linux, aarch64-linux)
- `"*-darwin"` - matches all macOS systems (aarch64-darwin, x86_64-darwin)
- `"x86_64-linux"` - matches only that specific system

These libraries will be:

1. Added to RPATH when patching `.so` files in wheels
2. Added to `LIBRARY_PATH`, `C_INCLUDE_PATH`, and `PKG_CONFIG_PATH`
   during source builds

Common examples:

| Library | Use case |
|---------|----------|
| `libGL` | OpenGL support (PyOpenGL, OpenCV, etc.) |
| `cudaPackages.cudatoolkit` | CUDA support for ML frameworks |
| `ffmpeg` | Audio/video processing (moviepy, etc.) |
| `libpq` | PostgreSQL support (psycopg2) |
| `openssl` | SSL/TLS support |
| `zlib` | Compression support |
#### `nixpkgs`

By default, uv-nix auto-detects your nixpkgs from:
1. Your `flake.nix` inputs
2. Your `devenv.yaml` inputs
3. Falls back to `github:NixOS/nixpkgs/nixpkgs-unstable`

You can override this with an explicit flake reference:

```toml
[tool.uv-nix]
nixpkgs = "github:NixOS/nixpkgs/a3c0b3b21515f74fd2665903d4ce6f4d83838dde"
```

### Per-package configuration

For fine-grained control over individual packages, use `[[tool.uv-nix.package]]`
array tables:

```toml
[[tool.uv-nix.package]]
name = "psycopg2"
# Override the default library list entirely
libraries = ["postgresql_17"]
# Add extra build tools for source builds
extra-build-tools = ["gcc"]
# Pin to a specific nixpkgs commit for this package
nixpkgs = "github:NixOS/nixpkgs/a3c0b3b21515f74fd2665903d4ce6f4d83838dde"
[[tool.uv-nix.package]]
name = "pillow"
# Add libraries on top of defaults (from package-build-libs.json)
extra-libraries = [
  "libheif",
  "libavif",
  # Platform-specific libraries use object syntax with platforms filter
  { pkg = "libdrm", platforms = ["*-linux"] },
  { pkg = "darwin.apple_sdk.frameworks.Accelerate", platforms = ["*-darwin"] },
]
```

#### Per-package options

| Option | Description |
|--------|-------------|
| `name` | Package name (required) |
| `libraries` | Replace default libraries entirely |
| `extra-libraries` | Add libraries to defaults (string or `{pkg, platforms}` object) |
| `extra-build-tools` | Additional build tools (cargo, cmake, etc.) |
| `nixpkgs` | Per-package nixpkgs override |

#### Viewing package configuration

Use `uv nix info --package` to see the effective build configuration:

```bash
$ uv nix info --package psycopg2
Package: psycopg2

Custom config: no (using defaults)

Nixpkgs source:
  FlakeLock { rev: "abc123..." }

Libraries:
  libpq

Build tools:
  libpq.pg_config
```

### Built-in package support

uv-nix includes built-in support for common packages that require
native libraries. These are automatically resolved without needing
`extra-libraries` configuration:

- **psycopg2**: PostgreSQL adapter (libpq + pg_config)
- **pillow**: Image processing (libjpeg, libpng, zlib, etc.)
- **cryptography**: Crypto libraries (openssl)
- **bcrypt**: Password hashing (cargo for build)
- **lxml**: XML processing (libxml2, libxslt)
- **numpy/scipy**: Scientific computing (blas, lapack)
- And many more...

See `data/package-build-libs.json` for the full list.

## CLI Commands

### `uv nix info [path]`

Show information about patched packages in a virtual environment.

```bash
uv nix info                    # Use .venv
uv nix info /path/to/venv      # Specify path
uv nix info --show-details     # Show individual binaries and RPATH
uv nix info --json             # Output as JSON
uv nix info --package numpy    # Show build config for a package
```

### `uv nix patch [path]`

Manually patch ELF binaries in a virtual environment.

```bash
uv nix patch                   # Patch .venv
uv nix patch --only-python     # Only patch Python interpreter
uv nix patch --only-packages   # Only patch installed packages
uv nix patch --packages numpy  # Only patch specific packages
```

### `uv nix rebuild [path]`

Re-patch all native binaries (useful after config changes).

```bash
uv nix rebuild                 # Rebuild .venv
uv nix rebuild --force         # Force rebuild even if up-to-date
uv nix rebuild --packages pkg  # Only rebuild specific packages
```
