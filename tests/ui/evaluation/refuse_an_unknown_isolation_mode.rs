//@ run: exit 1
//@ stdout
//@ stderr: empty
//@ run-flags: --isolate=everything

//! An option value the harness does not know ends the run before it starts, with the code for a
//! command that cannot run as given, not with a panic.

#[test]
fn test() {}
