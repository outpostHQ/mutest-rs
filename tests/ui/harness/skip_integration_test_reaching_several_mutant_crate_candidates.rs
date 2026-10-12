//@ run
//@ stdout
//@ stderr
//@ aux-build: crate_with_generated_mod.rs
//@ aux-build: mutant_crate_candidate.rs
//@ aux-build: other_mutant_crate_candidate.rs
//@ aux-build: links_mutant_crate_candidates.rs
//@ mutest-flags: --crate-kind=integration-tests

//! An integration test that names none of the several mutant crate candidates it links is skipped with a warning.

extern crate links_mutant_crate_candidates;

#[test]
fn is_not_run() {
    panic!("a skipped test ran");
}
