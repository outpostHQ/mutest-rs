//! The codes `cargo mutest` exits with, for the scripts and CI that run it, following cargo-mutants'
//! codes; and how each test harness it runs passes its own code on to it. Each harness exits with
//! one of these codes too.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

/// The analysis completed, and the tests caught every mutation.
pub const SUCCESS: i32 = 0;
/// The command could not run as given: an argument is wrong, or a part of mutest-rs it needs is
/// not installed.
pub const USAGE: i32 = 1;
/// The analysis completed, and the tests missed some mutations: they all passed with one applied.
pub const MISSED: i32 = 2;
/// The analysis completed, and some mutations timed out: a test ran past its time limit with one
/// applied.
pub const TIMED_OUT: i32 = 3;
/// A harness did not build, or its tests failed with no mutation applied, so none of its mutations
/// was evaluated.
pub const BASELINE_FAILED: i32 = 4;
/// mutest-rs panicked, or a harness ended in a way none of these codes describes.
pub const PANIC: i32 = 101;

/// Whether a run that ended with this code evaluated every mutation it set out to.
pub fn analysis_completed(code: i32) -> bool {
    matches!(code, SUCCESS | MISSED | TIMED_OUT)
}

/// The code a run exits with that ended in each of these ways, as it does when it runs several
/// harnesses: the one that says the most went wrong. A timeout outranks a miss, as in cargo-mutants:
/// until the timeout is looked into, a missed mutation may be one a hung test would have caught. A
/// code that is none of these is a harness's that ended abnormally, and counts as a panic.
pub fn worst(codes: impl IntoIterator<Item = i32>) -> i32 {
    codes.into_iter()
        .map(|code| match code {
            SUCCESS | USAGE | MISSED | TIMED_OUT | BASELINE_FAILED => code,
            _ => PANIC,
        })
        .max_by_key(|&code| match code {
            SUCCESS => 0,
            MISSED => 1,
            TIMED_OUT => 2,
            BASELINE_FAILED => 3,
            USAGE => 4,
            _ => 5,
        })
        .unwrap_or(SUCCESS)
}

/// Set by `cargo mutest`, for the harnesses Cargo runs, to a file each records its exit code in.
/// Cargo runs them all, then exits 101 if any of them failed, whichever it was and however it failed.
pub const LOG_VAR: &str = "MUTEST_EXIT_CODE_LOG";

/// Records that the harness with this process id has started. A harness that records no exit after
/// it ended in a way it could not record, such as being killed.
pub fn record_start(log: &Path, pid: u32) -> io::Result<()> {
    append(log, &format!("started {pid}\n"))
}

pub fn record_exit(log: &Path, pid: u32, code: i32) -> io::Result<()> {
    append(log, &format!("exited {pid} {code}\n"))
}

/// Each line in one write, at the end of the file, as any harness of the run may be recording.
fn append(log: &Path, line: &str) -> io::Result<()> {
    OpenOptions::new().create(true).append(true).open(log)?.write_all(line.as_bytes())
}

/// The code each harness recorded, in the order they started. A harness that recorded its start
/// and no exit counts as having panicked.
pub fn read(log: &str) -> Vec<i32> {
    let mut harnesses = Vec::<(u32, Option<i32>)>::new();
    for line in log.lines() {
        match line.split(' ').collect::<Vec<_>>()[..] {
            ["started", pid] if let Ok(pid) = pid.parse() => harnesses.push((pid, None)),
            ["exited", pid, code] if let (Ok(pid), Ok(code)) = (pid.parse(), code.parse()) => {
                // A process id can go to a later harness once an earlier one has exited.
                match harnesses.iter_mut().rev().find(|(started, exit)| *started == pid && exit.is_none()) {
                    Some((_, exit)) => *exit = Some(code),
                    None => harnesses.push((pid, Some(code))),
                }
            }
            _ => {}
        }
    }
    harnesses.into_iter().map(|(_, code)| code.unwrap_or(PANIC)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_outranks_a_miss_and_a_panic_outranks_everything() {
        assert_eq!(worst([]), SUCCESS);
        assert_eq!(worst([SUCCESS, MISSED, SUCCESS]), MISSED);
        assert_eq!(worst([MISSED, TIMED_OUT]), TIMED_OUT);
        assert_eq!(worst([TIMED_OUT, BASELINE_FAILED]), BASELINE_FAILED);
        assert_eq!(worst([BASELINE_FAILED, USAGE]), USAGE);
        assert_eq!(worst([USAGE, PANIC, MISSED]), PANIC);
        // Killed by SIGKILL, as a shell reports it.
        assert_eq!(worst([MISSED, 137]), PANIC);
    }

    #[test]
    fn each_harness_is_read_with_the_code_it_recorded() {
        let log = "started 10\nexited 10 0\nstarted 11\nstarted 12\nexited 12 2\nexited 11 3\n";

        assert_eq!(read(log), [SUCCESS, TIMED_OUT, MISSED]);
    }

    #[test]
    fn a_harness_that_recorded_no_exit_counts_as_having_panicked() {
        assert_eq!(read("started 10\nexited 10 2\nstarted 11\n"), [MISSED, PANIC]);
    }

    #[test]
    fn a_process_id_taken_again_by_a_later_harness_is_read_as_that_harness() {
        assert_eq!(read("started 10\nexited 10 2\nstarted 10\nexited 10 0\n"), [MISSED, SUCCESS]);
    }

    #[test]
    fn what_a_harness_records_is_read_back() {
        let log = std::env::temp_dir().join(format!("mutest-exit-code-log-{}", std::process::id()));
        let _ = std::fs::remove_file(&log);

        record_start(&log, 10).unwrap();
        record_start(&log, 11).unwrap();
        record_exit(&log, 11, MISSED).unwrap();

        assert_eq!(read(&std::fs::read_to_string(&log).unwrap()), [PANIC, MISSED]);
        std::fs::remove_file(&log).unwrap();
    }
}
