//@ run: exit 3
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default
//@ run-flags: --isolate=all

//! The tests of isolated mutations run in child processes, so their timeouts are confirmed side by side:
//! each mutation of `next` makes the loop hang, and both rerun with longer limits.

fn next(count: u32) -> u32 {
    count + 1
}

#[test]
fn count_to_three() {
    let mut count = 0;
    while count != 3 {
        count = next(count);
    }
}
