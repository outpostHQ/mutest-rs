//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

struct NoDefault(u32);

// Nothing can be returned without running the body, so the function is left alone.
fn no_default(x: u32) -> NoDefault {
    NoDefault(x)
}

#[test]
fn test() {
    assert_eq!(no_default(1).0, 1);
}
