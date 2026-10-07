//@ print-mutations
//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: bool_expr_negate
//@ mutations: none

use std::cell::Cell;
use std::collections::BTreeSet;
use std::ops::Not;

fn insert(seen: &mut BTreeSet<u64>, id: u64) {
    seen.insert(id);
    (seen.insert(id));
    { seen.insert(id); }
}

#[mutest::skip]
fn exited(outcome: Result<bool, u8>, owned: &mut bool, calls: &mut usize) -> Result<bool, u8> {
    *calls += 1;
    if outcome.is_err() { *owned = false; }
    outcome
}

fn terminate(outcome: Result<bool, u8>, owned: &mut bool, calls: &mut usize, trace: &mut Vec<u8>) -> Result<(), String> {
    exited(outcome, owned, calls).map_err(|error| error.to_string())?;
    trace.push(1);
    Ok(())
}

struct Flag<'a>(&'a Cell<u32>);

impl Not for Flag<'_> {
    type Output = bool;

    #[mutest::skip]
    fn not(self) -> bool {
        self.0.set(self.0.get() + 1);
        true
    }
}

#[allow(unused_must_use, reason = "the complete overloaded-Not result is intentionally discarded")]
fn discarded_not(calls: &Cell<u32>) {
    !Flag(calls);
    Flag(calls);
}

#[test]
fn statement_effects_and_errors_are_preserved() {
    let mut seen = BTreeSet::new();
    insert(&mut seen, 7);
    assert_eq!(seen, BTreeSet::from([7]));
    insert(&mut seen, 9);
    assert_eq!(seen, BTreeSet::from([7, 9]));

    for value in [false, true] {
        let (mut owned, mut calls, mut trace) = (true, 0, Vec::new());
        assert_eq!(terminate(Ok(value), &mut owned, &mut calls, &mut trace), Ok(()));
        assert!(owned);
        assert_eq!(calls, 1);
        assert_eq!(trace, [1]);
    }
    let (mut owned, mut calls, mut trace) = (true, 0, Vec::new());
    assert_eq!(terminate(Err(17), &mut owned, &mut calls, &mut trace), Err("17".to_owned()));
    assert!(!owned);
    assert_eq!(calls, 1);
    assert_eq!(trace, Vec::<u8>::new());

    let calls = Cell::new(0);
    discarded_not(&calls);
    assert_eq!(calls.get(), 1);
}
