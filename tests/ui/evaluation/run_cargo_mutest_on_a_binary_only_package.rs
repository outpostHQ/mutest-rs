//@ edition: 2021
//@ run
//@ stdout
//@ stderr: empty

//! A package with only a binary has no library for `--lib` to select.

use std::env;
use std::fs;
use std::process::{self, Command};

#[test]
fn cargo_mutest_analyses_a_binary_only_package() {
    let cargo_mutest = env::var_os("MUTEST_TESTS_CARGO_MUTEST").expect("the UI test runner names `cargo-mutest`");
    let dir = env::temp_dir().join(format!("mutest-binary-only-{}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("Cargo.toml"), "[package]\nname = \"binary\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n").unwrap();
    fs::write(dir.join("src/main.rs"), "fn answer() -> u32 {\n    42\n}\n\nfn main() {\n    println!(\"{}\", answer());\n}\n\n#[test]\nfn calls_answer() {\n    let _ = answer();\n}\n").unwrap();

    let output = Command::new(cargo_mutest).args(["mutest", "run"]).current_dir(&dir).output().unwrap();
    let _ = fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("0 detected (0 timed out; 0 crashed); 2 undetected; 2 total"), "the package was not analysed:\n{stdout}\n{stderr}");
    assert_eq!(output.status.code(), Some(2), "{stdout}\n{stderr}");
}
