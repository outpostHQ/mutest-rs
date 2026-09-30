//@ run
//@ stdout
//@ stderr
//@ mutest-flags: --crate-kind=integration-tests

//! An integration test that does not link its package's library is skipped with a warning.

#[test]
fn is_not_run() {
    panic!("a skipped test ran");
}
