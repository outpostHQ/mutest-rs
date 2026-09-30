//@ run: exit 4
//@ stdout
//@ mutation-operators: fn_return_default

//! If a test fails without any mutation applied, no mutation is evaluated.

fn answer() -> u32 {
    41
}

#[test]
fn test() {
    assert_eq!(42, answer());
}
