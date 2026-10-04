//! End-to-end checks for `uv nix patch` on an existing virtual environment.
//!
//! `uv nix patch` exists to patch a venv uv-nix did not install into, so these
//! tests strip the RPATHs that install-time patching would have written and
//! then patch from a pristine state — the condition the command is for.

mod common;

use common::runner::UV_BIN;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A native wheel that is small and has exactly one extension module, so the
/// RPATH assertions below read clearly.
const PACKAGE: &str = "markupsafe";

fn patchelf() -> Option<PathBuf> {
    let out = Command::new("sh")
        .args(["-c", "command -v patchelf"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Create a project with one native dependency and sync it.
fn synced_project() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().expect("failed to create project dir");
    std::fs::write(
        dir.path().join("pyproject.toml"),
        format!(
            "[project]\n\
             name = \"nix-patch-e2e\"\n\
             version = \"0.1.0\"\n\
             requires-python = \">=3.9\"\n\
             dependencies = [\"{PACKAGE}\"]\n\
             \n\
             [tool.uv]\n\
             package = false\n"
        ),
    )
    .expect("failed to write pyproject.toml");

    let status = Command::new(UV_BIN.as_path())
        .current_dir(dir.path())
        .args(["sync", "-q"])
        .status()
        .expect("failed to run uv sync");

    status.success().then_some(dir)
}

/// Every extension module in the venv's site-packages.
fn extension_modules(venv: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(venv.join("lib"))
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.extension().is_some_and(|ext| ext == "so"))
        .collect()
}

fn rpath_entries(patchelf: &Path, binary: &Path) -> Vec<String> {
    let out = Command::new(patchelf)
        .arg("--print-rpath")
        .arg(binary)
        .output()
        .expect("failed to run patchelf --print-rpath");
    assert!(
        out.status.success(),
        "patchelf --print-rpath failed on {}",
        binary.display()
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn strip_rpaths(patchelf: &Path, binaries: &[PathBuf]) {
    for binary in binaries {
        let status = Command::new(patchelf)
            .arg("--remove-rpath")
            .arg(binary)
            .status()
            .expect("failed to run patchelf --remove-rpath");
        assert!(status.success(), "failed to strip {}", binary.display());
    }
}

/// `uv nix patch` must give a package binary the same minimal, soname-derived
/// RPATH as `--only-packages`.
///
/// It used to derive a libpython directory as `<python>/../../lib`, which for a
/// venv is site-packages, and `find_native_binaries` walks recursively — so the
/// default flags bulk-patched every extension module with the whole nix library
/// map (13 entries instead of 2 for markupsafe) and the targeted pass then
/// skipped them as already patched.
#[cfg(target_os = "linux")]
#[test]
fn default_patch_does_not_apply_the_global_rpath_to_packages() {
    let Some(patchelf) = patchelf() else {
        eprintln!("skipping: patchelf not on PATH (not in the nix dev shell)");
        return;
    };

    let mut rpaths = Vec::new();
    for args in [
        vec!["nix", "patch"],
        vec!["nix", "patch", "--only-packages"],
    ] {
        let Some(project) = synced_project() else {
            eprintln!("skipping: uv sync failed (no network?)");
            return;
        };
        let venv = project.path().join(".venv");

        let binaries = extension_modules(&venv);
        assert!(
            !binaries.is_empty(),
            "{PACKAGE} installed no extension module to patch"
        );
        strip_rpaths(&patchelf, &binaries);

        let output = Command::new(UV_BIN.as_path())
            .current_dir(project.path())
            .args(&args)
            .output()
            .expect("failed to run uv nix patch");
        assert!(
            output.status.success(),
            "uv {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        rpaths.push(rpath_entries(&patchelf, &binaries[0]));
    }

    let (default, only_packages) = (&rpaths[0], &rpaths[1]);
    assert_eq!(
        default,
        only_packages,
        "`uv nix patch` applied a different RPATH than `--only-packages`:\n\
         default ({} entries): {default:#?}\n\
         --only-packages ({} entries): {only_packages:#?}",
        default.len(),
        only_packages.len(),
    );
}

/// Replace the venv's interpreter symlink with a real copy of the binary.
///
/// This is the shape of a venv built on a Python that is not nix-provided —
/// a system or pyenv install, or uv's managed cache — without needing one on
/// the test machine. `pyvenv.cfg` still points `home` at the real prefix, so
/// the environment keeps working; only the interpreter uv would patch moves
/// out of the store.
fn delocate_interpreter(venv: &Path) -> PathBuf {
    let python = venv.join("bin/python3");
    let real = std::fs::canonicalize(&python).expect("venv has no interpreter");
    assert!(
        real.starts_with("/nix/store"),
        "expected a store interpreter to start from, got {}",
        real.display()
    );

    for name in [
        "python",
        "python3",
        "python3.12",
        "python3.13",
        "python3.14",
    ] {
        let link = venv.join("bin").join(name);
        if link.exists() || link.is_symlink() {
            std::fs::remove_file(&link).ok();
        }
    }
    std::fs::copy(&real, &python).expect("failed to copy the interpreter into the venv");
    let mut perms = std::fs::metadata(&python).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&python, perms).unwrap();
    python
}

/// A venv already on a nixpkgs interpreter needs no rebuild, and the store
/// path it points at must not be written to.
#[test]
fn nix_interpreter_is_left_alone() {
    let Some(project) = synced_project() else {
        eprintln!("skipping: uv sync failed (no network?)");
        return;
    };
    let venv = project.path().join(".venv");
    let before = std::fs::canonicalize(venv.join("bin/python3")).expect("no interpreter");
    let stat = std::fs::metadata(&before).expect("cannot stat interpreter");

    let output = Command::new(UV_BIN.as_path())
        .current_dir(project.path())
        .args(["nix", "patch"])
        .output()
        .expect("failed to run uv nix patch");
    assert!(
        output.status.success(),
        "uv nix patch failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = std::fs::canonicalize(venv.join("bin/python3")).expect("no interpreter");
    assert_eq!(before, after, "the interpreter was replaced unnecessarily");
    let stat_after = std::fs::metadata(&after).expect("cannot stat interpreter");
    assert_eq!(
        (stat.len(), stat.modified().ok()),
        (stat_after.len(), stat_after.modified().ok()),
        "uv nix patch rewrote the store interpreter at {}",
        after.display()
    );
}

/// A venv on a non-nix interpreter is rebuilt on a nixpkgs one. The offending
/// interpreter must be left byte-identical — it is replaced, never patched.
#[test]
fn non_nix_interpreter_is_replaced_not_patched() {
    let Some(project) = synced_project() else {
        eprintln!("skipping: uv sync failed (no network?)");
        return;
    };
    let venv = project.path().join(".venv");
    let delocated = delocate_interpreter(&venv);
    let before = std::fs::read(&delocated).expect("cannot read the delocated interpreter");

    let output = Command::new(UV_BIN.as_path())
        .current_dir(project.path())
        .args(["nix", "patch", "--recreate"])
        .output()
        .expect("failed to run uv nix patch --recreate");
    assert!(
        output.status.success(),
        "uv nix patch --recreate failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The venv is back on a nixpkgs interpreter...
    let rebuilt = std::fs::canonicalize(venv.join("bin/python3"))
        .expect("the rebuilt venv has no interpreter");
    assert!(
        rebuilt.starts_with("/nix/store"),
        "expected a nixpkgs interpreter after rebuilding, got {}",
        rebuilt.display()
    );

    // ...and the interpreter that was there was not rewritten. `uv venv
    // --clear` removes it, so either it is gone or it is unchanged; what must
    // never happen is patchelf having edited it in place.
    if let Ok(after) = std::fs::read(&delocated) {
        assert_eq!(
            before,
            after,
            "the non-nix interpreter at {} was patched in place",
            delocated.display()
        );
    }

    // The packages came back.
    let output = Command::new(UV_BIN.as_path())
        .current_dir(project.path())
        .args(["pip", "list", "--python"])
        .arg(venv.join("bin/python3"))
        .output()
        .expect("failed to run uv pip list");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(PACKAGE),
        "{PACKAGE} was not reinstalled after the rebuild:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Rebuilding is destructive, so without consent it must refuse and change
/// nothing. `confirm` falls back to its default when stderr is not a terminal,
/// which is what a CI run or a piped invocation looks like.
#[test]
fn rebuild_without_consent_refuses_and_changes_nothing() {
    let Some(project) = synced_project() else {
        eprintln!("skipping: uv sync failed (no network?)");
        return;
    };
    let venv = project.path().join(".venv");
    let delocated = delocate_interpreter(&venv);
    let before = std::fs::read(&delocated).expect("cannot read the delocated interpreter");

    let output = Command::new(UV_BIN.as_path())
        .current_dir(project.path())
        .args(["nix", "patch"])
        .output()
        .expect("failed to run uv nix patch");
    assert!(
        !output.status.success(),
        "uv nix patch should refuse to rebuild without --recreate"
    );

    let after = std::fs::read(&delocated).expect("the interpreter was removed without consent");
    assert_eq!(
        before, after,
        "the interpreter was modified without consent"
    );
    assert!(
        !std::fs::canonicalize(venv.join("bin/python3"))
            .expect("no interpreter")
            .starts_with("/nix/store"),
        "the venv was rebuilt without consent"
    );
}
