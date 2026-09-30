//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: call_delete,call_value_default_shadow

fn identity(value: &str) -> &str {
    value
}

fn consume(value: &str) -> usize {
    value.len()
}

fn borrowed_argument() -> usize {
    consume(identity(&String::from("abc")))
}

fn stable_borrow(value: &str) -> usize {
    consume(identity(value))
}

fn owned_from_temporary() -> usize {
    consume(&String::from("abc"))
}

#[test]
fn temporary_lives_through_the_outer_call() {
    assert_eq!(borrowed_argument(), 3);
    assert_eq!(stable_borrow("abc"), 3);
    assert_eq!(owned_from_temporary(), 3);
}
