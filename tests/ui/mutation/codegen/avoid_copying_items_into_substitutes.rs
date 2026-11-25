//@ build
//@ stderr: empty
//@ mutation-operators: bool_expr_negate

pub struct Flag;

fn mutable_fn(b: bool) -> bool {
    {
        // A copy of the `impl` in the negated block would define `Flag::ON` a second time.
        #[allow(non_local_definitions)]
        impl Flag { const ON: bool = true; }
        b && Flag::ON
    }
}

#[test]
fn test() {
    assert!(mutable_fn(true));
}
