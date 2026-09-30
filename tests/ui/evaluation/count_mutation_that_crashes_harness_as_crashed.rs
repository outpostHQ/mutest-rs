//@ run: exit 101
//@ stdout
//@ eval-stream
//@ mutation-operators: fn_return_default

//! Recursive `Default` aborts on stack overflow; analysis stays incomplete and retains its journal.

struct Limit(u32);

impl Default for Limit {
    fn default() -> Self {
        Limit(3)
    }
}

fn double(value: u32) -> u32 {
    value * 2
}

#[test]
fn test() {
    assert_eq!(3, Limit::default().0);
    assert_eq!(4, double(2));
}
