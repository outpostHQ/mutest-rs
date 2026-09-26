//@ run: exit 101
//@ stdout: empty
//@ run-flags: --isolate=everything

//! A harness that panics exits 101, as any Rust program does, and records that for `cargo mutest`;
//! here over an option `cargo mutest` itself would have refused.

#[test]
fn test() {}
