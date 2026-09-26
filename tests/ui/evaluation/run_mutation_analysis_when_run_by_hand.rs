//@ run: exit 2
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

//! A harness built with `cargo mutest run --no-run` and run later, by hand or by a script, runs the
//! mutation analysis, as it does when `cargo mutest` runs it: running as libtest instead would
//! pass every test and exit 0 without a mutation applied.

fn answer() -> u32 {
    42
}

#[test]
fn calls_answer() {
    let _ = answer();
}
