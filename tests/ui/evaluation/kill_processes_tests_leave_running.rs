//@ run
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default

//! Descendants in separate process groups must not outlive the harness.

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
    // NOTE: `sleep` may not exist on every platform, in which case there is nothing to clean up.
    let _ = cmd.spawn();
}

#[test]
fn leaves_two_processes_running() {
    start_sleeping(false);
    start_sleeping(true);
    assert_eq!(42, answer());
}
