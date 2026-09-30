//@ edition: 2021
//@ run
//@ stdout
//@ stderr: empty

//! A different executable must ignore the inherited libtest marker.

use std::env;
use std::fs;
use std::process::{self, Command};

#[test]
fn another_harness_analyses_its_package() {
    let cargo_mutest = env::var_os("MUTEST_TESTS_CARGO_MUTEST").expect("the UI test runner names `cargo-mutest`");
    let dir = env::temp_dir().join(format!("mutest-another-harness-{}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("Cargo.toml"), "[package]\nname = \"another\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n").unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn answer() -> u32 {\n    42\n}\n\n#[test]\nfn calls_answer() {\n    let _ = answer();\n}\n").unwrap();

    let build = Command::new(cargo_mutest).args(["mutest", "run", "--no-run"]).current_dir(&dir).output().unwrap();
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    // Cargo names what it built: `Executable unittests src/lib.rs (<path>)`, with `\` on Windows.
    let harness = build_stderr.lines().rev().find_map(|line| {
        let path = line.trim_start().strip_prefix("Executable unittests src")?.strip_prefix(['/', '\\'])?.strip_prefix("lib.rs (")?.strip_suffix(')')?;
        Some(dir.join(path))
    });
    let Some(harness) = harness else {
        let _ = fs::remove_dir_all(&dir);
        panic!("the other harness was not built:\n{build_stderr}");
    };

    let output = Command::new(harness).current_dir(&dir).output().unwrap();
    let _ = fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0 detected (0 timed out; 0 crashed); 2 undetected; 2 total"), "the other harness did not run the analysis:\n{stdout}");
    assert_eq!(output.status.code(), Some(2), "{stdout}");
}
