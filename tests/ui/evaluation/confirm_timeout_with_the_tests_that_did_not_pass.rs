//@ print-mutations
//@ run: exit 3
//@ stdout
//@ stderr: empty
//@ mutation-operators: call_delete
//@ run-flags: -v --exhaustive

//! A timed-out mutation runs again alone to confirm its timeout, but only with the tests that did not pass it:
//! here `return_at_once` passes the mutation, so only `wait_for_the_flag` runs again.

use std::sync::atomic::{AtomicBool, Ordering};

fn settle(flag: &AtomicBool, wait: bool) {
    while wait {
        // NOTE: Without this call, the loop spins until it times out, re-entering the substitution each time.
        if flag.swap(true, Ordering::SeqCst) { break; }
    }
}

#[test]
fn return_at_once() {
    settle(&AtomicBool::new(false), false);
}

#[test]
fn wait_for_the_flag() {
    settle(&AtomicBool::new(false), true);
}
