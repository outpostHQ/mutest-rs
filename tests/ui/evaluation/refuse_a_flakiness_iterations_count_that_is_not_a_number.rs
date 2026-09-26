//@ run: exit 1
//@ stdout
//@ stderr: empty
//@ run-flags: --flakes=many

//! An option value the harness cannot read ends the run before it starts, with the code for a
//! command that cannot run as given, not with a panic.

#[test]
fn test() {}
