//! A driver built with `embed-runtime` carries the runtime, and unpacks it into the target
//! directory for the harnesses it builds to link.

#![cfg(feature = "embed-runtime")]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{self, Command};

/// Runs the driver on a crate with one test, as `cargo mutest` would with `--emit={emit}`, and
/// returns the directory it was given as its target directory.
fn run_driver(name: &str, emit: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("mutest-driver-unpack-{}-{name}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("lib.rs");
    fs::write(&source, "pub fn answer() -> u32 { 42 }\n\n#[test]\nfn test() { assert_eq!(42, answer()); }\n").unwrap();
    let target_dir = dir.join("target");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mutest-driver"));
    cmd.arg(&source).args(["--crate-name", "unpack", "--edition=2021", "--crate-type", "lib", "--test", "--out-dir"]).arg(&target_dir);
    // As the UI tests run it: Cargo's variables would have it take the crate for a Cargo package.
    for (var, _) in env::vars() {
        if var.starts_with("CARGO_") { cmd.env_remove(var); }
    }
    cmd.env("MUTEST_ENCODED_ARGS", format!("--emit={emit}"));
    cmd.env("MUTEST_TARGET_DIR_ROOT", &target_dir);

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "the driver failed:\n{}", String::from_utf8_lossy(&output.stderr));
    target_dir
}

#[test]
fn a_run_that_builds_no_harness_unpacks_no_runtime() {
    let target_dir = run_driver("info", "info");

    let unpacked = target_dir.join("mutest_deps").exists();
    fs::remove_dir_all(target_dir.parent().unwrap()).unwrap();
    assert!(!unpacked, "the runtime was unpacked for a run that links nothing");
}

#[test]
fn a_run_that_builds_a_harness_unpacks_the_runtime_it_links() {
    let target_dir = run_driver("test-bin", "info,test-bin");

    let unpacked = target_dir.join("mutest_deps").join("libmutest_runtime.rlib").exists();
    fs::remove_dir_all(target_dir.parent().unwrap()).unwrap();
    assert!(unpacked, "the runtime the harness links was not unpacked");
}
