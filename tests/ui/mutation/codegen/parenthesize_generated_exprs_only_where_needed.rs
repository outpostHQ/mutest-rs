//@ build
//@ stderr: empty
//@ mutation-operators: math_op_add_sub_swap, relational_op_invert, bit_op_shift_dir_swap, call_value_default_shadow, range_limit_swap

#![allow(unused)]

fn cast_ends_lhs_of_inverted_comparison(len: u64, pad: u32) -> bool {
    pad * len as u32 >= 8
}

fn cast_ends_lhs_of_swapped_shift(len: u64, pad: u32) -> u32 {
    pad * len as u32 >> 2
}

fn substitution_starts_statement(a: u32, b: u32, c: u32) {
    a + b & c;
}

fn range_end_starts_with_substitution(block: &[u8], from: usize, end: u64, held: u64) -> &[u8] {
    &block[from..end.min(held) as usize]
}

#[test]
fn test() {
    cast_ends_lhs_of_inverted_comparison(1, 2);
    cast_ends_lhs_of_swapped_shift(1, 2);
    substitution_starts_statement(1, 2, 3);
    range_end_starts_with_substitution(&[0; 8], 1, 4, 6);
}
