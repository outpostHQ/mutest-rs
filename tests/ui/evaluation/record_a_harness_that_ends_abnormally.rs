//@ run: exit 101
//@ stdout
//@ stderr

//! An abnormal reference-run exit must propagate through the supervisor.

#[test]
fn ends_the_process() {
    std::process::exit(101);
}
