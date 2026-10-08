//@ print-mutations
//@ run: exit 3
//@ stdout
//@ stderr: empty
//@ mutation-operators: bool_expr_negate
//@ run-flags: -v --exhaustive

//! A timed-out test thread is cancelled by a panic at the next substitution point it reaches.
//! A destructor that reaches another one while the thread unwinds runs the original code, as a second panic aborts.

struct ReachesSubstWhileUnwinding;

impl Drop for ReachesSubstWhileUnwinding {
    fn drop(&mut self) {
        reaches_subst();
    }
}

fn reaches_subst() {
    if true {
        std::hint::black_box(());
    }
}

fn loops_until_cancelled() {
    reaches_subst();
    let _guard = ReachesSubstWhileUnwinding;

    loop {
        // NOTE: With a mutation swapping `true` to `false`, this loop never ends, leading to a timeout.
        if true {
            break;
        }
    }
}

#[test]
fn test1() {
    loops_until_cancelled();
}
