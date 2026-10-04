//! End-to-end checks that Python comes from nixpkgs by default.
//!
//! uv's own default is `python-preference = managed`, which prefers uv's
//! downloaded CPython builds over interpreters found on PATH. uv-nix inverts
//! that: a nixpkgs-provided interpreter wins by default, and uv's bundled
//! builds are only a fallback for versions nixpkgs doesn't provide.
//!
//! These tests drive the real patched `uv` binary end to end. Each one runs
//! with an isolated HOME / cache / `UV_PYTHON_INSTALL_DIR` so a bundled
//! interpreter is present only where a test deliberately puts one.

mod common;

use common::runner::UV_BIN;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use tempfile::TempDir;

/// Bundled version installed for the "bundled Python is also available" tests.
///
/// Deliberately older than the dev shell's nixpkgs interpreter so the two
/// candidates are distinguishable: both satisfy the project's
/// `requires-python`, so whichever uv picks reveals its true preference.
const BUNDLED_VERSION: (u64, u64) = (3, 12);

fn bundled_version_arg() -> String {
    format!("{}.{}", BUNDLED_VERSION.0, BUNDLED_VERSION.1)
}

/// The nixpkgs `python3` visible on PATH (the dev shell's interpreter).
struct NixpkgsPython {
    exe: PathBuf,
    version: (u64, u64),
}

/// Locate the nixpkgs `python3` on PATH, or `None` when not running inside the
/// nix dev shell (in which case the nixpkgs-vs-bundled distinction is moot).
fn nixpkgs_python() -> Option<NixpkgsPython> {
    let output = Command::new("python3")
        .args([
            "-c",
            "import os,sys; print(os.path.realpath(sys.executable)); \
             print(sys.version_info[0], sys.version_info[1])",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let exe = PathBuf::from(lines.next()?.trim());
    let mut parts = lines.next()?.split_whitespace();
    let version = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);

    // Only a store-backed interpreter is a nixpkgs interpreter.
    if !exe.starts_with("/nix/store") {
        return None;
    }

    Some(NixpkgsPython { exe, version })
}

/// Skip (rather than fail) when the environment can't express the distinction:
/// no nixpkgs python3 on PATH, or one no newer than the bundled version.
macro_rules! require_nixpkgs_python {
    () => {
        match nixpkgs_python() {
            Some(python) if python.version > BUNDLED_VERSION => python,
            Some(python) => {
                eprintln!(
                    "skipping: nixpkgs python3 {}.{} is not newer than bundled {}",
                    python.version.0,
                    python.version.1,
                    bundled_version_arg()
                );
                return;
            }
            None => {
                eprintln!("skipping: no /nix/store python3 on PATH (not in the nix dev shell)");
                return;
            }
        }
    };
}

/// Install a bundled (uv-managed) CPython into a fresh directory.
fn install_bundled_python() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create bundled python dir");

    let output = Command::new(UV_BIN.as_path())
        .args(["python", "install", &bundled_version_arg()])
        .env("HOME", dir.path())
        .env("UV_PYTHON_INSTALL_DIR", dir.path().join("python"))
        .env("UV_CACHE_DIR", dir.path().join("cache"))
        .output()
        .expect("bundled python install failed to execute");

    assert!(
        output.status.success(),
        "bundled python install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    dir
}

/// A bundled CPython shared read-only by the tests that only need one to
/// *exist*. Tests that assert on the install's contents must not use this —
/// they run in parallel against the same directory.
static BUNDLED_PYTHON: LazyLock<TempDir> = LazyLock::new(install_bundled_python);

/// Path of the shared bundled install directory.
fn bundled_python_dir() -> PathBuf {
    BUNDLED_PYTHON.path().join("python")
}

/// A throwaway project plus the isolated state dirs uv runs against.
struct Project {
    dir: TempDir,
    state: TempDir,
}

impl Project {
    /// Create a project whose `requires-python` both candidate interpreters
    /// satisfy, so the one uv picks reflects preference and nothing else.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("failed to create project dir");
        let state = tempfile::tempdir().expect("failed to create state dir");

        // `package = false` keeps `uv run` from trying to build the project —
        // these tests only care about interpreter selection.
        std::fs::write(
            dir.path().join("pyproject.toml"),
            format!(
                "[project]\n\
                 name = \"nixpkgs-python-e2e\"\n\
                 version = \"0.1.0\"\n\
                 requires-python = \">={}\"\n\
                 \n\
                 [tool.uv]\n\
                 package = false\n",
                bundled_version_arg()
            ),
        )
        .expect("failed to write pyproject.toml");

        Self { dir, state }
    }

    /// A `uv` invocation with fully isolated state and *no* bundled Python.
    fn uv(&self) -> Command {
        self.uv_with_python_dir(&self.state.path().join("python"))
    }

    /// A `uv` invocation that can see the shared bundled Python.
    fn uv_with_bundled(&self) -> Command {
        self.uv_with_python_dir(&bundled_python_dir())
    }

    fn uv_with_python_dir(&self, python_dir: &Path) -> Command {
        let mut cmd = Command::new(UV_BIN.as_path());
        cmd.current_dir(self.dir.path())
            // Isolate every source of interpreter state, but keep PATH so the
            // dev shell's nixpkgs python3 stays discoverable.
            .env("HOME", self.state.path())
            .env("XDG_DATA_HOME", self.state.path().join("share"))
            .env("XDG_CACHE_HOME", self.state.path().join("xdg-cache"))
            .env("UV_PYTHON_INSTALL_DIR", python_dir)
            .env("UV_CACHE_DIR", self.state.path().join("cache"))
            .env_remove("VIRTUAL_ENV")
            .env_remove("CONDA_PREFIX")
            .env_remove("UV_PYTHON")
            .env_remove("UV_PYTHON_PREFERENCE")
            .env_remove("UV_PYTHON_DOWNLOADS");
        cmd
    }

    fn venv_python(&self) -> PathBuf {
        self.dir.path().join(".venv/bin/python3")
    }

    /// Bundled interpreters uv has materialized in this project's own state.
    fn materialized_bundled(&self) -> Vec<String> {
        std::fs::read_dir(self.state.path().join("python"))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|name| !name.ends_with(".nix") && !name.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Assert an interpreter is nixpkgs-provided, not a uv-bundled build.
/// Returns the fully resolved path.
fn assert_nixpkgs_interpreter(label: &str, interpreter: &Path) -> PathBuf {
    let real = std::fs::canonicalize(interpreter)
        .unwrap_or_else(|e| panic!("{label}: cannot resolve {}: {e}", interpreter.display()));

    assert!(
        real.starts_with("/nix/store"),
        "{label}: expected a nixpkgs (/nix/store) interpreter, got {}",
        real.display()
    );
    assert!(
        !real.starts_with(bundled_python_dir()),
        "{label}: uv used its own bundled Python at {}",
        real.display()
    );

    real
}

/// Run a uv command, asserting success and returning stdout.
fn run(label: &str, cmd: &mut Command) -> String {
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("{label}: failed to execute: {e}"));

    assert!(
        output.status.success(),
        "{label}: uv exited with {}:\n{}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Baseline: with no bundled Python anywhere, nixpkgs supplies the interpreter —
/// and specifically the one on PATH.
#[test]
fn nixpkgs_python_used_when_no_bundled_install() {
    let nixpkgs = require_nixpkgs_python!();
    let project = Project::new();

    run("uv venv", project.uv().args(["venv", ".venv"]));

    let resolved = assert_nixpkgs_interpreter("uv venv", &project.venv_python());
    assert_eq!(
        resolved, nixpkgs.exe,
        "uv venv did not use the nixpkgs python3 on PATH"
    );
}

/// Resolving a default interpreter must not download a bundled CPython when
/// nixpkgs already provides a compatible one.
#[test]
fn default_resolution_does_not_download_bundled_python() {
    let _nixpkgs = require_nixpkgs_python!();
    let project = Project::new();

    run("uv venv", project.uv().args(["venv", ".venv"]));

    assert!(
        project.materialized_bundled().is_empty(),
        "uv downloaded a bundled Python instead of using nixpkgs: {:?}",
        project.materialized_bundled()
    );
}

/// The core guarantee: an already-installed bundled Python must NOT displace a
/// compatible nixpkgs interpreter. This is what uv's stock
/// `python-preference = managed` gets wrong.
#[test]
fn nixpkgs_python_wins_over_installed_bundled_python() {
    let _nixpkgs = require_nixpkgs_python!();
    let project = Project::new();

    run("uv venv", project.uv_with_bundled().args(["venv", ".venv"]));

    assert_nixpkgs_interpreter("uv venv (bundled Python installed)", &project.venv_python());
}

/// `uv python find` is the discovery path users and tooling query directly.
#[test]
fn python_find_reports_nixpkgs_over_bundled_python() {
    let _nixpkgs = require_nixpkgs_python!();
    let project = Project::new();

    let stdout = run(
        "uv python find",
        project.uv_with_bundled().args(["python", "find"]),
    );

    assert_nixpkgs_interpreter("uv python find", Path::new(stdout.trim()));
}

/// One file in a tree snapshot: relative path, size, mtime.
type Snapshot = (PathBuf, u64, std::time::SystemTime);

/// Recursive (relative path, size, mtime) snapshot of a tree.
fn tree_snapshot(root: &Path) -> Vec<Snapshot> {
    let mut entries: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let rel = e.path().strip_prefix(root).ok()?.to_path_buf();
            Some((rel, meta.len(), meta.modified().ok()?))
        })
        .collect();
    entries.sort();
    entries
}

/// uv must never rewrite a bundled Python install in place.
///
/// The install lives in a machine-global, cross-project cache
/// (`~/.local/share/uv/python/...`), often hardlinked and shared between
/// concurrent uv processes. Patching it in place corrupts state that doesn't
/// belong to this project. This asserts the invariant regardless of whether a
/// bundled interpreter ends up being *selected* — it just must not be modified.
#[test]
fn bundled_python_install_is_never_mutated() {
    let _nixpkgs = require_nixpkgs_python!();
    let project = Project::new();

    // A dedicated install: the other tests point `UV_PYTHON_INSTALL_DIR` at a
    // shared fixture and run in parallel, which would race with this snapshot.
    let bundled = install_bundled_python();
    let bundled_dir = bundled.path().join("python");

    // Snapshot only the interpreter that already exists. uv is free to add a
    // *new* install under `only-managed`; what it must never do is rewrite one
    // that's already there and shared with other projects.
    let interpreter_dir = std::fs::read_dir(&bundled_dir)
        .expect("failed to read bundled python dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("cpython-3.12."))
                && p.is_dir()
                && !p.is_symlink()
        })
        .expect("no concrete cpython-3.12.* install found");

    let before = tree_snapshot(&interpreter_dir);
    assert!(
        !before.is_empty(),
        "bundled python install is empty — nothing to check"
    );

    // `only-managed` is the most forceful way to push uv at the bundled
    // interpreter. Whether it succeeds is not the point; leaving the install
    // untouched is.
    let _ = project
        .uv_with_python_dir(&bundled_dir)
        .args(["venv", "--python-preference", "only-managed", ".venv"])
        .output()
        .expect("uv venv failed to execute");

    let after = tree_snapshot(&interpreter_dir);

    let describe = |entries: &[Snapshot]| -> Vec<String> {
        entries.iter().map(|e| e.0.display().to_string()).collect()
    };
    let before_paths = describe(&before);
    let after_paths = describe(&after);

    // Cap the lists: a Python install has thousands of files, and an
    // unbounded diff buries the signal in CI logs.
    let sample = |paths: Vec<&String>| -> String {
        let total = paths.len();
        let shown: Vec<_> = paths.into_iter().take(10).cloned().collect();
        format!("{total} [{}]", shown.join(", "))
    };

    let added = sample(
        after_paths
            .iter()
            .filter(|p| !before_paths.contains(p))
            .collect(),
    );
    let removed = sample(
        before_paths
            .iter()
            .filter(|p| !after_paths.contains(p))
            .collect(),
    );
    let modified: Vec<_> = before
        .iter()
        .filter_map(|b| {
            after
                .iter()
                .find(|a| a.0 == b.0)
                .filter(|a| a != &b)
                .map(|a| format!("{} ({:?} -> {:?})", b.0.display(), (b.1, b.2), (a.1, a.2)))
        })
        .take(10)
        .collect();

    assert!(
        added.starts_with("0 ") && removed.starts_with("0 ") && modified.is_empty(),
        "uv rewrote the existing bundled Python install at {}.\n\
         added: {added}\nremoved: {removed}\nmodified:\n{}",
        interpreter_dir.display(),
        modified.join("\n")
    );
}

/// `uv run` must execute against the nixpkgs interpreter too, not just `uv venv`.
#[test]
fn uv_run_executes_against_nixpkgs_python() {
    let _nixpkgs = require_nixpkgs_python!();
    let project = Project::new();

    let stdout = run(
        "uv run",
        project.uv_with_bundled().args([
            "run",
            "python",
            "-c",
            "import os,sys; print(os.path.realpath(sys.base_prefix))",
        ]),
    );

    let base_prefix = PathBuf::from(stdout.trim());
    assert!(
        base_prefix.starts_with("/nix/store"),
        "uv run used a non-nixpkgs interpreter rooted at {}",
        base_prefix.display()
    );
    assert!(
        !base_prefix.starts_with(bundled_python_dir()),
        "uv run used uv's bundled Python at {}",
        base_prefix.display()
    );
}
