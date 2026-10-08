//@ run
//@ stdout
//@ stderr
//@ mutation-operators: fn_return_default

//! A test that fails after another test has crashed gives the mutation a detection verdict.

use std::thread;
use std::time::Duration;

fn checked(value: u32) -> u32 {
    value
}

#[test]
fn ends_the_process() {
    if checked(7) != 7 {
        std::process::exit(9);
    }
}

#[test]
fn fails_later() {
    // NOTE: The wait puts this test after the one that ends the process, as the fastest test runs first.
    thread::sleep(Duration::from_millis(200));
    assert_eq!(7, checked(7));
}
