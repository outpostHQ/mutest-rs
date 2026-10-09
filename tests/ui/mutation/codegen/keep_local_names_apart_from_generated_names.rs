//@ build
//@ stderr: empty
//@ mutation-operators: math_op_add_sub_swap

//! The generated code is printed, so a name it binds must not take the place of a name of the function.

fn plain(subst: i32) -> i32 {
    subst + 1
}

// NOTE: The body of this function reads each substitution from a slot of its own.
fn inner_item(subst: i32) -> i32 {
    fn inner() -> i32 { 1 }
    subst + inner()
}

#[test]
fn test() {
    assert_eq!(3, plain(2));
    assert_eq!(3, inner_item(2));
}
