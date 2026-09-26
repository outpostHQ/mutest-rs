//@ edition: 2021
//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

//! A test may run its own binary again to run one test in a child process, naming it as libtest
//! takes it: `<name> --exact`. The harness has to run that one test, as libtest would. Running a
//! mutation analysis instead runs the test that starts the child again, and so another child.

use std::env;
use std::process::Command;

const CHILD_VAR: &str = "MUTEST_TEST_RUN_AS_CHILD";

fn answer() -> u32 {
    42
}

#[test]
fn child() {
    if env::var(CHILD_VAR).is_err() { return; }
    println!("the child ran");
}

#[test]
fn runs_one_test_in_a_child() {
    if env::var(CHILD_VAR).is_ok() { return; }

    let output = Command::new(env::current_exe().unwrap())
        .args(["child", "--exact", "--nocapture"])
        .env(CHILD_VAR, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "the child failed:\n{stdout}");
    assert!(stdout.contains("the child ran"), "the test did not run in the child:\n{stdout}");
    assert!(stdout.contains("running 1 test\n"), "the child ran more than the test it was given:\n{stdout}");
    assert!(!stdout.contains("profiling reference test run"), "the child ran a mutation analysis:\n{stdout}");

    assert_eq!(42, answer());
}
