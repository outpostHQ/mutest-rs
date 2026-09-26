//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: continue_break_swap

//! A `loop` with no `break` has type `!`, which coerces to whatever its place expects. A `break`
//! swapped in makes it `()` and lets it finish, so the swap only compiles where the loop's value
//! is `()`, or where it is a statement that code after it does not rely on to diverge. And a
//! `break` out of a labeled block has no loop to `continue`.

#![allow(unreachable_code)]

// Coerced to `Result`: not mutated.
fn first_even(values: &[u32]) -> Result<u32, ()> {
    let mut i = 0;
    loop {
        let Some(&value) = values.get(i) else { return Err(()) };
        i += 1;
        if value % 2 == 1 {
            continue;
        }
        return Ok(value);
    }
}

// The last statement of a body that returns `u32`, which only type-checks because the loop
// diverges: not mutated.
fn count_in_last_statement(limit: u32) -> u32 {
    let mut count = 0;
    loop {
        count += 1;
        if count < limit {
            continue;
        }
        return count;
    };
}

// Coerced to `()`: mutated.
fn count_to(limit: u32, count: &mut u32) {
    loop {
        *count += 1;
        if *count < limit {
            continue;
        }
        return;
    }
}

// A statement with code after it: mutated.
fn count_before_tail(limit: u32) -> u32 {
    let mut count = 0;
    loop {
        count += 1;
        if count < limit {
            continue;
        }
        return count;
    }
    0
}

// Leaves a labeled block: not mutated.
fn first_positive(values: &[i32]) -> Option<i32> {
    let mut found = None;
    'search: {
        for &value in values {
            if value > 0 {
                found = Some(value);
                break 'search;
            }
        }
    }
    found
}

#[test]
fn test() {
    assert_eq!(Ok(2), first_even(&[1, 2]));
    assert_eq!(Some(2), first_positive(&[-1, 2]));
    assert_eq!(3, count_in_last_statement(3));
    let mut count = 0;
    count_to(3, &mut count);
    assert_eq!(3, count);
    assert_eq!(3, count_before_tail(3));
}
