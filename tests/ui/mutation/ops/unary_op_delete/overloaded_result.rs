//@ build
//@ stderr: empty
//@ mutation-operators: unary_op_delete

use std::ops::{Neg, Not};

struct Flag(bool);

impl Not for Flag {
    type Output = bool;

    #[mutest::skip]
    fn not(self) -> bool { !self.0 }
}

struct Number(i32);

impl Neg for Number {
    type Output = i32;

    #[mutest::skip]
    fn neg(self) -> i32 { -self.0 }
}

fn flag(value: Flag) -> bool { !value }
fn number(value: Number) -> i32 { -value }
fn ordinary(value: i32) -> i32 { -value }

#[test]
fn result_types_are_preserved() {
    assert!(!flag(Flag(true)));
    assert_eq!(number(Number(7)), -7);
    assert_eq!(ordinary(3), -3);
}
