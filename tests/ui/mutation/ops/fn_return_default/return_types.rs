//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

// Both values, so that a body always returning the non-default one is detected.
fn a_bool(x: u32) -> bool {
    x > 2
}

fn an_int(x: i32) -> i32 {
    x + 1
}

// No `-1`, which does not type-check for unsigned integers.
fn a_uint(x: u32) -> u32 {
    x + 1
}

fn a_str() -> &'static str {
    "hello"
}

#[test]
fn test() {
    assert!(a_bool(3));
    assert_eq!(an_int(1), 2);
    assert_eq!(a_uint(1), 2);
    assert_eq!(a_str(), "hello");
}
