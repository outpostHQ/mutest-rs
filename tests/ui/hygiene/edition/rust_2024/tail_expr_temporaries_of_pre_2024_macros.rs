//@ build
//@ stderr: empty
//@ edition: 2024
//@ aux-build: tail_expr_temporaries_macros.rs

extern crate tail_expr_temporaries_macros;

use tail_expr_temporaries_macros::{closure_name_len, const_arg_len, labeled_name_len, name_len, unsafe_name_len};

fn name_lens(name: &str) -> usize {
    name_len!(name.to_owned())
        + unsafe_name_len!(name.to_owned())
        + labeled_name_len!(name.to_owned())
        + closure_name_len!(name.to_owned())
        + const_arg_len!()
}

#[test]
fn test() {
    assert_eq!(name_lens("abc"), 15);
}
