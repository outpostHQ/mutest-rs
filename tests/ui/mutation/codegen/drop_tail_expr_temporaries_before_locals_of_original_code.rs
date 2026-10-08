//@ build
//@ stderr: empty
//@ edition: 2024
//@ mutation-operators: math_op_add_sub_swap

//! In edition 2024, the temporaries of the tail expression of a body are dropped before its local variables.
//! The original code and the code with substitutions of a body must keep that order, or the borrow outlives `cell`.

use std::cell::RefCell;

fn tail_expr_borrows_local(value: i32) -> i32 {
    let cell = RefCell::new(value + 1);
    *cell.borrow()
}

#[test]
fn test() {
    assert_eq!(3, tail_expr_borrows_local(2));
}
