"""Tests for Python interpreter patching via Nix derivation.

Each test runs in an isolated Docker container (busybox + nix mounts).
"""

import pytest

from conftest import run_in_container


class TestPatchPythonInstall:
    """Test that `uv python install` produces a correctly patched Python."""

    def test_install_and_nix_ref(self):
        """After install, a .nix sibling should reference /nix/store/."""
        result = run_in_container("""
            cd /tmp
            uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            NIXREF="${PYDIR}.nix"
            test -L "$NIXREF" && readlink "$NIXREF" | grep -q '^/nix/store/' && echo "NIX_REF_OK"
            test -d "$PYDIR" && ! test -L "$PYDIR" && echo "DIR_OK"
            test -L "$PYDIR/bin/python3.12" && echo "SYMLINK_OK"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "NIX_REF_OK" in result.stdout, f"No .nix ref:\n{result.stdout}"
        assert "DIR_OK" in result.stdout, f"Dir not writable:\n{result.stdout}"
        assert "SYMLINK_OK" in result.stdout, f"Files not symlinked:\n{result.stdout}"

    def test_patched_python_runs(self):
        """The patched Python interpreter should be executable."""
        result = run_in_container("""
            cd /tmp && uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            "$PYDIR/bin/python3.12" -c "import sys; print(sys.version)"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "3.12" in result.stdout

    def test_ssl_module_works(self):
        """The patched Python should be able to import ssl."""
        result = run_in_container("""
            cd /tmp && uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            "$PYDIR/bin/python3.12" -c "import ssl; print(ssl.OPENSSL_VERSION)"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "OpenSSL" in result.stdout

    def test_stdlib_imports(self):
        """Key stdlib modules including C extensions should import."""
        result = run_in_container("""
            cd /tmp && uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            "$PYDIR/bin/python3.12" -c "
import os, sys, json, ssl, sqlite3, ctypes, hashlib, zlib
print('stdlib ok')
"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "stdlib ok" in result.stdout
