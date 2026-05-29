//! Offline tool to generate `data/soname-map.json`.
//!
//! Resolves all known lib attrs from default-libs.json + package-build-libs.json
//! via nix eval, then lists .so/.dylib files in each lib's store path.
//!
//! Usage: cargo run --bin generate_soname_map > data/soname-map.json

use std::collections::HashMap;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cwd = std::env::current_dir()?;
    let project_dir = uv_nix::nix_config::find_project_root(&cwd).unwrap_or(cwd);

    let uv_nix_config = uv_nix::config::find_config(&project_dir)
        .map(|(c, _)| c)
        .unwrap_or_default();

    let source = uv_nix::nixpkgs::resolve_nixpkgs(&project_dir, &uv_nix_config);
    let is_darwin = cfg!(target_os = "macos");

    eprintln!(
        "Generating soname map for {} using {:?}",
        if is_darwin { "darwin" } else { "linux" },
        source
    );

    let platform_map = uv_nix::soname::generate_soname_map_for_platform(&source, is_darwin)?;

    eprintln!("Resolved {} soname entries", platform_map.len());

    // Build the full SonameMap structure
    let (linux, darwin) = if is_darwin {
        // Load existing linux entries, replace darwin
        let existing = uv_nix::soname::SonameMap::load_embedded().unwrap_or_else(|_| {
            uv_nix::soname::SonameMap {
                linux: HashMap::new(),
                darwin: HashMap::new(),
            }
        });
        (existing.linux, platform_map)
    } else {
        // Load existing darwin entries, replace linux
        let existing = uv_nix::soname::SonameMap::load_embedded().unwrap_or_else(|_| {
            uv_nix::soname::SonameMap {
                linux: HashMap::new(),
                darwin: HashMap::new(),
            }
        });
        (platform_map, existing.darwin)
    };

    let full_map = uv_nix::soname::SonameMap { linux, darwin };
    let json = serde_json::to_string_pretty(&full_map)?;
    println!("{json}");

    Ok(())
}
