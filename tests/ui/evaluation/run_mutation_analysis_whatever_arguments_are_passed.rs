//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default
//@ run-flags: --isolate=unsafe foo

//! A positional argument must not switch the harness into libtest mode.

fn answer() -> u32 {
    42
}

#[test]
fn test() {
    assert_eq!(42, answer());
}
