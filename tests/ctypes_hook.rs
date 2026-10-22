//! End-to-end checks that `ctypes.util.find_library` resolves nix libraries.
//!
//! Native binaries get nix library paths baked into their RPATH at install
//! time, but a package that loads its library at runtime — `ctypes.CDLL`,
//! `find_library` — has no RPATH to carry. Those lookups go through the hook
//! uv-nix writes into the venv's site-packages, so what the hook can see is
//! the whole of what such a package can load.

mod common;

use common::runner::UV_BIN;
use std::path::Path;
use std::process::Command;

/// `python-magic` is pure Python: it ships no binary of its own and calls
/// `ctypes.CDLL(find_library("magic"))` at import. Nothing in the install can
/// be patched on its behalf, so importing it exercises the hook and only the
/// hook.
const PACKAGE: &str = "python-magic";

/// The nixpkgs attribute providing libmagic.
const LIBRARY_ATTR: &str = "file";

fn project_with_extra_libraries() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("failed to create project dir");
    std::fs::write(
        dir.path().join("pyproject.toml"),
        format!(
            "[project]\n\
             name = \"ctypes-hook-e2e\"\n\
             version = \"0.1.0\"\n\
             requires-python = \">=3.9\"\n\
             dependencies = [\"{PACKAGE}\"]\n\
             \n\
             [tool.uv]\n\
             package = false\n\
             \n\
             [tool.uv-nix]\n\
             extra-libraries = [\"{LIBRARY_ATTR}\"]\n"
        ),
    )
    .expect("failed to write pyproject.toml");
    dir
}

fn uv(project: &Path, args: &[&str]) -> std::process::Output {
    Command::new(UV_BIN.as_path())
        .current_dir(project)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run uv {args:?}: {e}"))
}

/// A library declared in `extra-libraries` must be loadable by name from
/// Python, not merely present in some binary's RPATH.
///
/// Before the hook covered the whole resolved library set, this failed with
/// `ImportError: failed to find libmagic` — the hook was only written when an
/// installed package declared `runtime-libs`, which only matplotlib and
/// pysodium do, so an explicitly declared library was unreachable.
#[test]
fn extra_libraries_are_loadable_via_find_library() {
    let project = project_with_extra_libraries();

    let sync = uv(project.path(), &["sync", "-q"]);
    if !sync.status.success() {
        eprintln!(
            "skipping: uv sync failed (no network?):\n{}",
            String::from_utf8_lossy(&sync.stderr)
        );
        return;
    }

    let output = uv(
        project.path(),
        &[
            "run",
            "python",
            "-c",
            "import ctypes.util as u; print(u.find_library('magic'))",
        ],
    );
    let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(
        output.status.success(),
        "find_library('magic') failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        found.contains("/nix/store"),
        "find_library('magic') did not resolve to a nix library, got {found:?}"
    );

    // The real goal: the package that calls it actually imports and works.
    let output = uv(
        project.path(),
        &[
            "run",
            "python",
            "-c",
            "import magic; print(magic.from_buffer(b'hello'))",
        ],
    );
    assert!(
        output.status.success(),
        "`import magic` failed even though libmagic was declared in \
         extra-libraries:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("text"),
        "libmagic loaded but did not identify a text buffer: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}
