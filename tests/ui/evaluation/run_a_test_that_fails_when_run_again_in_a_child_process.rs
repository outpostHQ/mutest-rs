//@ print-mutations
//@ run: exit 2
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

//! A test that fails when it runs again in one process runs in a child process for each mutation,
//! so its second run does not falsely detect the mutation.

use std::sync::OnceLock;

static ONCE: OnceLock<()> = OnceLock::new();

fn record(flag: bool) -> bool {
    flag
}

#[test]
fn set_once() {
    ONCE.set(()).unwrap();
    let _ = record(true);
}
