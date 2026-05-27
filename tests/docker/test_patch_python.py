"""Tests for Python interpreter patching via patchelf + nix.

Each test runs in an isolated Docker container (busybox + nix mounts).
"""

from conftest import run_in_container


class TestPatchPythonInstall:
    """Test that `uv python install` produces a correctly patched Python."""

    def test_install_and_elf_patched(self):
        """After install, the python binary's interpreter should point to /nix/store/."""
        result = run_in_container("""
            cd /tmp
            uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            PYBIN="$PYDIR/bin/python3.12"
            # The patched binary's ELF interpreter should be in /nix/store
            # Read the .interp section by finding the PT_INTERP string in the binary
            strings "$PYBIN" | grep -q '^/nix/store/.*/ld-linux' && echo "INTERP_OK"
            # Check the directory is writable (not a nix store path)
            test -d "$PYDIR" && ! test -L "$PYDIR" && echo "DIR_OK"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "INTERP_OK" in result.stdout, f"No nix interpreter:\n{result.stdout}\n{result.stderr}"
        assert "DIR_OK" in result.stdout, f"Dir issue:\n{result.stdout}\n{result.stderr}"

    def test_ctypes_hook_installed(self):
        """After install, the ctypes hook should be present in site-packages."""
        result = run_in_container("""
            cd /tmp
            uv python install 3.12
            PYDIR=$(ls -d /root/.local/share/uv/python/cpython-3.12.* 2>/dev/null | head -1)
            SP=$(ls -d "$PYDIR"/lib/python3.12/site-packages 2>/dev/null | head -1)
            test -f "$SP/_uv_nix_ctypes_hook.py" && echo "HOOK_OK"
            test -f "$SP/uv-nix.pth" && echo "PTH_OK"
            test -f "$SP/_uv_nix_libs.conf" && echo "CONF_OK"
        """)
        assert result.returncode == 0, f"Failed:\n{result.stderr}"
        assert "HOOK_OK" in result.stdout, f"No ctypes hook:\n{result.stdout}\n{result.stderr}"
        assert "PTH_OK" in result.stdout, f"No .pth file:\n{result.stdout}\n{result.stderr}"
        assert "CONF_OK" in result.stdout, f"No libs.conf:\n{result.stdout}\n{result.stderr}"

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
