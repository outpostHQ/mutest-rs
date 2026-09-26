//@ run
//@ stdout
//@ eval-stream
//@ mutation-operators: fn_return_default

//! A mutation can take down the whole process its tests run in, not just the test: returning
//! `Default::default()` from `default` itself recurses until the stack overflows, and Rust aborts
//! the process on a stack overflow. The mutation is counted as crashed, and the mutations after it
//! are still evaluated. The evaluation stream holds the tests run on either side of the crash,
//! the one it cut short included, under a single header.

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
