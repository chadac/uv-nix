//! Benchmark tests for package install + patch performance.
//!
//! Measures time for each stage: venv creation, pip install (with patching),
//! and import verification. Outputs timing in a format suitable for CI reporting.
//!
//! When uv is built with `UV_NIX_TIMING=1`, the install step also reports
//! a breakdown of nix config resolution, binary discovery, and patching.

mod common;

use common::runner::UV_BIN;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

/// Parsed timing from `uv-nix-timing:` line in stderr.
#[derive(Default)]
struct PatchTiming {
    nix_resolve_ms: u128,
    find_binaries_ms: u128,
    num_binaries: usize,
    patch_ms: u128,
}

/// Parse all `uv-nix-timing:` lines from stderr and sum them up.
/// Multiple lines may appear (one per site-packages directory).
fn parse_patch_timing(stderr: &str) -> Option<PatchTiming> {
    let mut total = PatchTiming::default();
    let mut found = false;
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("uv-nix-timing:") {
            found = true;
            for part in rest.split_whitespace() {
                if let Some(val) = part.strip_prefix("nix_resolve=").and_then(|s| s.strip_suffix("ms")) {
                    total.nix_resolve_ms += val.parse::<u128>().unwrap_or(0);
                } else if let Some(val) = part.strip_prefix("find_binaries=").and_then(|s| s.strip_suffix("ms")) {
                    total.find_binaries_ms += val.parse::<u128>().unwrap_or(0);
                } else if let Some(val) = part.strip_prefix("patch=").and_then(|s| s.strip_suffix("ms")) {
                    total.patch_ms += val.parse::<u128>().unwrap_or(0);
                } else if let Some(val) = part.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
                    // "(N files)" — but we split by whitespace so this comes as "(N" and "files)"
                    if let Ok(n) = val.strip_suffix("files)").unwrap_or(val).parse::<usize>() {
                        total.num_binaries += n;
                    }
                }
                // Handle the "N" from "(N files)" split
                if part.starts_with('(') {
                    if let Ok(n) = part.trim_start_matches('(').parse::<usize>() {
                        total.num_binaries += n;
                    }
                }
            }
        }
    }
    if found { Some(total) } else { None }
}

/// Benchmark result for a single package install
struct BenchResult {
    package: String,
    venv_ms: u128,
    install_ms: u128,
    import_ms: u128,
    patch_timing: Option<PatchTiming>,
    success: bool,
    error: Option<String>,
}

impl BenchResult {
    fn total_ms(&self) -> u128 {
        self.venv_ms + self.install_ms + self.import_ms
    }
}

/// Run a benchmark for a single package in an isolated venv.
fn bench_package(package: &str, import_check: &str) -> BenchResult {
    let test_dir = std::env::temp_dir().join("uv-nix-bench");
    std::fs::create_dir_all(&test_dir).expect("create bench dir");

    let venv_path = test_dir.join(format!("bench-{}", package.replace(['[', ']'], "")));

    // Clean up any previous run
    let _ = std::fs::remove_dir_all(&venv_path);

    // Stage 1: Create venv
    let start = Instant::now();
    let venv_out = Command::new(UV_BIN.as_path())
        .args(["venv", venv_path.to_str().unwrap()])
        .output()
        .expect("venv creation failed");
    let venv_ms = start.elapsed().as_millis();

    if !venv_out.status.success() {
        return BenchResult {
            package: package.to_string(),
            venv_ms,
            install_ms: 0,
            import_ms: 0,
            patch_timing: None,
            success: false,
            error: Some(format!(
                "venv failed: {}",
                String::from_utf8_lossy(&venv_out.stderr)
            )),
        };
    }

    let python = venv_path.join("bin/python");

    // Stage 2: Install package (includes download + patch)
    // Set UV_NIX_TIMING=1 to get patch timing breakdown
    let start = Instant::now();
    let install_out = Command::new(UV_BIN.as_path())
        .args([
            "pip", "install", "-v",
            "--python", python.to_str().unwrap(),
            package,
        ])
        .env("UV_NIX_TIMING", "1")
        .output()
        .expect("install command failed");
    let install_ms = start.elapsed().as_millis();

    let install_stderr = String::from_utf8_lossy(&install_out.stderr).to_string();
    let patch_timing = parse_patch_timing(&install_stderr);

    if !install_out.status.success() {
        let _ = std::fs::remove_dir_all(&venv_path);
        return BenchResult {
            package: package.to_string(),
            venv_ms,
            install_ms,
            import_ms: 0,
            patch_timing,
            success: false,
            error: Some(format!("install failed: {install_stderr}")),
        };
    }

    // Stage 3: Import check
    let start = Instant::now();
    let check = Command::new(&python)
        .args(["-c", import_check])
        .output()
        .expect("import check failed");
    let import_ms = start.elapsed().as_millis();

    let success = check.status.success();
    let error = if success {
        None
    } else {
        Some(format!(
            "import failed: {}",
            String::from_utf8_lossy(&check.stderr)
        ))
    };

    // Cleanup
    let _ = std::fs::remove_dir_all(&venv_path);

    BenchResult {
        package: package.to_string(),
        venv_ms,
        install_ms,
        import_ms,
        patch_timing,
        success,
        error,
    }
}

/// Format benchmark results as a markdown table.
fn format_markdown(results: &[BenchResult]) -> String {
    let mut md = String::new();

    // Check if any result has patch timing
    let has_timing = results.iter().any(|r| r.patch_timing.is_some());

    if has_timing {
        md.push_str("| Package | Venv | Install | Nix Resolve | Find | Patch | Files | Import | Total | Status |\n");
        md.push_str("|---------|-----:|--------:|------------:|-----:|------:|------:|-------:|------:|--------|\n");
        for r in results {
            let status = if r.success { "pass" } else { "FAIL" };
            let (nix_resolve, find, patch, files) = match &r.patch_timing {
                Some(t) => (
                    format!("{}ms", t.nix_resolve_ms),
                    format!("{}ms", t.find_binaries_ms),
                    format!("{}ms", t.patch_ms),
                    format!("{}", t.num_binaries),
                ),
                None => ("-".into(), "-".into(), "-".into(), "-".into()),
            };
            md.push_str(&format!(
                "| {} | {}ms | {}ms | {} | {} | {} | {} | {}ms | {}ms | {} |\n",
                r.package,
                r.venv_ms,
                r.install_ms,
                nix_resolve,
                find,
                patch,
                files,
                r.import_ms,
                r.total_ms(),
                status,
            ));
        }
    } else {
        md.push_str("| Package | Venv | Install | Import | Total | Status |\n");
        md.push_str("|---------|-----:|--------:|-------:|------:|--------|\n");
        for r in results {
            let status = if r.success { "pass" } else { "FAIL" };
            md.push_str(&format!(
                "| {} | {}ms | {}ms | {}ms | {}ms | {} |\n",
                r.package,
                r.venv_ms,
                r.install_ms,
                r.import_ms,
                r.total_ms(),
                status,
            ));
        }
    }
    md
}

/// Benchmark packages are chosen to cover different cases:
/// - numpy: heavy native deps, .libs directory, many .so files
/// - aiohttp: multiple C extension packages in dependency tree
/// - orjson: single native extension, small
/// - pandas: depends on numpy, tests incremental install
/// - cryptography: OpenSSL binding
const BENCH_PACKAGES: &[(&str, &str)] = &[
    ("orjson", "import orjson; print(orjson.dumps({'a': 1}))"),
    ("cryptography", "from cryptography.hazmat.primitives.ciphers import Cipher; print('ok')"),
    ("numpy", "import numpy; print(numpy.__version__)"),
    ("aiohttp", "import aiohttp; print(aiohttp.__version__)"),
    ("pandas", "import pandas; print(pandas.__version__)"),
];

#[test]
fn bench_installs() {
    let results: Vec<BenchResult> = BENCH_PACKAGES
        .iter()
        .map(|(pkg, check)| {
            eprintln!("Benchmarking {pkg}...");
            bench_package(pkg, check)
        })
        .collect();

    let table = format_markdown(&results);
    eprintln!("\n{table}");

    // Write results to file for CI to pick up
    let out_path = std::env::var("BENCH_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let dir = std::env::temp_dir().join("uv-nix-bench");
            std::fs::create_dir_all(&dir).ok();
            dir.join("results.md")
        });
    std::fs::write(&out_path, &table).ok();
    eprintln!("Results written to {}", out_path.display());

    // Fail if any package failed
    for r in &results {
        assert!(
            r.success,
            "{} failed: {}",
            r.package,
            r.error.as_deref().unwrap_or("unknown")
        );
    }
}
