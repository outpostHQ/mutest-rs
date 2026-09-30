//@ build
//@ stderr: empty
//@ mutation-operators: unary_op_delete

fn smallest() -> i8 { -128i8 }
fn inferred_smallest() -> i8 { -128 }
fn ordinary(value: i8) -> i8 { -value }

#[test]
fn minimum_literals_keep_their_sign() {
    assert_eq!(smallest(), i8::MIN);
    assert_eq!(inferred_smallest(), i8::MIN);
    assert_eq!(ordinary(7), -7);
}
