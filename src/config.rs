use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::debug;

/// The `[tool.uv-nix]` section from pyproject.toml.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UvNixConfig {
    /// Extra nixpkgs attr paths to include in RPATH (additive to defaults).
    #[serde(default)]
    pub extra_libraries: Vec<String>,

    /// Optional explicit nixpkgs pin (overrides auto-detection).
    pub nixpkgs: Option<String>,
}

/// Partial pyproject.toml structure — just enough to extract `[tool.uv-nix]`.
#[derive(Debug, Deserialize)]
struct PyprojectToml {
    #[serde(default)]
    tool: Option<ToolTable>,
}

#[derive(Debug, Deserialize)]
struct ToolTable {
    #[serde(rename = "uv-nix")]
    uv_nix: Option<UvNixConfig>,
}

/// Search upward from `start` for a `pyproject.toml` containing `[tool.uv-nix]`.
///
/// Returns the parsed config and the directory containing the pyproject.toml.
pub fn find_config(start: &Path) -> Option<(UvNixConfig, PathBuf)> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };

    loop {
        let candidate = dir.join("pyproject.toml");
        if candidate.is_file() {
            if let Some(config) = try_parse_config(&candidate) {
                debug!(
                    "Found [tool.uv-nix] in {}",
                    candidate.display()
                );
                return Some((config, dir));
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Try to parse `[tool.uv-nix]` from a pyproject.toml file.
fn try_parse_config(path: &Path) -> Option<UvNixConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    let pyproject: PyprojectToml = toml::from_str(&content).ok()?;
    let config = pyproject.tool?.uv_nix?;

    // Only return if there's actually something configured
    if config.extra_libraries.is_empty() && config.nixpkgs.is_none() {
        return None;
    }

    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_extra_libraries() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("pyproject.toml");
        fs::write(
            &toml_path,
            r#"
[project]
name = "test"

[tool.uv-nix]
extra-libraries = ["libGL", "cudaPackages.cudatoolkit"]
"#,
        )
        .unwrap();

        let (config, project_dir) = find_config(dir.path()).unwrap();
        assert_eq!(config.extra_libraries, vec!["libGL", "cudaPackages.cudatoolkit"]);
        assert!(config.nixpkgs.is_none());
        assert_eq!(project_dir, dir.path());
    }

    #[test]
    fn test_parse_with_nixpkgs_pin() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("pyproject.toml");
        fs::write(
            &toml_path,
            r#"
[tool.uv-nix]
extra-libraries = ["libGL"]
nixpkgs = "github:NixOS/nixpkgs/nixos-24.11"
"#,
        )
        .unwrap();

        let (config, _) = find_config(dir.path()).unwrap();
        assert_eq!(config.extra_libraries, vec!["libGL"]);
        assert_eq!(
            config.nixpkgs.as_deref(),
            Some("github:NixOS/nixpkgs/nixos-24.11")
        );
    }

    #[test]
    fn test_no_uv_nix_section() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("pyproject.toml");
        fs::write(
            &toml_path,
            r#"
[project]
name = "test"
"#,
        )
        .unwrap();

        assert!(find_config(dir.path()).is_none());
    }

    #[test]
    fn test_empty_uv_nix_section() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("pyproject.toml");
        fs::write(
            &toml_path,
            r#"
[tool.uv-nix]
"#,
        )
        .unwrap();

        assert!(find_config(dir.path()).is_none());
    }
}
