//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

//! A test may start a process and never wait for it, as one does that fails an assertion before
//! it kills its child, and the harness leaves a test running once another has detected the
//! mutation. None of what they start may outlive the harness, whichever process group it is in:
//! a shell job, for one, is in a group of its own.

use std::process::{Command, Stdio};

fn answer() -> u32 {
    42
}

#[mutest::skip]
fn start_sleeping(own_process_group: bool) {
    let mut cmd = Command::new("sleep");
    cmd.arg("60").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    if own_process_group {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = own_process_group;
    // NOTE: There is no `sleep` to start everywhere, and this test is about what happens when there is.
    let _ = cmd.spawn();
}

#[test]
fn leaves_two_processes_running() {
    start_sleeping(false);
    start_sleeping(true);
    assert_eq!(42, answer());
}
