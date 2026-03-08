use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone)]
pub struct PackageInfo {
    /// Python code to run to verify the package works
    pub import_check: String,

    /// Skip this package on Darwin (macOS)
    #[serde(default)]
    pub skip_darwin: bool,

    /// Skip wheel install test (source-only package)
    #[serde(default)]
    pub skip_wheel: bool,

    /// Mark as slow test (requires --features slow-tests)
    #[serde(default)]
    pub slow: bool,
}

/// Load package definitions from embedded JSON
pub fn load_packages() -> HashMap<String, PackageInfo> {
    let json = include_str!("../data/packages.json");
    serde_json::from_str(json).expect("Failed to parse packages.json")
}

/// Get import check for a package, with sensible defaults
pub fn import_check_for(package: &str) -> String {
    let packages = load_packages();
    if let Some(info) = packages.get(package) {
        info.import_check.clone()
    } else {
        // Default: try to import the package with underscores
        let module = package.replace('-', "_");
        format!("import {}; print('ok')", module)
    }
}
