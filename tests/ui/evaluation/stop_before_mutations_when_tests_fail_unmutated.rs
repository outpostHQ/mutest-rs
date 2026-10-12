//@ run: exit 4
//@ stdout
//@ mutation-operators: fn_return_default

//! If a test fails without any mutation applied, no mutation is evaluated, and the output of the test is shown.

fn answer() -> u32 {
    41
}

#[test]
fn test() -> Result<(), String> {
    if answer() == 42 { Ok(()) } else { Err(format!("expected 42, got {}", answer())) }
}
