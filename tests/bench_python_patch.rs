//! Benchmark for Python interpreter patching performance.
//!
//! Measures the time to patch an already-installed Python interpreter,
//! breaking down into: binary discovery, patching, and ctypes hook install.
//!
//! Run with: `just test -- bench_python_patch --nocapture`

mod common;

use common::runner::UV_BIN;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use tempfile::tempdir;

/// Install Python into a temp HOME and return the path to the installation directory.
fn install_python(home: &std::path::Path) -> PathBuf {
    let output = Command::new(UV_BIN.as_path())
        .args(["python", "install", "3.12"])
        .env("HOME", home)
        .output()
        .expect("python install failed to execute");

    assert!(
        output.status.success(),
        "python install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let python_dir = home.join(".local/share/uv/python");
    std::fs::read_dir(&python_dir)
        .expect("Failed to read python dir")
        .filter_map(|e| e.ok())
        .find(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("cpython-3.12") && !name.ends_with(".nix")
        })
        .map(|e| e.path())
        .expect("Python installation not found")
}

/// Remove the stamp file so patching will re-run.
fn remove_stamp(python_dir: &std::path::Path) {
    let stamp = python_dir.join(".uv-nix-patched");
    let _ = std::fs::remove_file(&stamp);
}

/// Count native binaries in a directory (for reporting).
fn count_native_binaries(dir: &std::path::Path) -> usize {
    let is_darwin = cfg!(target_os = "macos");
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            let is_so = name.contains(".so");
            let is_dylib = name.contains(".dylib");
            let is_extensionless = !name.contains('.');
            if is_darwin {
                is_so || is_dylib || is_extensionless
            } else {
                is_so || is_extensionless
            }
        })
        .filter(|e| uv_nix::patchelf::is_native_binary(e.path(), cfg!(target_os = "macos")))
        .count()
}

#[test]
fn bench_python_patch_timing() {
    let home = tempdir().unwrap();

    eprintln!("Installing Python 3.12...");
    let t0 = Instant::now();
    let python_dir = install_python(home.path());
    let install_ms = t0.elapsed().as_millis();
    eprintln!("  Install: {}ms", install_ms);

    let num_binaries = count_native_binaries(&python_dir);
    eprintln!("  Native binaries found: {}", num_binaries);

    // The first install already patched. Remove stamp and re-patch to benchmark.
    remove_stamp(&python_dir);

    // Benchmark: re-patch the interpreter (binaries already have /nix/store refs,
    // so the "already patched" short-circuit kicks in — measure that overhead too)
    eprintln!("\nBenchmarking patch (already-patched short-circuit)...");
    let t1 = Instant::now();
    uv_nix::post_python_install_patch(&python_dir);
    let repatch_ms = t1.elapsed().as_millis();
    eprintln!("  Re-patch (short-circuit): {}ms", repatch_ms);

    // Benchmark: stamp file check (should be instant)
    eprintln!("\nBenchmarking stamp file check...");
    let t2 = Instant::now();
    uv_nix::post_python_install_patch(&python_dir);
    let stamp_ms = t2.elapsed().as_millis();
    eprintln!("  Stamp check (no-op): {}ms", stamp_ms);

    // Now benchmark a fresh patch by removing both stamp and nix store references.
    // We can't easily "un-patch" binaries, so we measure the short-circuit path
    // which still does binary discovery + rpath reading. For a true fresh-patch
    // benchmark, we'd need to reinstall Python.
    remove_stamp(&python_dir);

    eprintln!("\nBenchmarking full patch (with binary discovery)...");
    let t3 = Instant::now();
    uv_nix::post_python_install_patch(&python_dir);
    let full_ms = t3.elapsed().as_millis();
    eprintln!("  Full patch pass: {}ms", full_ms);

    // Summary
    eprintln!("\n--- Python Patch Benchmark Summary ---");
    eprintln!("  Binaries:             {}", num_binaries);
    eprintln!("  Install + patch:      {}ms", install_ms);
    eprintln!("  Re-patch (has refs):  {}ms", repatch_ms);
    eprintln!("  Stamp no-op:          {}ms", stamp_ms);
    eprintln!("  Full pass (parallel): {}ms", full_ms);
    eprintln!("--------------------------------------");

    // Assertions: stamp check should be near-instant
    assert!(stamp_ms < 10, "Stamp check took too long: {}ms", stamp_ms);
}

#[test]
fn bench_fresh_python_patch() {
    let home = tempdir().unwrap();

    eprintln!("Installing Python 3.12 (timing includes download + patch)...");
    let t0 = Instant::now();

    // Use UV_NIX_TIMING to get internal breakdown
    let output = Command::new(UV_BIN.as_path())
        .args(["python", "install", "3.12"])
        .env("HOME", home.path())
        .env("UV_NIX_TIMING", "1")
        .output()
        .expect("python install failed");

    let total_ms = t0.elapsed().as_millis();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "python install failed: {}",
        stderr
    );

    eprintln!("  Total: {}ms", total_ms);
    eprintln!("  stderr output:");
    for line in stderr.lines() {
        if line.contains("Patching") || line.contains("Patched") || line.contains("uv-nix") {
            eprintln!("    {}", line);
        }
    }
}
