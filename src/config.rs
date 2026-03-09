use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// Per-package build configuration from `[[tool.uv-nix.package]]`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PackageConfig {
    /// Package name (required).
    pub name: String,

    /// Custom nixpkgs for this package (overrides project default).
    pub nixpkgs: Option<String>,

    /// Libraries to use (replaces defaults from package-build-libs.json).
    /// If empty, defaults are used.
    #[serde(default)]
    pub libraries: Vec<String>,

    /// Extra libraries to add to defaults.
    #[serde(default)]
    pub extra_libraries: Vec<String>,

    /// Extra build tools (e.g., cargo, cmake).
    #[serde(default)]
    pub extra_build_tools: Vec<String>,

    /// Linux-only extra libraries.
    #[serde(default)]
    pub extra_linux_libraries: Vec<String>,

    /// Darwin-only extra libraries.
    #[serde(default)]
    pub extra_darwin_libraries: Vec<String>,
}

impl PackageConfig {
    /// Check if this config has any meaningful overrides.
    pub fn has_overrides(&self) -> bool {
        self.nixpkgs.is_some()
            || !self.libraries.is_empty()
            || !self.extra_libraries.is_empty()
            || !self.extra_build_tools.is_empty()
            || !self.extra_linux_libraries.is_empty()
            || !self.extra_darwin_libraries.is_empty()
    }
}

/// The `[tool.uv-nix]` section from pyproject.toml.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UvNixConfig {
    /// Extra nixpkgs attr paths to include in RPATH (additive to defaults).
    #[serde(default)]
    pub extra_libraries: Vec<String>,

    /// Optional explicit nixpkgs pin (overrides auto-detection).
    pub nixpkgs: Option<String>,

    /// Per-package build configurations.
    #[serde(default, rename = "package")]
    pub packages: Vec<PackageConfig>,
}

impl UvNixConfig {
    /// Get custom config for a specific package, if any.
    pub fn get_package_config(&self, name: &str) -> Option<&PackageConfig> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Check if this config has any meaningful settings.
    pub fn has_config(&self) -> bool {
        !self.extra_libraries.is_empty()
            || self.nixpkgs.is_some()
            || !self.packages.is_empty()
    }
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
    if !config.has_config() {
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

    #[test]
    fn test_parse_package_config() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("pyproject.toml");
        fs::write(
            &toml_path,
            r#"
[tool.uv-nix]

[[tool.uv-nix.package]]
name = "psycopg2"
nixpkgs = "github:NixOS/nixpkgs/my-custom-pin"
libraries = ["postgresql_17"]
extra-build-tools = ["gcc"]

[[tool.uv-nix.package]]
name = "numpy"
extra-libraries = ["mkl"]
extra-linux-libraries = ["cuda"]
"#,
        )
        .unwrap();

        let (config, _) = find_config(dir.path()).unwrap();
        assert_eq!(config.packages.len(), 2);

        let psycopg2 = config.get_package_config("psycopg2").unwrap();
        assert_eq!(psycopg2.nixpkgs.as_deref(), Some("github:NixOS/nixpkgs/my-custom-pin"));
        assert_eq!(psycopg2.libraries, vec!["postgresql_17"]);
        assert_eq!(psycopg2.extra_build_tools, vec!["gcc"]);
        assert!(psycopg2.has_overrides());

        let numpy = config.get_package_config("numpy").unwrap();
        assert!(numpy.nixpkgs.is_none());
        assert!(numpy.libraries.is_empty());
        assert_eq!(numpy.extra_libraries, vec!["mkl"]);
        assert_eq!(numpy.extra_linux_libraries, vec!["cuda"]);
        assert!(numpy.has_overrides());

        assert!(config.get_package_config("nonexistent").is_none());
    }

    #[test]
    fn test_package_config_only() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("pyproject.toml");
        fs::write(
            &toml_path,
            r#"
[tool.uv-nix]

[[tool.uv-nix.package]]
name = "pillow"
extra-libraries = ["libheif"]
"#,
        )
        .unwrap();

        let (config, _) = find_config(dir.path()).unwrap();
        assert!(config.extra_libraries.is_empty());
        assert_eq!(config.packages.len(), 1);
        assert!(config.has_config());
    }
}
