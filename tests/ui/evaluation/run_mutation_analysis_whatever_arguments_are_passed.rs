//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default
//@ run-flags: --isolate=unsafe foo

//! `cargo mutest run -- foo` passes `foo` to the harness after the options of its own. The harness
//! runs the mutation analysis because `cargo mutest` started it, whatever its arguments look like:
//! taken for a test name, `foo` would hand them all to libtest, which knows none of the options.

fn answer() -> u32 {
    42
}

#[test]
fn test() {
    assert_eq!(42, answer());
}
