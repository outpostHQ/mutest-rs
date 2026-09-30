//@ run: exit 2
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

//! Standalone harnesses must analyse mutations rather than silently run plain tests.

fn answer() -> u32 {
    42
}

#[test]
fn calls_answer() {
    let _ = answer();
}
