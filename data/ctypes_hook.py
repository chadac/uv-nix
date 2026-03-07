"""
uv-nix ctypes hook — auto-loaded via uv-nix.pth

Monkey-patches ctypes so that dlopen() and find_library() search
Nix store library paths in addition to the standard system locations.
"""

import ctypes
import ctypes.util
import sys
from pathlib import Path

_nix_lib_dirs = []
_custom_resolvers = {}


def _load_lib_dirs():
    """Build the list of Nix library directories from config file."""
    dirs = []
    seen = set()

    # Read baked-in config from site-packages
    conf = Path(__file__).with_name("_uv_nix_libs.conf")
    if conf.is_file():
        for line in conf.read_text().splitlines():
            line = line.strip()
            if line and line not in seen:
                seen.add(line)
                dirs.append(line)

    return dirs


def _resolve_in_nix_dirs(name):
    """Try to find a library file in the Nix directories.

    Handles both bare names ("libz.so") and unqualified names ("z").
    Returns the full path as a string, or None.
    """
    if not name:
        return None

    # Check custom resolvers first
    if name in _custom_resolvers:
        result = _custom_resolvers[name](name)
        if result:
            return result

    # If name contains a slash, it's already a path — don't search
    if "/" in name:
        return None

    # Build candidate filenames
    candidates = [name]
    if not name.startswith("lib"):
        candidates.append(f"lib{name}.so")
    if ".so" not in name:
        candidates.append(f"{name}.so")

    for d in _nix_lib_dirs:
        dirpath = Path(d)
        if not dirpath.is_dir():
            continue
        for candidate in candidates:
            full = dirpath / candidate
            if full.is_file():
                return str(full)
            # Also try versioned .so (e.g., libz.so.1)
            if ".so" in candidate:
                for f in dirpath.iterdir():
                    if f.name.startswith(candidate) and f.is_file():
                        return str(f)

    return None


def register_resolver(name, fn):
    """Register a custom resolver function for a specific library name.

    The function receives the library name and should return a path string
    or None to fall through to default resolution.
    """
    _custom_resolvers[name] = fn


# --- Monkey-patching ---

_orig_cdll_init = ctypes.CDLL.__init__


def _patched_cdll_init(self, name=None, *args, **kwargs):
    if name is not None and isinstance(name, str):
        resolved = _resolve_in_nix_dirs(name)
        if resolved is not None:
            name = resolved
    return _orig_cdll_init(self, name, *args, **kwargs)


_orig_find_library = ctypes.util.find_library


def _patched_find_library(name):
    # find_library receives bare names like "z", "ssl", "crypto"
    candidates = [f"lib{name}.so"]
    for d in _nix_lib_dirs:
        dirpath = Path(d)
        if not dirpath.is_dir():
            continue
        for candidate in candidates:
            full = dirpath / candidate
            if full.is_file():
                return str(full)
            # Check versioned .so files
            for f in dirpath.iterdir():
                if f.name.startswith(candidate) and f.is_file():
                    return str(f)

    return _orig_find_library(name)


# Patch _ctypes.dlopen if available
_orig_dlopen = None
try:
    import _ctypes

    _orig_dlopen = _ctypes.dlopen

    def _patched_dlopen(name, *args):
        if name is not None and isinstance(name, str):
            resolved = _resolve_in_nix_dirs(name)
            if resolved is not None:
                name = resolved
        return _orig_dlopen(name, *args)
except (ImportError, AttributeError):
    pass


def _install():
    """Apply all monkey-patches. Called once on import."""
    global _nix_lib_dirs
    _nix_lib_dirs = _load_lib_dirs()

    if not _nix_lib_dirs:
        return

    ctypes.CDLL.__init__ = _patched_cdll_init
    ctypes.util.find_library = _patched_find_library

    if _orig_dlopen is not None:
        try:
            _ctypes.dlopen = _patched_dlopen
        except Exception:
            pass


_install()
