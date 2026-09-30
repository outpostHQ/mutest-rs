//! How a worker proves it finished the analysis, so that tested code exiting early is not taken for success.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Read, Write};
use std::path::{self, Path, PathBuf};
use std::process::{self, Command};

use mutest_exit_code as exit_code;

const PATH_VAR: &str = "__MUTEST_COMPLETION_PATH";
const TOKEN_VAR: &str = "__MUTEST_COMPLETION_TOKEN";
const RECORD_LIMIT: u64 = 256;

/// The supervisor's exit code: a completed status stands only with the worker's matching record.
pub(crate) fn outcome(status: Option<i32>, completed: Option<i32>, cancelled: bool, incomplete: bool) -> i32 {
    match status {
        _ if cancelled || incomplete => exit_code::PANIC,
        Some(code) if exit_code::analysis_completed(code) && completed != Some(code) => exit_code::PANIC,
        Some(code) => code,
        None => exit_code::PANIC,
    }
}

/// The record one worker writes when it finishes; an empty placeholder until then.
pub(crate) struct Completion {
    path: PathBuf,
    token: String,
}

impl Completion {
    pub(crate) fn create() -> io::Result<Self> {
        let random = RandomState::new();
        let token = format!("{:016x}{:016x}", random.hash_one(0), random.hash_one(1));
        let path = path::absolute(env::temp_dir())?.join(format!("mutest-completion-{}-{token}", process::id()));
        OpenOptions::new().write(true).create_new(true).open(&path)?;
        Ok(Self { path, token })
    }

    pub(crate) fn pass_to(&self, command: &mut Command) {
        command.env(PATH_VAR, &self.path).env(TOKEN_VAR, &self.token);
    }

    /// The code `worker` recorded, `None` if it has not recorded one yet, or an error for anything else at the path.
    pub(crate) fn read(&self, worker: u32) -> io::Result<Option<i32>> {
        // Checked before opening, so that a FIFO put in its place cannot block the supervisor.
        let metadata = fs::symlink_metadata(&self.path)?;
        if !metadata.is_file() {
            return Err(io::Error::other("completion record is not a regular file"));
        }
        if metadata.len() == 0 {
            return Ok(None);
        }
        let mut text = String::new();
        File::open(&self.path)?.take(RECORD_LIMIT).read_to_string(&mut text)?;
        parse(&text, &self.token, worker)
            .map(Some)
            .ok_or_else(|| io::Error::other("invalid completion record"))
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("pending"));
    }
}

fn parse(text: &str, token: &str, worker: u32) -> Option<i32> {
    [exit_code::SUCCESS, exit_code::MISSED, exit_code::TIMED_OUT]
        .into_iter()
        .find(|code| text == format!("{token} {worker} {code}\n"))
}

pub(crate) fn record(code: i32) -> io::Result<()> {
    if !exit_code::analysis_completed(code) {
        return Err(io::Error::other("not a completed analysis status"));
    }
    let path = env::var_os(PATH_VAR).ok_or_else(|| io::Error::other("completion path missing"))?;
    let token = env::var(TOKEN_VAR).map_err(io::Error::other)?;
    write(Path::new(&path), &format!("{token} {} {code}\n", process::id()))
}

/// Replaces the placeholder in one rename, so that the supervisor never reads a partial record.
fn write(path: &Path, record: &str) -> io::Result<()> {
    let placeholder = fs::symlink_metadata(path)?;
    if !placeholder.is_file() || placeholder.len() != 0 {
        return Err(io::Error::other("completion already recorded or replaced"));
    }
    let staging = path.with_extension("pending");
    let mut file = OpenOptions::new().write(true).create_new(true).open(&staging)?;
    file.write_all(record.as_bytes())?;
    file.sync_all()?;
    fs::rename(staging, path)
}

pub(crate) fn finish(code: i32) {
    if crate::journal::worker().is_some_and(|journal| !journal.healthy()) {
        crate::test_runner::progress::fail("journal write failed");
    }
    if let Err(error) = crate::test_runner::progress::ready() {
        crate::test_runner::progress::fail(error);
    }
    if let Err(error) = record(code) {
        crate::test_runner::progress::fail(error);
    }
    if let Err(error) = crate::test_runner::progress::terminal(true, code) {
        crate::test_runner::progress::fail(error);
    }
    if code != 0 {
        process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_complete_record_for_this_worker_is_accepted() {
        assert_eq!(parse("nonce 12 0\n", "nonce", 12), Some(0));
        for text in [
            "",
            "nonce 12 0",
            "nonce 13 0\n",
            "other 12 0\n",
            "nonce 12 0\nnonce 12 0\n",
        ] {
            assert_eq!(parse(text, "nonce", 12), None);
        }
    }

    #[test]
    fn a_record_is_read_back_once_and_cannot_be_written_twice() {
        let completion = Completion::create().unwrap();
        assert_eq!(completion.read(12).unwrap(), None);

        write(&completion.path, &format!("{} 12 2\n", completion.token)).unwrap();

        assert_eq!(completion.read(12).unwrap(), Some(2));
        assert!(completion.read(13).is_err());
        assert!(write(&completion.path, &format!("{} 12 0\n", completion.token)).is_err());
        assert_eq!(completion.read(12).unwrap(), Some(2));
    }

    #[test]
    fn anything_but_a_regular_file_at_the_path_is_an_error() {
        let completion = Completion::create().unwrap();
        fs::remove_file(&completion.path).unwrap();
        assert!(completion.read(12).is_err());

        fs::create_dir(&completion.path).unwrap();
        assert!(completion.read(12).is_err());
        assert!(write(&completion.path, &format!("{} 12 0\n", completion.token)).is_err());
        fs::remove_dir(&completion.path).unwrap();
    }

    #[test]
    fn completion_never_overrides_interruption_or_unknown_outcomes() {
        for code in [0, 2, 3] {
            assert_eq!(outcome(Some(code), Some(code), false, false), code);
            assert_eq!(outcome(Some(code), None, false, false), 101);
            assert_eq!(outcome(Some(code), Some(code), true, false), 101);
            assert_eq!(outcome(Some(code), Some(code), false, true), 101);
        }
    }
}
