//! Resolve Python interpreters from nix.
//!
//! uv's own interpreter provisioning downloads a prebuilt CPython into a
//! machine-global cache (`~/.local/share/uv/python/...`) that is shared across
//! every project on the host. Making those builds work under nix means
//! patchelf-ing them in place, which mutates state this project does not own —
//! so uv-nix sources interpreters from nix instead and never touches them.
//!
//! Today the only provider is nixpkgs. The `Provider` split exists so
//! `nixpkgs-python` (which publishes every patch release, not just the handful
//! nixpkgs carries) can be added without disturbing callers.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::python_install::{self, PythonRequirement};

/// Where a nix-provided interpreter comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Provider {
    /// `pkgs.pythonXY` from the project's nixpkgs.
    #[default]
    Nixpkgs,
}

/// Resolve interpreters from nix that may satisfy `request`.
///
/// `request` is uv's canonical request string (e.g. `3.12`, `>=3.11`,
/// `cpython@3.12`); `None`/`default`/`any` means "no explicit version", in
/// which case the project's own `requires-python` is consulted and finally
/// plain `python3`.
///
/// Callers are expected to validate the returned interpreters against the
/// original request — this only narrows the search to nix-provided candidates.
pub fn candidates(request: Option<&str>, project_dir: &Path) -> Vec<PathBuf> {
    let requirement = request
        .filter(|r| !is_unversioned(r))
        .and_then(python_install::parse_request)
        .or_else(|| python_install::find_python_requirement(project_dir));

    match requirement {
        Some(requirement) => resolve_versioned(&requirement, project_dir),
        // No version constraint anywhere: the default interpreter will do.
        None => resolve_default(project_dir),
    }
}

/// `true` for request strings that carry no version information.
fn is_unversioned(request: &str) -> bool {
    let request = request.trim();
    request.is_empty()
        || request.eq_ignore_ascii_case("default")
        || request.eq_ignore_ascii_case("any")
}

fn resolve_versioned(requirement: &PythonRequirement, project_dir: &Path) -> Vec<PathBuf> {
    match python_install::find_nixpkgs_python(project_dir, requirement) {
        Ok(Some(python)) => vec![python],
        Ok(None) => {
            debug!(
                "nixpkgs has no Python matching {:?}",
                requirement.specifiers
            );
            Vec::new()
        }
        Err(err) => {
            debug!("failed to resolve a nixpkgs Python: {err}");
            Vec::new()
        }
    }
}

fn resolve_default(project_dir: &Path) -> Vec<PathBuf> {
    match python_install::resolve_default_python(project_dir) {
        Ok(python) => vec![python],
        Err(err) => {
            debug!("failed to resolve the default nixpkgs Python: {err}");
            Vec::new()
        }
    }
}
