//@ run
//@ stdout
//@ stderr
//@ aux-build: crate_with_generated_mod.rs
//@ aux-build: mutant_crate_candidate.rs
//@ aux-build: other_mutant_crate_candidate.rs
//@ mutest-flags: --crate-kind=integration-tests

//! An integration test that names several mutant crate candidates, none of its package, is skipped with a warning.

extern crate mutant_crate_candidate;
extern crate other_mutant_crate_candidate;

#[test]
fn is_not_run() {
    panic!("a skipped test ran");
}
