//! The exit codes of `cargo mutest` and its test harnesses, following cargo-mutants.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

pub const SUCCESS: i32 = 0;
/// An argument is wrong, or a part of mutest-rs that is needed is not installed.
pub const USAGE: i32 = 1;
pub const MISSED: i32 = 2;
pub const TIMED_OUT: i32 = 3;
/// A harness did not build, or its tests failed with no mutation applied.
pub const BASELINE_FAILED: i32 = 4;
/// mutest-rs panicked, or a harness ended in a way none of these codes describes.
pub const PANIC: i32 = 101;

pub fn analysis_completed(code: i32) -> bool {
    matches!(code, SUCCESS | MISSED | TIMED_OUT)
}

/// The code that says the most went wrong. A timeout outranks a miss, as in cargo-mutants.
pub fn worst(codes: impl IntoIterator<Item = i32>) -> i32 {
    const LEAST_TO_MOST_SEVERE: [i32; 6] =
        [SUCCESS, MISSED, TIMED_OUT, BASELINE_FAILED, USAGE, PANIC];

    codes
        .into_iter()
        .map(|code| {
            LEAST_TO_MOST_SEVERE
                .iter()
                .position(|&known| known == code)
                .unwrap_or(LEAST_TO_MOST_SEVERE.len() - 1)
        })
        .max()
        .map_or(SUCCESS, |severity| LEAST_TO_MOST_SEVERE[severity])
}

/// The log each harness records its exit code in, as Cargo only says whether any of them failed.
pub const LOG_VAR: &str = "MUTEST_EXIT_CODE_LOG";

/// A harness that records a start and no exit is read as having panicked.
pub fn record_start(log: &Path, pid: u32) -> io::Result<()> {
    append(log, &format!("started {pid}\n"))
}

pub fn record_exit(log: &Path, pid: u32, code: i32) -> io::Result<()> {
    append(log, &format!("exited {pid} {code}\n"))
}

fn append(log: &Path, line: &str) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?
        .write_all(line.as_bytes())
}

/// The code each harness recorded, in the order they started.
pub fn read(log: &str) -> Vec<i32> {
    let mut harnesses = Vec::<(u32, Option<i32>)>::new();
    for line in log.lines() {
        match line.split(' ').collect::<Vec<_>>()[..] {
            ["started", pid] if let Ok(pid) = pid.parse() => harnesses.push((pid, None)),
            ["exited", pid, code] if let (Ok(pid), Ok(code)) = (pid.parse(), code.parse()) => {
                record_exit_of_latest(&mut harnesses, pid, code);
            }
            _ => {}
        }
    }
    harnesses
        .into_iter()
        .map(|(_, code)| code.unwrap_or(PANIC))
        .collect()
}

/// A process id can go to a later harness once an earlier one has exited.
fn record_exit_of_latest(harnesses: &mut Vec<(u32, Option<i32>)>, pid: u32, code: i32) {
    match harnesses.iter_mut().rev().find(|(started, exit)| *started == pid && exit.is_none()) {
        Some((_, exit)) => *exit = Some(code),
        None => harnesses.push((pid, Some(code))),
    }
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
        assert_eq!(worst([MISSED, 128 + 9]), PANIC);
    }

    #[test]
    fn each_harness_is_read_with_the_code_it_recorded() {
        let log = "started 10\nexited 10 0\nstarted 11\nstarted 12\nexited 12 2\nexited 11 3\n";

        assert_eq!(read(log), [SUCCESS, TIMED_OUT, MISSED]);
    }

    #[test]
    fn a_harness_that_recorded_no_exit_counts_as_having_panicked() {
        assert_eq!(
            read("started 10\nexited 10 2\nstarted 11\n"),
            [MISSED, PANIC]
        );
    }

    #[test]
    fn a_process_id_taken_again_by_a_later_harness_is_read_as_that_harness() {
        assert_eq!(
            read("started 10\nexited 10 2\nstarted 10\nexited 10 0\n"),
            [MISSED, SUCCESS]
        );
    }

    #[test]
    fn what_a_harness_records_is_read_back() {
        let log = std::env::temp_dir().join(format!("mutest-exit-code-log-{}", std::process::id()));
        let _ = std::fs::remove_file(&log);

        record_start(&log, 10).unwrap();
        record_start(&log, 11).unwrap();
        record_exit(&log, 11, MISSED).unwrap();

        assert_eq!(
            read(&std::fs::read_to_string(&log).unwrap()),
            [PANIC, MISSED]
        );
        std::fs::remove_file(&log).unwrap();
    }
}
