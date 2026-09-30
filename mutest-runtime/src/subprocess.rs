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
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{self, Child, ExitStatus};
    use std::thread;
    use std::time::Instant;

    use super::*;

    const OWNER_FD: &str = "__MUTEST_TEST_OWNER_FD";
    const TIMEOUT: &str = "__MUTEST_TEST_OWNER_TIMEOUT_NANOS";
    const READY: u8 = 0xa1;
    const EXITED: u8 = 0;
    const TIMED_OUT: u8 = 1;
    const CANCELLED: u8 = 2;
    const FRAME_LEN: usize = 14;
    const DRAIN_QUANTUM: usize = 64 * 1024;

    /// Sets or clears `flag` among a descriptor's flags, the ones `get` reads and `set` writes.
    fn set_fd_flag(fd: i32, get: i32, set: i32, flag: i32, on: bool) -> io::Result<()> {
        // SAFETY: `fcntl` only reads and writes the flags of a descriptor the caller holds open.
        unsafe {
            let flags = libc::fcntl(fd, get);
            if flags < 0 || libc::fcntl(fd, set, if on { flags | flag } else { flags & !flag }) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn nonblocking(fd: i32) -> io::Result<()> {
        set_fd_flag(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK, true)
    }

    fn close_on_exec(fd: i32, on: bool) -> io::Result<()> {
        set_fd_flag(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC, on)
    }

    #[cfg(test)]
    fn scenario_is(name: &str) -> bool {
        env::var("MUTEST_LIFECYCLE_SCENARIO").is_ok_and(|scenario| scenario == name)
    }

    struct Capture {
        pipe: Option<File>,
        bytes: Vec<u8>,
        discarded: usize,
    }

    impl Capture {
        fn new(pipe: Option<OwnedFd>) -> io::Result<Self> {
            if let Some(pipe) = &pipe {
                nonblocking(pipe.as_raw_fd())?;
            }
            Ok(Self {
                pipe: pipe.map(File::from),
                bytes: Vec::new(),
                discarded: 0,
            })
        }

        fn drain(&mut self) -> io::Result<()> {
            let Some(pipe) = &mut self.pipe else {
                return Ok(());
            };
            let mut buffer = [0; 16384];
            let mut drained = 0;
            while drained < DRAIN_QUANTUM {
                match pipe.read(&mut buffer) {
                    Ok(0) => {
                        self.pipe = None;
                        break;
                    }
                    Ok(n) => {
                        let keep = n.min(OUTPUT_LIMIT - self.bytes.len());
                        self.bytes.extend_from_slice(&buffer[..keep]);
                        self.discarded = self.discarded.saturating_add(n - keep);
                        drained += n;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }

        fn finish(mut self) -> Vec<u8> {
            if self.discarded > 0 {
                self.bytes.extend_from_slice(
                    format!("\n[mutest truncated {} output bytes]\n", self.discarded).as_bytes(),
                );
            }
            self.bytes
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
        let (channel, inherited) = UnixStream::pair()?;
        channel.set_nonblocking(true)?;
        let fd = inherited.as_raw_fd();
        command.env(OWNER_FD, fd.to_string());
        match timeout {
            Some(timeout) => command.env(TIMEOUT, timeout.as_nanos().to_string()),
            None => command.env_remove(TIMEOUT),
        };
        // The descriptor stays close-on-exec here, so that owners spawned at the same time do not inherit it.
        // SAFETY: After the fork this only calls `fcntl`, which is async-signal-safe.
        unsafe { command.pre_exec(move || close_on_exec(fd, false)) };
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
    }

    fn cancel_requested(control: Option<&mpsc::Receiver<ControlMsg>>) -> bool {
        control.is_some_and(|control| !matches!(control.try_recv(), Err(mpsc::TryRecvError::Empty)))
    }

    fn owner_failed(status: ExitStatus, stdout: &Capture, stderr: &Capture) -> io::Error {
        let tail = |capture: &Capture| {
            String::from_utf8_lossy(&capture.bytes[capture.bytes.len().saturating_sub(4096)..]).into_owned()
        };
        io::Error::other(format!("isolated owner failed: {status}; stdout: {}; stderr: {}", tail(stdout), tail(stderr)))
    }

    pub(super) fn run(
        command: Command,
        control: Option<mpsc::Receiver<ControlMsg>>,
        timeout: Option<Duration>,
    ) -> io::Result<(TestResult, Duration, Vec<u8>)> {
        let mut owner = spawn_owner(command, timeout)?;
        let child = owner.child.as_mut().unwrap();
        let mut stdout = Capture::new(child.stdout.take().map(OwnedFd::from))?;
        let mut stderr = Capture::new(child.stderr.take().map(OwnedFd::from))?;
        let mut stage = Stage { deadline: Some(Instant::now() + STARTUP_TIMEOUT), ready: false, cancelled: false, exited: false };
        let mut frame = Vec::new();
        loop {
            stdout.drain()?;
            stderr.drain()?;
            read_completion(&mut owner.channel, &mut frame)?;
            if !stage.ready && frame.first() == Some(&READY) {
                stage.start_executing(timeout);
            }
            if !owner.cleanup_reported && frame.len() == FRAME_LEN {
                owner.cleanup_reported = true;
                stage.allow(REPORT_TIMEOUT);
            }
            if !stage.cancelled && !owner.cleanup_reported && !stage.exited && cancel_requested(control.as_ref()) {
                cancel(&mut owner.channel)?;
                stage.cancelled = true;
                stage.allow(CLEANUP_TIMEOUT + REPORT_TIMEOUT);
            }
            if let Some(status) = owner.try_wait()? {
                if !status.success() {
                    return Err(owner_failed(status, &stdout, &stderr));
                }
                stage.exited = true;
                stage.allow(REPORT_TIMEOUT);
            }
            if stage.exited && owner.cleanup_reported && stdout.pipe.is_none() && stderr.pipe.is_none() {
                break;
            }
            if stage.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "isolated owner missed its execution or teardown deadline"));
            }
            thread::sleep(POLL_INTERVAL);
        }
        let (result, elapsed) = decode(&frame, timeout)?;
        let mut output = stdout.finish();
        output.extend(stderr.finish());
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
            .env_remove(OWNER_FD)
            .env_remove(TIMEOUT)
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
            let mut status = 0;
            // SAFETY: `status` is a valid place for `waitpid` to write; reaping any child also reaps adopted orphans.
            let exited = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if exited == child.id() as libc::pid_t {
                return Ok((EXITED, status, start.elapsed()));
            }
            if exited < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error());
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
        let input = (0..OUTPUT_LIMIT + DRAIN_QUANTUM + 37)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let sent = input.clone();
        let sender = thread::spawn(move || writer.write_all(&sent).unwrap());
        let mut capture = Capture::new(Some(reader.into())).unwrap();
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while capture.pipe.is_some() {
            capture.drain().unwrap();
            assert!(
                Instant::now() < deadline,
                "bounded capture did not reach EOF"
            );
            thread::yield_now();
        }
        sender.join().unwrap();
        assert_eq!(capture.bytes, input[..OUTPUT_LIMIT]);
        assert_eq!(capture.discarded, DRAIN_QUANTUM + 37);
        let output = capture.finish();
        assert_eq!(&output[..OUTPUT_LIMIT], &input[..OUTPUT_LIMIT]);
        assert_eq!(
            &output[OUTPUT_LIMIT..],
            format!("\n[mutest truncated {} output bytes]\n", DRAIN_QUANTUM + 37).as_bytes()
        );
    }

    /// Runs as the owner when this process was started as one, and exits; returns otherwise.
    pub(super) fn dispatch() {
        let Some(fd) = env::var_os(OWNER_FD) else {
            return;
        };
        let result = own(fd.to_str().and_then(|fd| fd.parse().ok()).filter(|&fd| fd >= 3));
        if let Err(error) = &result {
            eprintln!("mutation analysis incomplete: isolated owner: {error}");
        }
        process::exit(if result.is_ok() { 0 } else { 101 });
    }

    fn own(fd: Option<i32>) -> io::Result<()> {
        let fd = fd.ok_or_else(|| io::Error::other("invalid owner descriptor"))?;
        // The test this owner starts must not inherit the channel.
        close_on_exec(fd, true)?;
        // SAFETY: The spawning monitor passed this descriptor for the channel alone, and it is claimed once.
        let mut channel = unsafe { UnixStream::from_raw_fd(fd) };
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
