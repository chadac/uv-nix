//! uv-nix must patch the environment after *every* sync, not only when uv
//! installs something new.
//!
//! When nothing needs installing, uv's fast path skips the install step
//! entirely, which is where the per-wheel patch hook lives. Packages that were
//! already present but never patched (installed by a stock uv, by an older
//! uv-nix, or left by an interrupted run) stayed broken until someone ran
//! `uv nix patch` manually. These tests set up exactly that state and check
//! that a no-op `uv sync` / `uv pip install` repairs it.

mod common;

use common::runner::UV_BIN;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// A small project with a native dependency whose binaries need Nix rpaths.
fn numpy_project() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("pyproject.toml"),
        r#"[project]
name = "reconcile-test"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = ["numpy"]
"#,
    )
    .unwrap();
    dir
}

fn uv(project: &Path, args: &[&str]) -> Output {
    let output = Command::new(UV_BIN.as_path())
        .args(args)
        .current_dir(project)
        .env_remove("VIRTUAL_ENV")
        .output()
        .expect("failed to run uv");
    assert!(
        output.status.success(),
        "uv {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn site_packages(venv: &Path) -> PathBuf {
    std::fs::read_dir(venv.join("lib"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path().join("site-packages"))
        .find(|p| p.is_dir())
        .expect("site-packages")
}

fn processed_json(venv: &Path) -> PathBuf {
    venv.join("share/uv-nix/processed.json")
}

fn processed_has_numpy(venv: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(processed_json(venv)) else {
        return false;
    };
    let entries: Vec<String> = serde_json::from_str(&content).unwrap();
    entries.iter().any(|e| e.starts_with("numpy-"))
}

/// After a no-op sync, packages missing from the processed list are handled.
#[test]
fn noop_sync_processes_untracked_packages() {
    let project = numpy_project();
    let venv = project.path().join(".venv");
    uv(project.path(), &["sync"]);
    assert!(processed_has_numpy(&venv), "install should record numpy");

    std::fs::remove_file(processed_json(&venv)).unwrap();
    uv(project.path(), &["sync"]);
    assert!(
        processed_has_numpy(&venv),
        "no-op `uv sync` should re-process numpy"
    );
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    fn rpath(binary: &Path) -> String {
        let out = Command::new("patchelf")
            .arg("--print-rpath")
            .arg(binary)
            .output()
            .expect("patchelf");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Native binaries under `dir` that uv-nix gave a /nix/store rpath.
    fn nix_patched_binaries(dir: &Path) -> Vec<PathBuf> {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| p.to_string_lossy().contains(".so"))
            .filter(|p| rpath(p).contains("/nix/store"))
            .collect()
    }

    /// Put the environment back into the state a stock uv would leave it in:
    /// nix rpath entries gone and no record of uv-nix having run.
    fn unpatch(venv: &Path, binaries: &[PathBuf]) {
        for binary in binaries {
            let kept: Vec<String> = rpath(binary)
                .split(':')
                .filter(|p| !p.is_empty() && !p.starts_with("/nix/store"))
                .map(str::to_string)
                .collect();
            let status = Command::new("patchelf")
                .arg("--set-rpath")
                .arg(kept.join(":"))
                .arg(binary)
                .status()
                .unwrap();
            assert!(status.success());
            assert!(!rpath(binary).contains("/nix/store"));
        }
        let _ = std::fs::remove_file(processed_json(venv));
        let _ = std::fs::remove_file(venv.join("share/uv-nix/patches.json"));
    }

    fn assert_repatched(binaries: &[PathBuf], after: &str) {
        for binary in binaries {
            assert!(
                rpath(binary).contains("/nix/store"),
                "{} was not re-patched after {after}",
                binary.display()
            );
        }
    }

    #[test]
    fn noop_sync_patches_unpatched_binaries() {
        let project = numpy_project();
        let venv = project.path().join(".venv");
        uv(project.path(), &["sync"]);

        let binaries = nix_patched_binaries(&site_packages(&venv).join("numpy"));
        assert!(
            !binaries.is_empty(),
            "expected uv-nix to patch at least one numpy binary on install"
        );

        unpatch(&venv, &binaries);
        let out = uv(project.path(), &["sync"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("Installed"),
            "second sync should be a no-op, got:\n{stderr}"
        );
        assert_repatched(&binaries, "no-op `uv sync`");

        unpatch(&venv, &binaries);
        uv(project.path(), &["run", "python", "-c", "import numpy"]);
        assert_repatched(&binaries, "`uv run`");

        unpatch(&venv, &binaries);
        let python = venv.join("bin/python");
        uv(
            project.path(),
            &[
                "pip",
                "install",
                "--python",
                python.to_str().unwrap(),
                "numpy",
            ],
        );
        assert_repatched(&binaries, "no-op `uv pip install`");
    }
}
