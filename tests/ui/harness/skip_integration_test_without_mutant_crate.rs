//@ run
//@ stdout
//@ stderr
//@ mutest-flags: --crate-kind=integration-tests

//! An integration test that names nothing from its package's library does not link it, so no
//! mutation can reach its tests. That is no reason to stop the run: it is reported and skipped.

#[test]
fn is_not_run() {
    panic!("a skipped test ran");
}
