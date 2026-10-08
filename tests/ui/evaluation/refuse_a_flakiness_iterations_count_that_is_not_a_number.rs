//@ run: exit 1
//@ stdout
//@ stderr: empty
//@ run-flags: --flakes=many

//! A flakiness iterations count that is not a number is a usage error, not a panic.

#[test]
fn test() {}
