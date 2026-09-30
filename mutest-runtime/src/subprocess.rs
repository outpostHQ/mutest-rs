//! Runs an isolated test under an owner process that reaps the test and everything it leaves running.

use std::io;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use super::{ControlMsg, TestResult};

pub(super) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const REPORT_TIMEOUT: Duration = Duration::from_secs(1);
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(2);
pub(super) const OUTPUT_LIMIT: usize = 1024 * 1024;

pub(super) fn run(
    command: Command,
    control: Option<mpsc::Receiver<ControlMsg>>,
    timeout: Option<Duration>,
) -> io::Result<(TestResult, Duration, Vec<u8>)> {
    #[cfg(target_os = "linux")]
    return linux::run(command, control, timeout);
    #[cfg(not(target_os = "linux"))]
    return portable::run(command, control, timeout);
}

/// Runs the test directly, without an owner, so processes it leaves running are not reaped.
#[cfg(not(target_os = "linux"))]
mod portable {
    use std::io::Read;
    use std::process::Child;
    use std::thread;
    use std::time::Instant;

    use super::*;

    pub(super) fn run(
        mut command: Command,
        control: Option<mpsc::Receiver<ControlMsg>>,
        timeout: Option<Duration>,
    ) -> io::Result<(TestResult, Duration, Vec<u8>)> {
        let mut child = command.spawn()?;
        let readers = [child.stdout.take().map(|pipe| thread::spawn(|| read_capped(pipe))), child.stderr.take().map(|pipe| thread::spawn(|| read_capped(pipe)))];
        let start = Instant::now();
        let result = loop {
            if let Some(status) = child.try_wait()? {
                break TestResult::from_exit_status(status, timeout, Some(start.elapsed()));
            }
            if control.as_ref().is_some_and(|control| !matches!(control.try_recv(), Err(mpsc::TryRecvError::Empty))) {
                break kill(&mut child, TestResult::Ignored)?;
            }
            if timeout.is_some_and(|timeout| start.elapsed() > timeout) {
                break kill(&mut child, TestResult::TimedOut)?;
            }
            thread::sleep(POLL_INTERVAL);
        };
        let elapsed = start.elapsed();
        let mut output = Vec::new();
        for reader in readers.into_iter().flatten() {
            output.extend(reader.join().map_err(|_| io::Error::other("test output reader panicked"))?);
        }
        Ok((result, elapsed, output))
    }

    fn kill(child: &mut Child, result: TestResult) -> io::Result<TestResult> {
        child.kill()?;
        child.wait()?;
        Ok(result)
    }

    /// Reads the pipe to its end, keeping at most `OUTPUT_LIMIT` bytes.
    fn read_capped(mut pipe: impl Read) -> Vec<u8> {
        let mut bytes = Vec::new();
        let _ = (&mut pipe).take(OUTPUT_LIMIT as u64).read_to_end(&mut bytes);
        let discarded = io::copy(&mut pipe, &mut io::sink()).unwrap_or(0);
        if discarded > 0 {
            bytes.extend_from_slice(format!("\n[mutest truncated {discarded} output bytes]\n").as_bytes());
        }
        bytes
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
    use std::sync::{Arc, Mutex, PoisonError};
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

    /// What has been read from a pipe: at most `OUTPUT_LIMIT` bytes, and how many more there were.
    #[derive(Default)]
    struct Captured {
        bytes: Vec<u8>,
        discarded: usize,
        error: Option<io::Error>,
    }

    /// A pipe read to its end on a thread of its own, so that a descendant holding it open cannot block the owner.
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

        /// The last `len` bytes read so far.
        fn tail(&self, len: usize) -> String {
            let output = self.output.lock().unwrap_or_else(PoisonError::into_inner);
            String::from_utf8_lossy(&output.bytes[output.bytes.len().saturating_sub(len)..]).into_owned()
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
        control: Option<mpsc::Receiver<ControlMsg>>,
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
            stage.forward_cancellation(&mut owner, control.as_ref())?;
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
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        assert_eq!(received, frame);
        let (mut reader, writer) = UnixStream::pair().unwrap();
        drop(writer);
        cancel(&mut reader).unwrap();
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        assert!(
            received.is_empty(),
            "closed control pipe invented a completion"
        );
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
