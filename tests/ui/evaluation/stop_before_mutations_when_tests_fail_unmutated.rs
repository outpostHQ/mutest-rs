//@ run: exit 4
//@ stdout
//@ mutation-operators: fn_return_default

//! A test that fails with no mutation applied cannot tell a mutation from the bug already there, so
//! no mutation is evaluated, and the harness exits with the code that says the baseline failed.

fn answer() -> u32 {
    41
}

#[test]
fn test() {
    assert_eq!(42, answer());
}
