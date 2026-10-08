//@ print-mutations
//@ run: exit 2
//@ stdout
//@ stderr
//@ mutest-flags: --call-graph-depth-limit=2 --depth=2 --mutant-batch-size=100 --mutant-batch-algorithm=greedy --mutant-batch-greedy-ordering-heuristic=none
//@ run-flags: --exhaustive

fn add_one(a: u32) -> u32 {
    a + 1
}

fn add_two(a: u32) -> u32 {
    a + 2
}

fn add_one_if_small(a: u32) -> u32 {
    if a > 100 { return a; }
    add_one(a)
}

fn add_one_if_small_and_even(a: u32) -> u32 {
    if a % 2 != 0 { return a; }
    add_one_if_small(a)
}

#[test]
fn test_add_one() {
    assert_eq!(3, add_one(2));
}

#[test]
fn test_add_two() {
    assert_eq!(4, add_two(2));
}

// The call of `add_one` is past the depth limit, so mutations of `add_one` may change the result of this test.
#[test]
fn test_add_one_if_small_and_even() {
    assert_eq!(3, add_one_if_small_and_even(2));
}
