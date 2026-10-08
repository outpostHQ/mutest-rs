//! A child process that runs the listed tests of one run in turn, as the parent sees it.

use std::collections::VecDeque;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::panic;
use std::path::PathBuf;
use std::process::{self, Command};
use std::sync::atomic;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::super::{ControlMsg, TestResult, incomplete_isolation, isolated_command, subprocess, test};
use super::NEXT_LIST;

type Outcome = io::Result<(TestResult, Duration, Vec<u8>)>;
/// The result, run time and output of a test, and whether its child still runs.
pub(super) type Ran = (TestResult, Duration, Vec<u8>, bool);

/// A child process that runs the listed tests in turn, and the passes it has reported that are not yet given out.
pub(super) struct Child {
    pub(super) tests: VecDeque<test::TestId>,
    control_tx: mpsc::Sender<ControlMsg>,
    done_rx: mpsc::Receiver<Outcome>,
    exited: Option<Outcome>,
    done: bool,
    report: File,
    unread: Vec<u8>,
    passed: VecDeque<Duration>,
    list: PathBuf,
}

impl Child {
    pub(super) fn spawn(cmd_hook: Arc<dyn Fn(&mut Command) + Send + Sync>, tests: &[(test::TestId, &test::TestName)], no_capture: bool) -> io::Result<Self> {
        let list = env::temp_dir().join(format!("mutest-{}-{}.tests", process::id(), NEXT_LIST.fetch_add(1, atomic::Ordering::Relaxed)));
        fs::write(&list, tests.iter().map(|(_, name)| name.as_slice()).collect::<Vec<_>>().join("\n"))?;
        let report = fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(list.with_extension("passed"))?;
        let (control_tx, control_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let (first, child_list) = (tests[0].1.clone(), list.clone());
        thread::Builder::new().name(first.as_slice().to_owned()).spawn(move || {
            let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                subprocess::run(isolated_command(&first, &*cmd_hook, Some(&child_list), no_capture)?, Some(&control_rx), None)
            }));
            let _ = done_tx.send(outcome.unwrap_or_else(|_| Err(io::Error::other("isolated test monitor panicked"))));
        })?;
        let tests = tests.iter().map(|(id, _)| *id).collect();
        Ok(Self { tests, control_tx, done_rx, exited: None, done: false, report, unread: Vec::new(), passed: VecDeque::new(), list })
    }

    /// Reads the passes the child has reported since the last read; a line ends each.
    fn read_report(&mut self) -> io::Result<()> {
        self.report.read_to_end(&mut self.unread)?;
        while let Some(end) = self.unread.iter().position(|&byte| byte == b'\n') {
            let line = self.unread.drain(..=end).collect::<Vec<_>>();
            let nanos = std::str::from_utf8(&line[..end]).ok().and_then(|nanos| nanos.parse().ok());
            self.passed.push_back(Duration::from_nanos(nanos.ok_or_else(|| io::Error::other("invalid isolated test report"))?));
        }
        Ok(())
    }

    /// Waits up to `bound` for the child to exit, then reads what it reported.
    fn wait(&mut self, bound: Duration) {
        let received = self.done_rx.recv_timeout(bound);
        if let Err(error) = self.read_report() { incomplete_isolation(&error.to_string()); }
        match received {
            Ok(outcome) => (self.exited, self.done) = (Some(outcome), true),
            Err(error) => if error == mpsc::RecvTimeoutError::Disconnected { incomplete_isolation("isolated monitor channel disconnected") },
        }
    }

    /// Kills the child if it still runs, and waits until its monitor has reaped it.
    fn stop(&mut self) {
        let _ = self.control_tx.send(ControlMsg::KillChildProcess);
        let deadline = Instant::now() + subprocess::STARTUP_TIMEOUT + subprocess::CLEANUP_TIMEOUT * 2 + subprocess::REPORT_TIMEOUT * 2;
        while !self.done {
            if Instant::now() >= deadline { incomplete_isolation("isolated monitor missed its cleanup deadline"); }
            self.wait(subprocess::POLL_INTERVAL);
        }
    }

    /// The output of the exited child; its exit is the result of the test that did not report a pass.
    fn exit_output(&mut self) -> (TestResult, Vec<u8>) {
        let (result, _, output) = self.exited.take().expect("the child has exited").unwrap_or_else(|error| incomplete_isolation(&error.to_string()));
        (result, output)
    }

    /// The result, run time and output of the next test, which started at `started`, and whether the child still runs.
    pub(super) fn next_result(&mut self, timeout: Option<Duration>, started: Instant) -> Ran {
        self.tests.pop_front();
        loop {
            if let Some(exec_time) = self.passed.pop_front() { return (within(exec_time, timeout), exec_time, Vec::new(), !self.done); }
            if self.exited.is_some() { return self.exit_result(timeout, started); }
            if timeout.is_some_and(|timeout| started.elapsed() > timeout) { return self.kill_timed_out(timeout, started); }
            self.wait(subprocess::POLL_INTERVAL);
        }
    }

    /// The exit of the child, as the result of the test that did not report a pass.
    fn exit_result(&mut self, timeout: Option<Duration>, started: Instant) -> Ran {
        let (result, output) = self.exit_output();
        let exec_time = started.elapsed();
        (if result == TestResult::Ok { within(exec_time, timeout) } else { result }, exec_time, output, false)
    }

    /// Kills the child of a test past its timeout; the test can pass just before the child is killed.
    fn kill_timed_out(&mut self, timeout: Option<Duration>, started: Instant) -> Ran {
        self.stop();
        let output = self.exit_output().1;
        let (result, exec_time) = self.passed.pop_front().map_or((TestResult::TimedOut, started.elapsed()), |exec_time| (within(exec_time, timeout), exec_time));
        (result, exec_time, output, false)
    }
}

/// A test that passed after its timeout has timed out.
fn within(exec_time: Duration, timeout: Option<Duration>) -> TestResult {
    if timeout.is_some_and(|timeout| exec_time > timeout) { TestResult::TimedOut } else { TestResult::Ok }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.done { self.stop(); }
        if let Some(Err(error)) = &self.exited { incomplete_isolation(&error.to_string()); }
        let _ = fs::remove_file(&self.list);
        let _ = fs::remove_file(self.list.with_extension("passed"));
    }
}
