//@ run: exit 101
//@ stdout
//@ stderr: empty

//! A harness can end in a way it did not choose: a panic in mutest-rs ends it with 101, as it ends
//! any Rust program. A test that exits the whole process with 101, as this one does during the
//! reference run, ends it the same way. The harness exits with that code and records it for
//! `cargo mutest`, which exits 101 too.

#[test]
fn ends_the_process() {
    std::process::exit(101);
}
