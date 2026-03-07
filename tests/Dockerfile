# Minimal container for testing patched Python binaries.
# Usage: build context should include the /nix/store paths needed at runtime.
# The test harness constructs this dynamically.
FROM scratch

# The test copies in the required /nix/store closures
# and the patched Python installation at build time.
# See test_patch_python.py for the dynamic build logic.
