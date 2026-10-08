//@ run: exit 1
//@ stdout
//@ stderr: empty
//@ run-flags: --isolate=everything

//! An unknown isolation mode is a usage error, not a panic.

#[test]
fn test() {}
