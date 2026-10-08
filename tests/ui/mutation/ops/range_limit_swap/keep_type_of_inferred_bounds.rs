//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: range_limit_swap

fn exclusive(pos: u8, len: u8) -> std::ops::Range<usize> {
    pos.into()..len.into()
}

fn inclusive(pos: u8, len: u8) -> std::ops::RangeInclusive<usize> {
    pos.into()..=len.into()
}

#[test]
fn test() {
    assert_eq!(exclusive(1, 3), 1..3);
    assert_eq!(inclusive(1, 3), 1..=3);
}
