//! Runs an isolated test under an owner process that reaps the test and everything it leaves running.

use std::io::{self, Read};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use super::{ControlMsg, TestResult};

pub(super) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const REPORT_TIMEOUT: Duration = Duration::from_secs(1);
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(2);
pub(super) const OUTPUT_LIMIT: usize = 1024 * 1024;
/// Set for an isolated test run without an owner, so that the test ends with the process that started it.
pub(crate) const TEST_RUNNER_PID_VAR: &str = "__MUTEST_TEST_RUNNER_PID";

pub(super) fn run(
    command: Command,
    control: Option<&mpsc::Receiver<ControlMsg>>,
    timeout: Option<Duration>,
) -> io::Result<(TestResult, Duration, Vec<u8>)> {
    #[cfg(target_os = "linux")]
    return linux::run(command, control, timeout);
    #[cfg(not(target_os = "linux"))]
    return portable::run(command, control, timeout);
}

/// What has been read from a pipe: at most `OUTPUT_LIMIT` bytes, and how many more there were.
#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    discarded: usize,
    error: Option<io::Error>,
}

/// A pipe read to its end on a thread of its own, so that a descendant holding it open cannot block the test's monitor.
struct Capture {
    output: Arc<Mutex<Captured>>,
    reader: Option<thread::JoinHandle<()>>,
}

impl Capture {
    fn new(pipe: Option<impl Read + Send + 'static>) -> Self {
        let output = Arc::new(Mutex::new(Captured::default()));
        let reader = pipe.map(|mut pipe| {
            let output = Arc::clone(&output);
            thread::spawn(move || {
                let mut buffer = [0; 16384];
                loop {
                    let read = pipe.read(&mut buffer);
                    let mut output = output.lock().unwrap_or_else(PoisonError::into_inner);
                    match read {
                        Ok(0) => return,
                        Ok(n) => {
                            let keep = n.min(OUTPUT_LIMIT - output.bytes.len());
                            output.bytes.extend_from_slice(&buffer[..keep]);
                            output.discarded = output.discarded.saturating_add(n - keep);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            output.error = Some(error);
                            return;
                        }
                    }
                }
            })
        });
        Self { output, reader }
    }

    /// Whether the pipe has been read to its end.
    fn closed(&self) -> bool {
        self.reader.as_ref().is_none_or(|reader| reader.is_finished())
    }

    fn finish(self) -> io::Result<Vec<u8>> {
        let mut output = std::mem::take(&mut *self.output.lock().unwrap_or_else(PoisonError::into_inner));
        if let Some(error) = output.error { return Err(error); }
        if output.discarded > 0 {
            output.bytes.extend_from_slice(format!("\n[mutest truncated {} output bytes]\n", output.discarded).as_bytes());
        }
        Ok(output.bytes)
    }
}

/// Runs the test directly, without an owner. What the test leaves running ends with it: on Windows its job ends it,
/// elsewhere its process group does, and the test ends with this process.
#[cfg(any(not(target_os = "linux"), test))]
mod portable {
    use std::process::{Child, ExitStatus};
    use std::time::Instant;

    use super::*;

    pub(super) fn run(
        mut command: Command,
        control: Option<&mpsc::Receiver<ControlMsg>>,
        timeout: Option<Duration>,
    ) -> io::Result<(TestResult, Duration, Vec<u8>)> {
        #[cfg(unix)]
        {
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
            command.env(TEST_RUNNER_PID_VAR, std::process::id().to_string());
        }
        let mut child = command.spawn()?;
        // Best effort: without a job, `drained` still stops reading what a leftover process holds.
        #[cfg(windows)]
        let job = job::Job::holding(&child).ok();
        let (stdout, stderr) = (Capture::new(child.stdout.take()), Capture::new(child.stderr.take()));
        let start = Instant::now();
        let result = loop {
            if let Some(status) = exited(&mut child)? {
                break TestResult::from_exit_status(status, timeout, Some(start.elapsed()));
            }
            if control.is_some_and(|control| !matches!(control.try_recv(), Err(mpsc::TryRecvError::Empty))) {
                break kill(&mut child, TestResult::Ignored)?;
            }
            if timeout.is_some_and(|timeout| start.elapsed() > timeout) {
                break kill(&mut child, TestResult::TimedOut)?;
            }
            thread::sleep(POLL_INTERVAL);
        };
        let elapsed = start.elapsed();
        // The test has ended, so closing its job ends only what it left running, and frees the pipes.
        #[cfg(windows)]
        drop(job);
        Ok((result, elapsed, drained(stdout, stderr)?))
    }

    /// The exit status of the test, once it has exited and what it left in its process group is killed.
    #[cfg(unix)]
    fn exited(child: &mut Child) -> io::Result<Option<ExitStatus>> {
        let pid = child.id();
        match crate::supervisor::exited_child(libc::P_PID, pid as libc::id_t, libc::WNOHANG) {
            Ok(0) => Ok(None),
            // The unreaped test still holds its group id, so no other group can take it yet.
            Ok(_) => {
                let _ = crate::supervisor::kill_group(pid as libc::pid_t);
                child.wait().map(Some)
            }
            Err(error) if error.raw_os_error() == Some(libc::EINTR) => Ok(None),
            Err(error) => Err(error),
        }
    }
    #[cfg(windows)]
    fn exited(child: &mut Child) -> io::Result<Option<ExitStatus>> {
        child.try_wait()
    }

    fn kill(child: &mut Child, result: TestResult) -> io::Result<TestResult> {
        #[cfg(unix)]
        if crate::supervisor::kill_group(child.id() as libc::pid_t).is_err() {
            child.kill()?;
        }
        #[cfg(windows)]
        child.kill()?;
        child.wait()?;
        Ok(result)
    }

    /// The output once both pipes close, or what arrived within `REPORT_TIMEOUT` of the test ending:
    /// with no owner to reap it, a process the test left running holds a pipe for as long as it lives.
    fn drained(stdout: Capture, stderr: Capture) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + REPORT_TIMEOUT;
        while !(stdout.closed() && stderr.closed()) && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        let held = !(stdout.closed() && stderr.closed());
        let mut output = stdout.finish()?;
        output.extend(stderr.finish()?);
        if held {
            output.extend_from_slice(b"\n[mutest stopped reading: a process the test started still holds its output]\n");
        }
        Ok(output)
    }

    /// Runs `script` under `sh` until the shell exits, with its stdout and stderr captured as `run` captures them.
    #[cfg(all(test, unix))]
    fn finished(script: &str) -> (Capture, Capture) {
        let piped = std::process::Stdio::piped;
        let mut child = Command::new("sh").args(["-c", script]).stdout(piped()).stderr(piped()).spawn().unwrap();
        let captured = (Capture::new(child.stdout.take()), Capture::new(child.stderr.take()));
        child.wait().unwrap();
        captured
    }

    #[cfg(all(test, unix))]
    #[test]
    fn output_is_read_to_its_end_when_the_test_leaves_nothing_running() {
        let (stdout, stderr) = finished("echo out; echo err >&2");
        assert_eq!(String::from_utf8(drained(stdout, stderr).unwrap()).unwrap(), "out\nerr\n");
    }

    /// The shell exits at once, and the sleep it left running still holds stdout.
    #[cfg(all(test, unix))]
    #[test]
    fn output_a_process_left_running_still_holds_is_given_up_on() {
        let (stdout, stderr) = finished("sleep 600 & echo $!");
        let started = Instant::now();
        let output = String::from_utf8(drained(stdout, stderr).unwrap()).unwrap();
        let waited = started.elapsed();
        let left = output.lines().next().unwrap().parse().unwrap();
        // SAFETY: kill only sends a signal, here to the sleep the test left running.
        unsafe { libc::kill(left, libc::SIGKILL) };
        assert!(waited < STARTUP_TIMEOUT, "waited {waited:?}");
        assert!(output.ends_with("\n[mutest stopped reading: a process the test started still holds its output]\n"), "{output}");
    }

    /// Runs `script` under `sh` as an isolated test runs, and how long that took.
    #[cfg(all(test, unix))]
    fn run_script(script: &str, timeout: Option<Duration>) -> (TestResult, String, Duration) {
        let _turn = crate::supervisor::STARTING_PROCESSES.lock().unwrap_or_else(PoisonError::into_inner);
        let piped = std::process::Stdio::piped;
        let mut command = Command::new("sh");
        command.args(["-c", script]).stdin(std::process::Stdio::null()).stdout(piped()).stderr(piped());
        let started = Instant::now();
        let (result, _, output) = run(command, None, timeout).unwrap();
        (result, String::from_utf8(output).unwrap(), started.elapsed())
    }

    #[cfg(all(test, unix))]
    #[test]
    fn what_a_finished_test_left_in_its_process_group_ends_with_it() {
        let (result, output, waited) = run_script(&format!("sleep 600 & echo started; exit {}", crate::test_runner::TR_OK), None);
        assert_eq!((result, output.as_str()), (TestResult::Ok, "started\n"));
        assert!(waited < REPORT_TIMEOUT, "waited {waited:?}");
    }

    #[cfg(all(test, unix))]
    #[test]
    fn a_timed_out_test_ends_with_what_it_left_running() {
        let (result, output, _) = run_script("sleep 600 & echo started; sleep 600", Some(Duration::from_millis(200)));
        assert_eq!((result, output.as_str()), (TestResult::TimedOut, "started\n"));
    }

    /// The shell joins the job, then starts a ping that outlives it; closing the job ends that ping.
    #[cfg(all(test, windows))]
    #[test]
    fn output_is_read_to_its_end_once_the_job_ends_what_the_test_left_running() {
        use std::os::windows::process::CommandExt;

        let piped = std::process::Stdio::piped;
        let mut command = Command::new("cmd");
        command.raw_arg("/c ping -n 2 127.0.0.1 >nul & start /b ping -n 600 127.0.0.1");
        let mut child = command.stdout(piped()).stderr(piped()).spawn().unwrap();
        let job = job::Job::holding(&child).unwrap();
        let (stdout, stderr) = (Capture::new(child.stdout.take()), Capture::new(child.stderr.take()));
        child.wait().unwrap();
        drop(job);
        let output = String::from_utf8_lossy(&drained(stdout, stderr).unwrap()).into_owned();
        assert!(!output.contains("[mutest stopped reading"), "{output}");
    }
}

/// A Windows job object: closing it ends every process still in it, as the Linux owner reaps them.
#[cfg(windows)]
pub(crate) mod job {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::ptr;

    type Handle = *mut c_void;

    const KILL_ON_JOB_CLOSE: u32 = 0x2000;
    /// Ends a process in the job at its first unhandled exception, so a crash shows no error dialog.
    const DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x400;
    const EXTENDED_LIMIT_INFORMATION: i32 = 9;

    /// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`, the structure `EXTENDED_LIMIT_INFORMATION` names.
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io_counters: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, information: *const c_void, length: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    /// Ends every process still in the job when dropped.
    pub(crate) struct Job(Handle);

    impl Job {
        /// A new job that holds `child` and every process `child` starts from now on.
        pub(crate) fn holding(child: &Child) -> io::Result<Self> {
            // SAFETY: both arguments may be null: no security attributes, and no name.
            let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);
            let basic = BasicLimits { limit_flags: KILL_ON_JOB_CLOSE | DIE_ON_UNHANDLED_EXCEPTION, ..BasicLimits::default() };
            let limits = ExtendedLimits { basic, ..ExtendedLimits::default() };
            let length = u32::try_from(size_of::<ExtendedLimits>()).map_err(io::Error::other)?;
            let information = ptr::from_ref(&limits).cast();
            // SAFETY: `information` points to the structure the class names, which outlives the call.
            if unsafe { SetInformationJobObject(job.0, EXTENDED_LIMIT_INFORMATION, information, length) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the job is open, and `child` keeps its process handle open while it is borrowed.
            if unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is open, and only this drop closes it.
            unsafe { CloseHandle(self.0) };
        }
    }

    #[cfg(test)]
    mod tests {
        use std::io::{BufRead, BufReader};
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        use super::*;

        const CREATE_DEFAULT_ERROR_MODE: u32 = 0x0400_0000;
        const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
        const CRASH_VAR: &str = "__MUTEST_CRASH_FIXTURE";

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn QueryInformationJobObject(job: Handle, class: i32, information: *mut c_void, length: u32, returned: *mut u32) -> i32;
            fn RaiseException(code: u32, flags: u32, argument_count: u32, arguments: *const usize);
        }

        /// Prints the limits of its job once its stdin closes, then crashes; when not started by the test below, it returns at once.
        #[test]
        fn crash_fixture() {
            if std::env::var_os(CRASH_VAR).is_none() {
                return;
            }
            let _ = std::io::stdin().read_line(&mut String::new());
            let mut limits = ExtendedLimits::default();
            let length = u32::try_from(size_of::<ExtendedLimits>()).unwrap();
            // SAFETY: a null job names the job of this process, and `limits` is the structure the class names.
            let queried = unsafe { QueryInformationJobObject(ptr::null_mut(), EXTENDED_LIMIT_INFORMATION, ptr::from_mut(&mut limits).cast(), length, ptr::null_mut()) };
            assert_ne!(queried, 0, "{}", io::Error::last_os_error());
            println!("job limits {}", limits.basic.limit_flags);
            // SAFETY: the exception has no arguments, and no handler in this process catches it.
            unsafe { RaiseException(STATUS_ACCESS_VIOLATION, 0, 0, ptr::null()) };
        }

        /// The fixture starts with the default error mode, under which a crash can show an error dialog.
        #[test]
        fn a_process_in_the_job_shows_no_crash_dialog() {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "test_runner::subprocess::job::tests::crash_fixture", "--nocapture", "--test-threads=1"])
                .env(CRASH_VAR, "1").creation_flags(CREATE_DEFAULT_ERROR_MODE)
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
                .spawn().unwrap();
            let job = Job::holding(&child).unwrap();
            drop(child.stdin.take());
            let stdout = BufReader::new(child.stdout.take().unwrap());
            let limits = stdout.lines().map_while(Result::ok)
                .find_map(|line| line.rsplit_once("job limits ").and_then(|(_, limits)| limits.trim().parse::<u32>().ok()));
            let status = child.wait().unwrap();
            drop(job);
            assert_eq!(limits.map(|limits| limits & DIE_ON_UNHANDLED_EXCEPTION), Some(DIE_ON_UNHANDLED_EXCEPTION), "job limits {limits:?}");
            assert_eq!(status.code().map(i32::cast_unsigned), Some(STATUS_ACCESS_VIOLATION));
        }
    }
}

pub(crate) fn dispatch() {
    #[cfg(target_os = "linux")]
    linux::dispatch();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::env;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{self, Child, ExitStatus, Stdio};
    use std::sync::PoisonError;
    use std::thread;
    use std::time::Instant;

    use super::*;

    const OWNER: &str = "__MUTEST_TEST_OWNER";
    const TIMEOUT: &str = "__MUTEST_TEST_OWNER_TIMEOUT_NANOS";
    const READY: u8 = 0xa1;
    const EXITED: u8 = 0;
    const TIMED_OUT: u8 = 1;
    const CANCELLED: u8 = 2;
    const FRAME_LEN: usize = 14;

    #[cfg(test)]
    fn scenario_is(name: &str) -> bool {
        env::var("MUTEST_LIFECYCLE_SCENARIO").is_ok_and(|scenario| scenario == name)
    }

    impl Capture {
        /// The last `len` bytes read so far.
        fn tail(&self, len: usize) -> String {
            let output = self.output.lock().unwrap_or_else(PoisonError::into_inner);
            String::from_utf8_lossy(&output.bytes[output.bytes.len().saturating_sub(len)..]).into_owned()
        }
    }

    /// The owner process of one isolated test, and the socket it reports on.
    struct Owner {
        child: Option<Child>,
        channel: UnixStream,
        cleanup_reported: bool,
    }

    impl Owner {
        /// The owner's exit status once it has exited, after which it is no longer waited for.
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            let Some(child) = &mut self.child else {
                return Ok(None);
            };
            let status = child.try_wait()?;
            if status.is_some() {
                self.child = None;
            }
            Ok(status)
        }
    }

    fn exits_within(child: &mut Child, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        loop {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    impl Drop for Owner {
        /// Asks an owner that is still running to cancel, and kills it if it does not exit in time.
        fn drop(&mut self) {
            let Some(child) = &mut self.child else {
                return;
            };
            let _ = self.channel.write(&[CANCELLED]);
            child.stdout.take();
            child.stderr.take();
            let grace = if self.cleanup_reported { Duration::ZERO } else { CLEANUP_TIMEOUT + REPORT_TIMEOUT };
            if !exits_within(child, grace) {
                let _ = child.kill();
                exits_within(child, REPORT_TIMEOUT);
            }
        }
    }

    fn spawn_owner(mut command: Command, timeout: Option<Duration>) -> io::Result<Owner> {
        let (channel, owner_end) = UnixStream::pair()?;
        channel.set_nonblocking(true)?;
        // NOTE: The owner receives its end of the channel as its stdin.
        command.env(OWNER, "1").stdin(Stdio::from(OwnedFd::from(owner_end)));
        match timeout {
            Some(timeout) => command.env(TIMEOUT, timeout.as_nanos().to_string()),
            None => command.env_remove(TIMEOUT),
        };
        Ok(Owner { child: Some(command.spawn()?), channel, cleanup_reported: false })
    }

    /// How far the owner has got, and until when it may take for the current stage.
    struct Stage {
        deadline: Option<Instant>,
        ready: bool,
        cancelled: bool,
        exited: bool,
    }

    impl Stage {
        /// The test started: it may run for its timeout, if it has one, then clean up and report.
        fn start_executing(&mut self, timeout: Option<Duration>) {
            self.ready = true;
            if !self.cancelled {
                self.deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout + CLEANUP_TIMEOUT + REPORT_TIMEOUT));
            }
        }

        fn allow(&mut self, bound: Duration) {
            self.deadline = Some(Instant::now() + bound);
        }

        /// Advances on what the owner has written: `READY` starts the test, and a whole frame reports its cleanup.
        fn read(&mut self, frame: &[u8], owner: &mut Owner, timeout: Option<Duration>) {
            if !self.ready && frame.first() == Some(&READY) {
                self.start_executing(timeout);
            }
            if !owner.cleanup_reported && frame.len() == FRAME_LEN {
                owner.cleanup_reported = true;
                self.allow(REPORT_TIMEOUT);
            }
        }

        /// Passes a requested cancellation on to an owner that has neither reported nor exited yet.
        fn forward_cancellation(&mut self, owner: &mut Owner, control: Option<&mpsc::Receiver<ControlMsg>>) -> io::Result<()> {
            if self.cancelled || owner.cleanup_reported || self.exited || !cancel_requested(control) {
                return Ok(());
            }
            cancel(&mut owner.channel)?;
            self.cancelled = true;
            self.allow(CLEANUP_TIMEOUT + REPORT_TIMEOUT);
            Ok(())
        }
    }

    fn cancel_requested(control: Option<&mpsc::Receiver<ControlMsg>>) -> bool {
        control.is_some_and(|control| !matches!(control.try_recv(), Err(mpsc::TryRecvError::Empty)))
    }

    fn owner_failed(status: ExitStatus, stdout: &Capture, stderr: &Capture) -> io::Error {
        io::Error::other(format!("isolated owner failed: {status}; stdout: {}; stderr: {}", stdout.tail(4096), stderr.tail(4096)))
    }

    pub(super) fn run(
        command: Command,
        control: Option<&mpsc::Receiver<ControlMsg>>,
        timeout: Option<Duration>,
    ) -> io::Result<(TestResult, Duration, Vec<u8>)> {
        let mut owner = spawn_owner(command, timeout)?;
        let child = owner.child.as_mut().unwrap();
        let stdout = Capture::new(child.stdout.take());
        let stderr = Capture::new(child.stderr.take());
        let mut stage = Stage { deadline: Some(Instant::now() + STARTUP_TIMEOUT), ready: false, cancelled: false, exited: false };
        let mut frame = Vec::new();
        loop {
            read_completion(&mut owner.channel, &mut frame)?;
            stage.read(&frame, &mut owner, timeout);
            stage.forward_cancellation(&mut owner, control)?;
            if let Some(status) = owner.try_wait()? {
                if !status.success() {
                    return Err(owner_failed(status, &stdout, &stderr));
                }
                stage.exited = true;
                stage.allow(REPORT_TIMEOUT);
            }
            if stage.exited && owner.cleanup_reported && stdout.closed() && stderr.closed() {
                break;
            }
            if stage.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "isolated owner missed its execution or teardown deadline"));
            }
            thread::sleep(POLL_INTERVAL);
        }
        let (result, elapsed) = decode(&frame, timeout)?;
        let mut output = stdout.finish()?;
        output.extend(stderr.finish()?);
        Ok((result, elapsed, output))
    }

    /// Reads a complete frame: `READY`, the outcome, the raw wait status, then the elapsed nanoseconds.
    fn decode(frame: &[u8], timeout: Option<Duration>) -> io::Result<(TestResult, Duration)> {
        let elapsed = Duration::from_nanos(u64::from_le_bytes(frame[6..14].try_into().unwrap()));
        let result = match frame[1] {
            EXITED => TestResult::from_exit_status(
                ExitStatus::from_raw(i32::from_le_bytes(frame[2..6].try_into().unwrap())),
                timeout,
                Some(elapsed),
            ),
            TIMED_OUT => TestResult::TimedOut,
            CANCELLED => TestResult::Ignored,
            _ => return Err(io::Error::other("invalid isolated owner outcome")),
        };
        Ok((result, elapsed))
    }

    fn read_completion(channel: &mut UnixStream, frame: &mut Vec<u8>) -> io::Result<()> {
        let mut buffer = [0; FRAME_LEN + 1];
        match channel.read(&mut buffer) {
            Ok(n) => frame.extend_from_slice(&buffer[..n]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            // Unread cancellation can reset after completion; the caller still checks exit and EOF.
            Err(error)
                if error.kind() == io::ErrorKind::ConnectionReset && frame.len() == FRAME_LEN => {}
            Err(error) => return Err(error),
        }
        if frame.len() > FRAME_LEN || frame.first().is_some_and(|byte| *byte != READY) {
            return Err(io::Error::other("invalid isolated owner completion frame"));
        }
        Ok(())
    }

    #[cfg(test)]
    #[test]
    fn unread_cancellation_reset_preserves_only_a_complete_frame() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.write_all(&[CANCELLED]).unwrap();
        let mut expected = [0; FRAME_LEN];
        expected[0] = READY;
        expected[1] = EXITED;
        writer.write_all(&expected).unwrap();
        drop(writer);
        let mut frame = Vec::new();
        read_completion(&mut reader, &mut frame).unwrap();
        read_completion(&mut reader, &mut frame).unwrap();
        assert_eq!(frame, expected);

        for bytes in [Vec::new(), vec![READY]] {
            let (mut reader, mut writer) = UnixStream::pair().unwrap();
            reader.write_all(&[CANCELLED]).unwrap();
            writer.write_all(&bytes).unwrap();
            drop(writer);
            let mut frame = Vec::new();
            let first = read_completion(&mut reader, &mut frame);
            let result = first.and_then(|()| read_completion(&mut reader, &mut frame));
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
            assert_eq!(frame, bytes);
        }
    }

    #[cfg(test)]
    #[test]
    fn reset_cannot_validate_an_invalid_or_oversized_completion() {
        for bytes in [vec![0; FRAME_LEN], vec![READY; FRAME_LEN + 1]] {
            let (mut reader, mut writer) = UnixStream::pair().unwrap();
            reader.write_all(&[CANCELLED]).unwrap();
            writer.write_all(&bytes).unwrap();
            drop(writer);
            let mut frame = Vec::new();
            assert_eq!(
                read_completion(&mut reader, &mut frame)
                    .unwrap_err()
                    .to_string(),
                "invalid isolated owner completion frame"
            );
        }
    }

    fn cancel(channel: &mut UnixStream) -> io::Result<()> {
        match channel.write_all(&[CANCELLED]) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    #[test]
    fn cancellation_after_owner_close_preserves_buffered_completion() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        let mut frame = [0; FRAME_LEN];
        frame[0] = READY;
        frame[1] = EXITED;
        writer.write_all(&frame).unwrap();
        drop(writer);
        cancel(&mut reader).unwrap();
        assert_eq!(read_to_close(&mut reader), frame);
        let (mut reader, writer) = UnixStream::pair().unwrap();
        drop(writer);
        cancel(&mut reader).unwrap();
        assert!(
            read_to_close(&mut reader).is_empty(),
            "closed control pipe invented a completion"
        );
    }

    // A child that another test spawns can hold the peer open over the cancellation; its exit then resets the read.
    #[cfg(test)]
    fn read_to_close(channel: &mut UnixStream) -> Vec<u8> {
        let mut received = Vec::new();
        let reset = channel.read_to_end(&mut received).err().map(|error| error.kind());
        assert!(reset.is_none_or(|kind| kind == io::ErrorKind::ConnectionReset), "{reset:?}");
        received
    }

    fn execute(channel: &mut UnixStream) -> io::Result<(u8, i32, Duration)> {
        let timeout = env::var(TIMEOUT)
            .ok()
            .map(|nanos| nanos.parse::<u128>().map_err(io::Error::other))
            .transpose()?
            .map(|nanos| Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX)));
        let child = Command::new(env::current_exe()?)
            .args(env::args_os().skip(1))
            .env_remove(OWNER)
            .env_remove(TIMEOUT)
            .stdin(Stdio::null())
            .spawn()?;
        let start = Instant::now();
        channel.write_all(&[READY])?;
        loop {
            let mut message = [0; 1];
            match channel.read(&mut message) {
                Ok(0) => return Ok((CANCELLED, 0, start.elapsed())),
                Ok(_) if message[0] == CANCELLED => return Ok((CANCELLED, 0, start.elapsed())),
                Ok(_) => return Err(io::Error::other("invalid isolated owner control message")),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
            // NOTE: Reaping any child also reaps the orphans this owner adopted.
            let (exited, status) = match crate::supervisor::reap_with_status(-1, libc::WNOHANG) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (0, 0),
                reaped => reaped?,
            };
            if exited == child.id() as libc::pid_t {
                return Ok((EXITED, status, start.elapsed()));
            }
            if timeout.is_some_and(|timeout| start.elapsed() >= timeout) {
                return Ok((TIMED_OUT, 0, start.elapsed()));
            }
            if exited == 0 {
                thread::sleep(POLL_INTERVAL);
            }
        }
    }

    #[cfg(test)]
    #[test]
    fn captured_output_limit_retains_exact_prefix_and_reports_discarded_bytes() {
        let input = (0..OUTPUT_LIMIT + 4133).map(|index| (index % 251) as u8).collect::<Vec<_>>();
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let sent = input.clone();
        let sender = thread::spawn(move || writer.write_all(&sent).unwrap());
        let capture = Capture::new(Some(reader));
        sender.join().unwrap();
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while !capture.closed() {
            assert!(Instant::now() < deadline, "bounded capture did not reach EOF");
            thread::yield_now();
        }
        let output = capture.finish().unwrap();
        assert_eq!(&output[..OUTPUT_LIMIT], &input[..OUTPUT_LIMIT]);
        assert_eq!(&output[OUTPUT_LIMIT..], b"\n[mutest truncated 4133 output bytes]\n");
    }

    /// Runs as the owner when this process was started as one, and exits; returns otherwise.
    pub(super) fn dispatch() {
        if env::var_os(OWNER).is_none() {
            return;
        }
        let result = own();
        if let Err(error) = &result {
            eprintln!("mutation analysis incomplete: isolated owner: {error}");
        }
        process::exit(if result.is_ok() { 0 } else { 101 });
    }

    fn own() -> io::Result<()> {
        let mut channel = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
        channel.set_nonblocking(true)?;
        #[cfg(test)]
        if scenario_is("failure-setup") {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        crate::supervisor::adopt_orphans()?;
        crate::supervisor::skip_crash_reports()?;
        let result = execute(&mut channel);
        crate::supervisor::kill_descendants_until(Instant::now() + CLEANUP_TIMEOUT)?;
        #[cfg(test)]
        if scenario_is("failure-cleanup") {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "injected cleanup timeout"));
        }
        #[cfg(test)]
        if scenario_is("failure-completion") {
            return Ok(());
        }
        let (outcome, status, elapsed) = result?;
        let nanos = u64::try_from(elapsed.as_nanos()).map_err(io::Error::other)?;
        let mut frame = [0; FRAME_LEN - 1];
        frame[0] = outcome;
        frame[1..5].copy_from_slice(&status.to_le_bytes());
        frame[5..13].copy_from_slice(&nanos.to_le_bytes());
        channel.write_all(&frame)?;
        #[cfg(test)]
        if scenario_is("failure-owner-exit") {
            thread::sleep(super::super::tests::FIXTURE_RUN_BOUND * 3);
        }
        Ok(())
    }
}
