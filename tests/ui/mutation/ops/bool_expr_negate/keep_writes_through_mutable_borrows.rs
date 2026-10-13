//@ print-mutations
//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: bool_expr_negate
//@ mutations: none

use std::mem;

#[allow(dead_code, reason = "rustc does not count a method call on `bool` as a use of the trait")]
trait Toggle {
    fn toggle(&mut self);
}

impl Toggle for bool {
    #[mutest::skip]
    fn toggle(&mut self) { *self = !*self; }
}

struct Marks {
    marked: bool,
    pair: [bool; 2],
}

#[allow(unused_must_use, reason = "only the writes through the borrowed places are observed")]
fn reset(marks: &mut Marks, mut local: bool) {
    mem::take(&mut local);
    mem::swap(&mut local, &mut marks.marked);
    mem::take(&mut marks.pair[0]);
    marks.pair[1].toggle();
}

#[allow(unused_must_use, reason = "only the write through the borrowed place is observed")]
fn clear(flag: &mut bool) {
    mem::take(&mut *flag);
}

#[test]
fn writes_reach_the_borrowed_places() {
    let mut marks = Marks { marked: true, pair: [true, false] };
    reset(&mut marks, true);
    assert!(!marks.marked);
    assert_eq!(marks.pair, [false, true]);

    let mut flag = true;
    clear(&mut flag);
    assert!(!flag);
}
