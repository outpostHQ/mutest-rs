//! Runs the mutation analysis in a worker process, and kills what it leaves running.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitStatus};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use mutest_exit_code as exit_code;

use crate::completion::{self, Completion};
use crate::journal::Journal;

#[cfg(target_os = "linux")]
pub(crate) use sys::kill_descendants_until;
#[cfg(target_os = "linux")]
pub use sys::{adopt_orphans, children};

const SUPERVISOR_PID_VAR: &str = "__MUTEST_SUPERVISOR_PID";
const RUN_ELAPSED_VAR: &str = "__MUTEST_RUN_ELAPSED_NANOS";

static RUN_START: OnceLock<Instant> = OnceLock::new();

pub fn run_start() -> Instant {
    *RUN_START.get_or_init(Instant::now)
}

fn run_start_before(now: Instant, elapsed_nanos: &str) -> Option<Instant> {
    now.checked_sub(Duration::from_nanos(elapsed_nanos.parse().ok()?))
}

/// Must be called before any thread starts.
pub fn is_worker() -> bool {
    let Ok(supervisor_pid) = env::var(SUPERVISOR_PID_VAR) else {
        return false;
    };
    let run_elapsed = env::var(RUN_ELAPSED_VAR);
    // SAFETY: No other thread is running yet.
    unsafe {
        env::remove_var(SUPERVISOR_PID_VAR);
        env::remove_var(RUN_ELAPSED_VAR);
    }
    if let Some(run_start) = run_elapsed
        .ok()
        .and_then(|elapsed| run_start_before(Instant::now(), &elapsed))
    {
        let _ = RUN_START.set(run_start);
    }
    after_initialization(sys::die_with(supervisor_pid.parse().ok()), || true)
        .unwrap_or_else(|status| exit_as(status, None))
}

pub fn supervise() -> ! {
    let run_start = run_start();
    sys::forward_stop_signals();

    let exit_code_log = env::var_os(exit_code::LOG_VAR).map(PathBuf::from);
    if let Some(exit_code_log) = &exit_code_log {
        let _ = exit_code::record_start(exit_code_log, process::id());
    }

    after_initialization(sys::adopt_orphans(), || ())
        .unwrap_or_else(|status| exit_as(status, exit_code_log.as_deref()));

    // Without a journal the run goes on, but a crash cannot name the unfinished mutations.
    let journal = Journal::create().ok();

    let status = run_worker(run_start, journal.as_ref());
    let mut status = after_cleanup(status, sys::kill_descendants());
    if let Some(journal) = journal {
        match journal.unfinished() {
            Ok(unfinished) if unfinished.is_empty() => {}
            Ok(unfinished) => {
                eprintln!("mutation analysis incomplete: unfinished mutations {unfinished:?}");
                status = sys::exit_status(exit_code::PANIC);
            }
            Err(error) => {
                eprintln!("mutation analysis incomplete: cannot read journal: {error}");
                status = sys::exit_status(exit_code::PANIC);
            }
        }
        if !status.code().is_some_and(exit_code::analysis_completed) {
            eprintln!("incomplete analysis journal retained at {}", journal.path().display());
            journal.preserve();
        }
    }
    exit_as(status, exit_code_log.as_deref())
}

fn after_initialization<T>(initialized: std::io::Result<()>, proceed: impl FnOnce() -> T) -> Result<T, ExitStatus> {
    initialized.map_err(|error| {
        eprintln!("mutation analysis incomplete: process containment initialization failed: {error}");
        sys::exit_status(exit_code::PANIC)
    })?;
    Ok(proceed())
}

fn after_cleanup(status: ExitStatus, cleanup: std::io::Result<()>) -> ExitStatus {
    match cleanup {
        Ok(()) => status,
        Err(error) => {
            eprintln!("mutation analysis incomplete: descendant cleanup failed: {error}");
            sys::exit_status(exit_code::PANIC)
        }
    }
}

fn run_worker(run_start: Instant, journal: Option<&Journal>) -> ExitStatus {
    let current_exe = env::current_exe().expect("cannot resolve test executable path");
    let mut cmd = Command::new(current_exe);
    cmd.args(env::args_os().skip(1));
    cmd.env(SUPERVISOR_PID_VAR, process::id().to_string());
    cmd.env(RUN_ELAPSED_VAR, run_start.elapsed().as_nanos().to_string());
    cmd.env_remove(exit_code::LOG_VAR);
    if let Some(journal) = journal {
        journal.pass_to(&mut cmd);
    }

    let completion = match Completion::create() {
        Ok(completion) => completion,
        Err(error) => {
            eprintln!("cannot create completion record: {error}");
            return sys::exit_status(exit_code::PANIC);
        }
    };
    completion.pass_to(&mut cmd);
    match sys::start_worker(&mut cmd) {
        Ok(worker) => sys::wait_completed(worker, &completion),
        Err(stopped) => stopped,
    }
}

/// The exit status of a worker's outcome, which says on stderr when the worker left no valid completion.
fn outcome_status(code: i32) -> ExitStatus {
    if code == exit_code::PANIC {
        eprintln!("mutation analysis incomplete: no valid completion or interrupted worker");
    }
    sys::exit_status(code)
}

fn exit_as(status: ExitStatus, exit_code_log: Option<&Path>) -> ! {
    let signal = sys::signal(status);
    let code = match signal {
        Some(signal) => 128 + signal,
        None => status.code().unwrap_or(exit_code::PANIC),
    };
    if let Some(exit_code_log) = exit_code_log {
        let _ = exit_code::record_exit(exit_code_log, process::id(), code);
    }
    if let Some(signal) = signal {
        sys::raise_with_default_action(signal);
    }
    process::exit(code)
}

#[cfg(unix)]
mod sys {
    use std::io;
    use std::mem;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Child, Command, ExitStatus};
    use std::ptr;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{Completion, completion, exit_code};

    use libc::{c_int, pid_t};

    const STOP_SIGNALS: [c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

    static WORKER_PID: AtomicI32 = AtomicI32::new(0);
    static STOP_SIGNAL: AtomicI32 = AtomicI32::new(0);
    /// 1 once a stop signal arrived first, 2 once a completion was witnessed first.
    static COMPLETION_ORDER: AtomicI32 = AtomicI32::new(0);

    /// Sends `signal` to `pid`; a process that has already exited is not an error.
    pub(super) fn send_signal(pid: pid_t, signal: c_int) -> io::Result<()> {
        // SAFETY: `kill` has no memory preconditions, and is async-signal-safe.
        if unsafe { libc::kill(pid, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) }
    }

    /// The id of a child that has exited, left unreaped so that its id is not reused yet; 0 if none has.
    fn exited_child(idtype: libc::idtype_t, id: libc::id_t, flags: c_int) -> io::Result<pid_t> {
        // SAFETY: An all-zero `siginfo_t` is valid; `waitid` fills it in, with a zero id if no child exited.
        unsafe {
            let mut info = mem::zeroed::<libc::siginfo_t>();
            if libc::waitid(idtype, id, &mut info, libc::WEXITED | libc::WNOWAIT | flags) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(info.si_pid())
        }
    }

    /// Reaps the child `pid`, or any child for -1, and returns its id; 0 if `WNOHANG` found none exited.
    fn reap(pid: pid_t, flags: c_int) -> io::Result<pid_t> {
        // SAFETY: A null status pointer is allowed.
        match unsafe { libc::waitpid(pid, ptr::null_mut(), flags) } {
            -1 => Err(io::Error::last_os_error()),
            reaped => Ok(reaped),
        }
    }

    extern "C" fn forward_signal(signal: c_int) {
        let _ = COMPLETION_ORDER.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
        STOP_SIGNAL.store(signal, Ordering::Relaxed);
        // The worker's id is cleared before it is reaped, so this cannot reach a reused id.
        let worker_pid = WORKER_PID.load(Ordering::Relaxed);
        if worker_pid > 0 {
            let _ = send_signal(worker_pid, signal);
        }
    }

    pub(super) fn forward_stop_signals() {
        for signal_number in STOP_SIGNALS {
            // SAFETY: The handler only touches atomics and sends a signal, both async-signal-safe.
            unsafe { libc::signal(signal_number, forward_signal as extern "C" fn(c_int) as libc::sighandler_t) };
        }
    }

    pub(super) fn start_worker(cmd: &mut Command) -> Result<Child, ExitStatus> {
        if let Some(signal) = stop_signal() {
            return Err(ExitStatus::from_raw(signal));
        }

        let worker = cmd.spawn().expect("cannot start the mutation analysis worker");
        WORKER_PID.store(worker.id() as pid_t, Ordering::Relaxed);

        // A stop that came while the worker was starting was not forwarded to it.
        if let Some(signal) = stop_signal() {
            let _ = send_signal(worker.id() as pid_t, signal);
        }
        Ok(worker)
    }

    fn stop_signal() -> Option<c_int> {
        match STOP_SIGNAL.load(Ordering::Relaxed) {
            0 => None,
            signal => Some(signal),
        }
    }

    /// Leaves the child unreaped, so that its id cannot go to another process yet.
    #[cfg(test)]
    fn wait_for_exit(idtype: libc::idtype_t, id: libc::id_t) -> pid_t {
        loop {
            match exited_child(idtype, id, 0) {
                Ok(pid) => return pid,
                Err(error) if error.raw_os_error() == Some(libc::EINTR) => {}
                Err(error) => panic!("cannot wait for the mutation analysis worker: {error}"),
            }
        }
    }

    fn reap_worker(mut worker: Child) -> ExitStatus {
        WORKER_PID.store(0, Ordering::Relaxed);
        worker.wait().expect("cannot wait for the mutation analysis worker")
    }

    pub(super) fn exit_status(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    /// What the supervisor has seen of the worker so far.
    struct Attempt<'a> {
        worker: Child,
        pid: pid_t,
        record: &'a Completion,
        witnessed: Option<i32>,
        cancelled: bool,
        stopping_since: Option<Instant>,
        transport_failed: bool,
    }

    impl Attempt<'_> {
        /// Reads the completion record, and asks the worker to stop if the record is unreadable.
        fn observe(&mut self) {
            if stop_signal().is_some() {
                self.cancelled |= COMPLETION_ORDER.load(Ordering::SeqCst) == 1;
                self.stopping_since.get_or_insert_with(Instant::now);
            }
            let current = self.record.read(self.pid as u32);
            if current.is_err() && !self.transport_failed {
                self.transport_failed = true;
                self.stopping_since.get_or_insert_with(Instant::now);
                let _ = send_signal(self.pid, libc::SIGTERM);
            }
            if stop_signal().is_some() || self.transport_failed {
                return;
            }
            // A completion counts only if no stop signal came before it.
            if let Ok(Some(code)) = current
                && matches!(COMPLETION_ORDER.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst), Ok(0) | Err(2))
            {
                #[cfg(test)]
                if self.witnessed.is_none() {
                    tests::completion_witnessed();
                }
                self.witnessed = Some(code);
            }
        }

        /// Kills a worker that has not exited a second after it was asked to stop.
        fn escalate(&mut self) {
            if self.stopping_since.is_some_and(|since| since.elapsed() >= Duration::from_secs(1)) {
                let _ = self.worker.kill();
            }
        }

        fn finish(self) -> ExitStatus {
            let Self { worker, pid, record, witnessed, cancelled, transport_failed, .. } = self;
            let status = reap_worker(worker);
            let final_record = record.read(pid as u32).ok().flatten();
            let cancelled = cancelled || COMPLETION_ORDER.load(Ordering::SeqCst) == 1;
            let witnessed = witnessed.filter(|_| !cancelled);
            let accepted = witnessed.or(final_record);
            // A stop signal that arrives after a witnessed completion does not undo it.
            let stopped_late = stop_signal().is_some()
                && status.signal().is_some_and(|signal| STOP_SIGNALS.contains(&signal));
            let status_code = if witnessed.is_some() && stopped_late { witnessed } else { status.code() };
            super::outcome_status(completion::outcome(status_code, accepted, cancelled, transport_failed || final_record != accepted))
        }
    }

    pub(super) fn wait_completed(worker: Child, record: &Completion) -> ExitStatus {
        let pid = worker.id() as pid_t;
        let mut attempt = Attempt {
            worker,
            pid,
            record,
            witnessed: None,
            cancelled: false,
            stopping_since: None,
            transport_failed: false,
        };
        loop {
            attempt.observe();
            attempt.escalate();
            // On Linux this process adopts orphans, so it also reaps any other child that exits.
            let exited = if cfg!(target_os = "linux") {
                exited_child(libc::P_ALL, 0, libc::WNOHANG)
            } else {
                exited_child(libc::P_PID, pid as libc::id_t, libc::WNOHANG)
            };
            match exited {
                Ok(exited) if exited == pid => return attempt.finish(),
                Ok(0) => {}
                Ok(orphan) => {
                    let _ = reap(orphan, libc::WNOHANG);
                    continue;
                }
                Err(error) if error.raw_os_error() == Some(libc::EINTR) => {}
                Err(_) => {
                    let _ = attempt.worker.kill();
                    WORKER_PID.store(0, Ordering::Relaxed);
                    return exit_status(exit_code::PANIC);
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub(super) fn signal(status: ExitStatus) -> Option<c_int> {
        status.signal()
    }

    pub(super) fn raise_with_default_action(signal_number: c_int) {
        // SAFETY: Restoring a signal's default action and raising it have no preconditions.
        unsafe {
            libc::signal(signal_number, libc::SIG_DFL);
            libc::raise(signal_number);
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) use linux::*;
    #[cfg(target_os = "linux")]
    pub use linux::{adopt_orphans, children};

    #[cfg(target_os = "linux")]
    mod linux {
        use std::fs;
        use std::os::unix::process::parent_id;
        use std::path::Path;
        use std::process;

        use libc::c_ulong;

        use super::*;

        fn checked_prctl(result: c_int, error: impl FnOnce() -> io::Error) -> io::Result<()> {
            if result == 0 { Ok(()) } else { Err(error()) }
        }

        fn set_process_flag(option: c_int, value: c_ulong) -> io::Result<()> {
            // SAFETY: `prctl` reads the four unsigned longs after the option, all given here.
            checked_prctl(unsafe { libc::prctl(option, value, 0 as c_ulong, 0 as c_ulong, 0 as c_ulong) }, io::Error::last_os_error)
        }

        pub fn adopt_orphans() -> io::Result<()> {
            set_process_flag(libc::PR_SET_CHILD_SUBREAPER, 1)
        }

        pub(crate) fn die_with(supervisor_pid: Option<u32>) -> io::Result<()> {
            set_process_flag(libc::PR_SET_PDEATHSIG, libc::SIGKILL as c_ulong)?;
            // The supervisor may have died before the flag was set.
            if supervisor_pid.is_some_and(|supervisor_pid| parent_id() != supervisor_pid) {
                return Err(io::Error::other("supervisor exited before parent-death setup"));
            }
            Ok(())
        }

        #[cfg(test)]
        #[test]
        fn process_containment_initialization_failure_prevents_worker_start() {
            let mut started = false;
            let denied = checked_prctl(-1, || io::Error::from_raw_os_error(libc::EPERM));
            let status = super::super::after_initialization(denied, || started = true).unwrap_err();
            assert_eq!(status.code(), Some(101));
            assert!(!started, "work started after a failed prctl");
            let allowed = checked_prctl(0, || panic!("successful prctl queried errno"));
            assert_eq!(super::super::after_initialization(allowed, || 42).unwrap(), 42);
        }

        #[cfg(test)]
        pub(crate) fn wait_reaping_orphans(worker: Child) -> ExitStatus {
            let worker_pid = worker.id() as pid_t;
            loop {
                let exited = wait_for_exit(libc::P_ALL, 0);
                if exited == worker_pid {
                    return reap_worker(worker);
                }
                // An interrupted reap is retried, as `waitid` finds the orphan again.
                let _ = reap(exited, 0);
            }
        }

        /// This process's children, adopted orphans included; empty if they cannot be listed.
        pub fn children() -> Vec<pid_t> {
            observed_children().unwrap_or_default()
        }

        fn observed_children() -> io::Result<Vec<pid_t>> {
            // A kernel built without `CONFIG_PROC_CHILDREN` keeps no per-thread lists.
            if Path::new("/proc/thread-self/children").exists() { listed_children() } else { scanned_children() }
        }

        /// The children each thread's `children` file lists; a thread that exits meanwhile lists none.
        fn listed_children() -> io::Result<Vec<pid_t>> {
            let mut children = Vec::new();
            for task in fs::read_dir("/proc/self/task")? {
                let Some(listed) = unless_vanished(fs::read_to_string(task?.path().join("children")))? else {
                    continue;
                };
                for pid in listed.split_whitespace() {
                    children.push(pid.parse::<pid_t>().map_err(|_| io::Error::other("invalid child process id"))?);
                }
            }
            Ok(children)
        }

        fn scanned_children() -> io::Result<Vec<pid_t>> {
            let me = process::id() as pid_t;
            let mut children = Vec::new();
            for entry in fs::read_dir("/proc")? {
                let Some(pid) = entry?.file_name().to_str().and_then(|name| name.parse::<pid_t>().ok()) else {
                    continue;
                };
                let Some(stat) = unless_vanished(fs::read_to_string(format!("/proc/{pid}/stat")))? else {
                    continue;
                };
                let (_, after_comm) = stat.rsplit_once(')').ok_or_else(|| io::Error::other("invalid process stat"))?;
                let ppid = after_comm
                    .split_whitespace()
                    .nth(1)
                    .and_then(|value| value.parse::<pid_t>().ok())
                    .ok_or_else(|| io::Error::other("invalid process parent"))?;
                if ppid == me {
                    children.push(pid);
                }
            }
            Ok(children)
        }

        /// A `/proc` read of a process that has exited meanwhile is `None`, not an error.
        fn unless_vanished(read: io::Result<String>) -> io::Result<Option<String>> {
            match read {
                Ok(text) => Ok(Some(text)),
                Err(error) if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH) => {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }

        #[cfg(test)]
        #[test]
        fn vanished_process_entries_are_not_cleanup_failures() {
            assert_eq!(unless_vanished(Err(io::Error::from_raw_os_error(libc::ENOENT))).unwrap(), None);
            assert_eq!(unless_vanished(Err(io::Error::from_raw_os_error(libc::ESRCH))).unwrap(), None);
            assert_eq!(unless_vanished(Ok("observed".into())).unwrap(), Some("observed".into()));
            assert_eq!(
                unless_vanished(Err(io::Error::from_raw_os_error(libc::EACCES))).unwrap_err().raw_os_error(),
                Some(libc::EACCES)
            );
        }

        #[cfg(test)]
        #[test]
        fn process_observation_tolerates_unrelated_process_churn() {
            // Its own children must not be reaped by another test's wait for any child.
            let _turn = super::tests::STARTING_PROCESSES.lock().unwrap_or_else(|e| e.into_inner());
            let churn = std::thread::spawn(|| {
                for _ in 0..32 {
                    let status = std::process::Command::new("true").status().unwrap();
                    assert!(status.success());
                }
            });
            let observations: Vec<_> = (0..64).map(|_| observed_children()).collect();
            churn.join().unwrap();
            for observation in observations {
                assert!(observation.is_ok(), "{observation:?}");
            }
        }

        #[cfg(test)]
        #[test]
        fn the_kernel_list_and_the_full_scan_find_the_same_child() {
            let _turn = super::tests::STARTING_PROCESSES.lock().unwrap_or_else(|e| e.into_inner());
            let mut child = std::process::Command::new("sleep")
                .arg("60")
                .stdin(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let listed = listed_children();
            let scanned = scanned_children();
            let _ = child.kill();
            let _ = child.wait();
            for found in [listed.unwrap(), scanned.unwrap()] {
                assert!(found.contains(&(child.id() as pid_t)), "{} is not among {found:?}", child.id());
            }
        }

        fn cleanup_until(
            mut step: impl FnMut() -> io::Result<bool>,
            mut expired: impl FnMut() -> bool,
            mut pause: impl FnMut(),
        ) -> io::Result<()> {
            loop {
                if step()? {
                    return Ok(());
                }
                if expired() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "descendants remain after cleanup deadline"));
                }
                pause();
            }
        }

        /// A killed child's children are reparented here, so killing and reaping until none is left reaches the whole tree.
        pub(crate) fn kill_descendants() -> io::Result<()> {
            kill_descendants_until(Instant::now() + Duration::from_secs(10))
        }

        pub(crate) fn kill_descendants_until(deadline: Instant) -> io::Result<()> {
            cleanup_until(
                || kill_and_reap_children(deadline),
                || Instant::now() >= deadline,
                || thread::sleep(Duration::from_millis(5)),
            )
        }

        /// Kills every child and reaps those that have exited; true once no child is left.
        fn kill_and_reap_children(deadline: Instant) -> io::Result<bool> {
            for child in observed_children()? {
                send_signal(child, libc::SIGKILL)?;
            }
            loop {
                match reap(-1, libc::WNOHANG) {
                    Ok(0) => return Ok(false),
                    Ok(_) if Instant::now() >= deadline => return Ok(false),
                    Ok(_) => {}
                    Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(true),
                    Err(error) if error.raw_os_error() == Some(libc::EINTR) => return Ok(false),
                    Err(error) => return Err(error),
                }
            }
        }

        #[cfg(test)]
        #[test]
        fn descendant_cleanup_deadline_and_errors_are_not_success() {
            let timeout = cleanup_until(|| Ok(false), || true, || panic!("expired cleanup slept")).unwrap_err();
            assert_eq!(timeout.kind(), io::ErrorKind::TimedOut);
            let failure = cleanup_until(
                || Err(io::Error::from_raw_os_error(libc::EPERM)),
                || false,
                || panic!("failed cleanup slept"),
            )
            .unwrap_err();
            assert_eq!(failure.raw_os_error(), Some(libc::EPERM));
            assert!(cleanup_until(|| Ok(true), || true, || panic!("completed cleanup slept")).is_ok());
            for code in [0, 2, 3] {
                let deadline_missed = io::Error::new(io::ErrorKind::TimedOut, "synthetic deadline");
                assert_eq!(super::super::after_cleanup(exit_status(code), Err(deadline_missed)).code(), Some(101));
                assert_eq!(super::super::after_cleanup(exit_status(code), Ok(())).code(), Some(code));
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) use elsewhere::*;

    #[cfg(not(target_os = "linux"))]
    mod elsewhere {
        use super::*;

        pub(crate) fn adopt_orphans() -> io::Result<()> {
            Ok(())
        }
        pub(crate) fn die_with(_supervisor_pid: Option<u32>) -> io::Result<()> {
            Ok(())
        }
        #[cfg(test)]
        pub(crate) fn wait_reaping_orphans(worker: Child) -> ExitStatus {
            wait_for_exit(libc::P_PID, worker.id() as libc::id_t);
            reap_worker(worker)
        }
        pub(crate) fn kill_descendants() -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(super) mod tests {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, Stdio};
        use std::sync::Mutex;
        use std::sync::atomic::Ordering;

        use super::*;

        // On Linux, waiting can reap any child, so process-starting tests take turns.
        pub(crate) static STARTING_PROCESSES: Mutex<()> = Mutex::new(());

        /// Lets the regression fixture check what is on disk at the moment a completion is witnessed.
        pub(super) fn completion_witnessed() {
            let Ok(scenario) = std::env::var("MUTEST_REGRESSION_SCENARIO") else {
                return;
            };
            if scenario != "late-stop" && scenario != "output-order" {
                return;
            }
            let root = std::path::PathBuf::from(std::env::var_os("MUTEST_REGRESSION_ROOT").unwrap());
            if scenario == "output-order" {
                let output = std::fs::read(root.join("metadata/evaluation.json")).unwrap();
                let _: serde_json::Value = serde_json::from_slice(&output).unwrap();
                let stream = std::fs::read_to_string(root.join("metadata/evaluation.jsonl")).unwrap();
                assert!(!stream.is_empty(), "requested JSONL output was not finalized");
                for line in stream.lines() {
                    let _: serde_json::Value = serde_json::from_str(line).unwrap();
                }
            }
            std::fs::write(root.join("completion-witnessed"), b"yes").unwrap();
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn a_process_this_one_started_is_among_its_children() {
            let _turn = STARTING_PROCESSES.lock().unwrap_or_else(|e| e.into_inner());

            let mut child = Command::new("sleep").arg("60").stdin(Stdio::null()).spawn().unwrap();
            let children = children();
            let _ = child.kill();
            let _ = child.wait();

            assert!(children.contains(&(child.id() as pid_t)), "{} is not among {children:?}", child.id());
        }

        #[test]
        fn a_stop_requested_between_workers_reaches_no_process_and_starts_no_worker() {
            let _turn = STARTING_PROCESSES.lock().unwrap_or_else(|e| e.into_inner());

            let worker = start_worker(&mut Command::new("true")).unwrap();
            let _ = wait_reaping_orphans(worker);
            assert_eq!(WORKER_PID.load(Ordering::Relaxed), 0, "a stop would be forwarded to the reaped worker's id");

            forward_signal(libc::SIGINT);
            let started = start_worker(Command::new("sleep").arg("60").stdin(Stdio::null()));
            STOP_SIGNAL.store(0, Ordering::Relaxed);
            COMPLETION_ORDER.store(0, Ordering::SeqCst);
            match started {
                Ok(mut worker) => {
                    let _ = worker.kill();
                    let _ = worker.wait();
                    panic!("a worker started after the run was asked to stop");
                }
                Err(stopped) => assert_eq!(stopped.signal(), Some(libc::SIGINT)),
            }
        }
    }
}

#[cfg(not(unix))]
mod sys {
    use std::process::{Child, Command, ExitStatus};

    pub(super) fn adopt_orphans() -> std::io::Result<()> {
        Ok(())
    }
    pub(super) fn die_with(_supervisor_pid: Option<u32>) -> std::io::Result<()> {
        Ok(())
    }
    pub(super) fn forward_stop_signals() {}
    pub(super) fn start_worker(cmd: &mut Command) -> Result<Child, ExitStatus> {
        Ok(cmd.spawn().expect("cannot start the mutation analysis worker"))
    }
    #[cfg(windows)]
    pub(super) fn exit_status(code: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    }
    #[cfg(not(windows))]
    pub(super) fn exit_status(_code: i32) -> ExitStatus {
        panic!("supervision unsupported")
    }
    pub(super) fn wait_completed(mut worker: Child, record: &super::Completion) -> ExitStatus {
        let pid = worker.id();
        let status = worker.wait().expect("cannot wait for mutation worker");
        super::outcome_status(super::completion::outcome(status.code(), record.read(pid).ok().flatten(), false, false))
    }
    pub(super) fn signal(_status: ExitStatus) -> Option<i32> {
        None
    }
    pub(super) fn raise_with_default_action(_signal_number: i32) {}
    pub(super) fn kill_descendants() -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::run_start_before;

    #[test]
    fn a_worker_times_what_it_does_from_when_the_run_started() {
        let now = Instant::now();

        assert_eq!(run_start_before(now, "1500000000"), now.checked_sub(Duration::from_millis(1500)));
        assert_eq!(run_start_before(now, "soon"), None);
    }

    #[cfg(target_os = "linux")]
    mod regressions {
        use std::env;
        use std::fs;
        use std::io;
        use std::os::unix::process::CommandExt;
        use std::path::{Path, PathBuf};
        use std::process::{Child, Command, ExitStatus, Stdio};
        use std::sync::MutexGuard;
        use std::thread;
        use std::time::{Duration, Instant};

        use super::super::{self as supervisor, SUPERVISOR_PID_VAR};
        use crate::{harness, journal};

        const ENTRY: &str = "supervisor::tests::regressions::runtime_fixture_entry";
        const SCENARIO: &str = "MUTEST_REGRESSION_SCENARIO";
        const ROOT: &str = "MUTEST_REGRESSION_ROOT";

        /// A supervisor running one scenario of `runtime_fixture_entry`, in a scratch directory of its own.
        struct Fixture {
            root: PathBuf,
            child: Child,
            _turn: MutexGuard<'static, ()>,
        }

        impl Fixture {
            fn start(scenario: &str) -> Self {
                let turn = supervisor::sys::tests::STARTING_PROCESSES.lock().unwrap_or_else(|error| error.into_inner());
                let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-scratch/runtime-regressions");
                fs::create_dir_all(&base).unwrap();
                let root = (0..1000)
                    .map(|n| base.join(format!("{}-{scenario}-{n}", std::process::id())))
                    .find(|path| match fs::create_dir(path) {
                        Ok(()) => true,
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
                        Err(error) => panic!("cannot create fixture directory: {error}"),
                    })
                    .expect("fixture names exhausted");
                fs::create_dir(root.join("tmp")).unwrap();
                let output = fs::File::create(root.join("output")).unwrap();
                let mut command = Command::new(env::current_exe().unwrap());
                command
                    .args(["--exact", ENTRY, "--nocapture", "--test-threads=1"])
                    // Nothing of the outer run, such as its journal or progress settings, reaches the fixture.
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("RUST_TEST_THREADS", "2")
                    .env("TMPDIR", root.join("tmp"))
                    .env(ROOT, &root)
                    .env(SCENARIO, scenario)
                    .current_dir(&root)
                    .stdin(Stdio::null())
                    .stdout(output.try_clone().unwrap())
                    .stderr(output);
                let parent = std::process::id();
                // SAFETY: After the fork this calls only `prctl`, `getppid` and `_exit`, all async-signal-safe.
                unsafe {
                    command.pre_exec(move || {
                        let zero = 0 as libc::c_ulong;
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, zero, zero, zero) < 0 {
                            return Err(io::Error::last_os_error());
                        }
                        if libc::getppid() as u32 != parent {
                            libc::_exit(125);
                        }
                        Ok(())
                    });
                }
                let child = command.spawn().unwrap();
                Self { root, child, _turn: turn }
            }

            fn admit_worker(&mut self) {
                self.wait_marker("worker-pid");
                fs::write(self.root.join("admit"), b"go").unwrap();
            }

            fn wait_marker(&mut self, name: &str) {
                let deadline = Instant::now() + Duration::from_secs(4);
                while !self.root.join(name).exists() {
                    assert!(self.child.try_wait().unwrap().is_none(), "fixture exited before {name}");
                    assert!(Instant::now() < deadline, "fixture marker deadline: {name}");
                    thread::sleep(Duration::from_millis(5));
                }
            }

            fn wait(&mut self) -> ExitStatus {
                let deadline = Instant::now() + Duration::from_secs(4);
                loop {
                    if let Some(status) = self.child.try_wait().unwrap() {
                        return status;
                    }
                    assert!(Instant::now() < deadline, "fixture did not finish: {}", self.root.display());
                    thread::sleep(Duration::from_millis(5));
                }
            }

            fn stop(&mut self) {
                supervisor::sys::send_signal(self.child.id() as libc::pid_t, libc::SIGTERM).unwrap();
            }

            fn output(&self) -> String {
                fs::read_to_string(self.root.join("output")).unwrap()
            }
        }

        impl Drop for Fixture {
            // The fixture's workers die with it, as each one sets its parent-death signal.
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                if !thread::panicking() {
                    let _ = fs::remove_dir_all(&self.root);
                }
            }
        }

        /// Writes this process's id to `root/name` in one rename, so that a reader never sees it half written.
        fn announce(root: &Path, name: &str) {
            let pending = root.join(format!("{name}.pending"));
            fs::write(&pending, std::process::id().to_string()).unwrap();
            fs::rename(pending, root.join(name)).unwrap();
        }

        /// Exits when told to, or after three seconds; its parent has exited, so the supervisor adopts it.
        fn orphan_entry(root: &Path) -> ! {
            announce(root, "orphan-pid");
            let deadline = Instant::now() + Duration::from_secs(3);
            while !root.join("orphan-exit").exists() {
                if Instant::now() >= deadline {
                    std::process::exit(125);
                }
                thread::sleep(Duration::from_millis(5));
            }
            std::process::exit(0);
        }

        fn orphan_parent_entry() -> ! {
            let _orphan = Command::new(env::current_exe().unwrap())
                .args(["--exact", ENTRY, "--test-threads=1"])
                .env("MUTEST_REGRESSION_ORPHAN", "1")
                .spawn()
                .unwrap();
            std::process::exit(0);
        }

        fn worker_entry(root: &Path, scenario: &str) {
            let parent = env::var(SUPERVISOR_PID_VAR).unwrap().parse::<u32>().unwrap();
            supervisor::sys::die_with(Some(parent)).expect("fixture parent-death setup failed");
            // SAFETY: `setrlimit` only reads the limit it is given; aborting scenarios then leave no core file.
            unsafe { libc::setrlimit(libc::RLIMIT_CORE, &libc::rlimit { rlim_cur: 0, rlim_max: 0 }) };
            if root.join("worker-pid").exists() {
                // A second worker would be a restart, which `restart-hold` checks never happens.
                announce(root, "retry-pid");
                loop {
                    thread::park();
                }
            }
            announce(root, "worker-pid");
            let deadline = Instant::now() + Duration::from_secs(4);
            while !root.join("admit").exists() {
                assert!(Instant::now() < deadline, "fixture admission deadline");
                thread::sleep(Duration::from_millis(5));
            }
            journal::open_fixture_worker();
            if scenario == "restart-hold" {
                journal::worker().unwrap().started(&[99]);
                std::process::abort();
            }
            harness::regression_fixture::run(scenario);
        }

        #[test]
        fn runtime_fixture_entry() {
            crate::test_runner::dispatch_subprocess_owner();
            let Ok(scenario) = env::var(SCENARIO) else {
                return;
            };
            let root = PathBuf::from(env::var_os(ROOT).unwrap());
            if env::var_os("MUTEST_REGRESSION_ORPHAN").is_some() {
                orphan_entry(&root);
            }
            if env::var_os("MUTEST_REGRESSION_ORPHAN_PARENT").is_some() {
                orphan_parent_entry();
            }
            if env::var_os(crate::test_runner::TEST_SUBPROCESS_INVOCATION).is_some() {
                supervisor::sys::die_with(Some(std::os::unix::process::parent_id()))
                    .expect("fixture parent-death setup failed");
                harness::regression_fixture::isolated_entry();
            }
            if env::var_os(SUPERVISOR_PID_VAR).is_none() {
                supervisor::supervise();
            }
            worker_entry(&root, &scenario);
        }

        fn premature(code: i32, mode: &str) {
            let mut fixture = Fixture::start(&format!("exit-{code}-{mode}"));
            fixture.admit_worker();
            let status = fixture.wait();
            assert!(fixture.root.join("reference-entered").exists(), "reference callback never ran");
            assert!(!fixture.root.join("harness-returned").exists(), "early exit returned normally");
            if mode == "journal-loss" {
                assert_eq!(
                    fs::read_to_string(fixture.root.join("journal-write-failed")).unwrap(),
                    "read-only handle rejected append; journal retained"
                );
            }
            assert!(
                !status.code().is_some_and(mutest_exit_code::analysis_completed),
                "premature reference exit reported completed: {status}\n{}",
                fixture.output()
            );
        }

        #[test]
        fn reference_exit_zero_is_incomplete() {
            premature(0, "evaluate");
        }
        #[test]
        fn reference_exit_missed_is_incomplete() {
            premature(2, "evaluate");
        }
        #[test]
        fn reference_exit_timeout_is_incomplete() {
            premature(3, "evaluate");
        }
        #[test]
        fn missing_journal_cannot_authorize_premature_completion() {
            premature(0, "journal-loss");
        }
        #[test]
        fn flakes_reference_exit_zero_is_incomplete() {
            premature(0, "flakes");
        }
        #[test]
        fn flakes_reference_exit_missed_is_incomplete() {
            premature(2, "flakes");
        }
        #[test]
        fn flakes_reference_exit_timeout_is_incomplete() {
            premature(3, "flakes");
        }
        #[test]
        fn simulate_test_exit_zero_is_incomplete() {
            premature(0, "simulate");
        }
        #[test]
        fn simulate_test_exit_missed_is_incomplete() {
            premature(2, "simulate");
        }
        #[test]
        fn simulate_test_exit_timeout_is_incomplete() {
            premature(3, "simulate");
        }

        fn zero(scenario: &str) {
            let mut fixture = Fixture::start(scenario);
            fixture.admit_worker();
            assert_eq!(fixture.wait().code(), Some(0), "{}", fixture.output());
            assert!(fixture.root.join("reference-entered").exists());
            assert!(fixture.root.join("harness-returned").exists(), "legitimate zero never completed");
            assert!(fixture.output().contains("0 total"));
        }

        #[test]
        fn legitimate_zero_mutation_evaluation_completes() {
            zero("zero");
        }
        #[test]
        fn legitimate_zero_mutation_flakes_completes() {
            zero("zero-flakes");
        }

        #[test]
        fn legitimate_simulation_reports_a_surviving_mutation() {
            let mut fixture = Fixture::start("zero-simulate");
            fixture.admit_worker();
            assert_eq!(fixture.wait().code(), Some(mutest_exit_code::MISSED), "{}", fixture.output());
            assert!(fixture.root.join("reference-entered").exists());
            assert!(fixture.output().contains("1 passed; 0 failed; 0 ignored"));
        }

        #[test]
        fn completion_record_write_failure_is_incomplete() {
            let mut fixture = Fixture::start("completion-write-failure");
            fixture.admit_worker();
            assert_eq!(fixture.wait().code(), Some(101), "{}", fixture.output());
            assert!(fixture.root.join("reference-entered").exists());
        }

        #[test]
        fn a_fifo_at_the_record_path_blocks_neither_completion_nor_cancellation() {
            let mut fixture = Fixture::start("completion-fifo");
            fixture.admit_worker();
            fixture.wait_marker("fifo-replaced");
            fixture.stop();
            assert_eq!(fixture.wait().code(), Some(101), "{}", fixture.output());
        }

        #[test]
        fn adopted_orphan_is_reaped_while_worker_still_runs() {
            let mut fixture = Fixture::start("orphan-reaping");
            fixture.admit_worker();
            fixture.wait_marker("orphan-pid");
            fixture.wait_marker("orphan-parent-reaped");
            let pid = fs::read_to_string(fixture.root.join("orphan-pid")).unwrap();
            let stat = format!("/proc/{pid}/stat");
            let parent = |stat: &str| stat.rsplit_once(')').unwrap().1.split_whitespace().nth(1).unwrap().parse::<u32>().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while parent(&fs::read_to_string(&stat).unwrap()) != fixture.child.id() {
                assert!(Instant::now() < deadline, "orphan not adopted by supervisor");
                thread::sleep(Duration::from_millis(5));
            }
            fs::write(fixture.root.join("orphan-exit"), b"go").unwrap();
            while fs::metadata(&stat).is_ok() {
                assert!(Instant::now() < deadline, "exited orphan remained unreaped while worker ran");
                thread::sleep(Duration::from_millis(5));
            }
            assert!(fixture.child.try_wait().unwrap().is_none(), "supervisor already exited");
            fixture.stop();
            assert_eq!(fixture.wait().code(), Some(101));
        }

        #[test]
        fn finished_results_survive_later_output_failure_and_cancellation() {
            for scenario in ["finished-output-failure", "finished-cancel"] {
                let mut fixture = Fixture::start(scenario);
                fixture.admit_worker();
                if scenario == "finished-cancel" {
                    fixture.wait_marker("finished-before-cancel");
                    fixture.stop();
                }
                assert_eq!(fixture.wait().code(), Some(101), "{}", fixture.output());
                let journals = fs::read_dir(fixture.root.join("tmp"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with("mutest-journal-"))
                    .collect::<Vec<_>>();
                let [journal] = journals.as_slice() else {
                    panic!("expected the one retained journal, found {journals:?}: {}", fixture.output());
                };
                let rows = fs::read_to_string(journal)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .collect::<Vec<_>>();
                assert!(
                    rows.iter().any(|row| row.get("finished") == Some(&serde_json::json!(1)) && row["result"] == "undetected"),
                    "{rows:?}"
                );
            }
        }

        #[test]
        fn unsafe_simulation_abort_is_incomplete() {
            let mut fixture = Fixture::start("unsafe-simulate");
            fixture.admit_worker();
            assert_eq!(fixture.wait().code(), Some(101), "{}", fixture.output());
            let child = fs::read_to_string(fixture.root.join("isolated-abort-entered")).unwrap();
            let worker = fs::read_to_string(fixture.root.join("worker-pid")).unwrap();
            assert_ne!(child, worker, "abort did not occur in isolated child");
            assert!(fixture.output().contains("simulation incomplete"));
        }

        #[test]
        fn genuine_evaluation_miss_and_timeout_keep_completed_statuses() {
            for (scenario, code) in [("evaluate-missed", 2), ("evaluate-timeout", 3)] {
                let mut fixture = Fixture::start(scenario);
                fixture.admit_worker();
                assert_eq!(fixture.wait().code(), Some(code), "{}", fixture.output());
                assert!(fixture.root.join("mutation-entered").exists());
                assert!(fixture.output().contains("1 total"));
            }
        }

        #[test]
        fn malformed_completion_record_is_incomplete() {
            for scenario in ["completion-partial", "completion-duplicate", "completion-wrong-worker"] {
                let mut fixture = Fixture::start(scenario);
                fixture.admit_worker();
                assert_eq!(fixture.wait().code(), Some(101), "{scenario}: {}", fixture.output());
                assert!(fixture.root.join("reference-entered").exists());
            }
        }

        #[test]
        fn signal_after_witnessed_completion_does_not_cancel_completed_work() {
            let mut fixture = Fixture::start("late-stop");
            fixture.admit_worker();
            fixture.wait_marker("completion-witnessed");
            fixture.stop();
            assert_eq!(fixture.wait().code(), Some(0), "{}", fixture.output());
            assert!(fixture.root.join("harness-returned").exists());
        }

        #[test]
        fn completion_is_observed_only_after_metadata_is_finalized() {
            let mut fixture = Fixture::start("output-order");
            fixture.admit_worker();
            assert_eq!(fixture.wait().code(), Some(0), "{}", fixture.output());
            assert_eq!(fs::read(fixture.root.join("completion-witnessed")).unwrap(), b"yes");
        }

        fn cancelled(scenario: &str) {
            let mut fixture = Fixture::start(scenario);
            fixture.admit_worker();
            fixture.wait_marker("signal-ready");
            fixture.stop();
            let status = fixture.wait();
            assert!(
                !status.code().is_some_and(mutest_exit_code::analysis_completed),
                "cancelled fixture reported completion: {status}\n{}",
                fixture.output()
            );
        }

        #[test]
        fn cancellation_cannot_be_hidden_by_worker_exit_zero() {
            cancelled("cancel-zero");
        }
        #[test]
        fn cancellation_escalates_when_worker_ignores_signal() {
            cancelled("cancel-ignore");
        }

        #[test]
        fn a_crashed_worker_is_not_restarted() {
            let mut fixture = Fixture::start("restart-hold");
            fixture.admit_worker();
            assert_eq!(fixture.wait().code(), Some(101));
            assert!(!fixture.root.join("retry-pid").exists(), "the crashed worker was restarted");
        }

        #[test]
        fn one_abort_does_not_complete_unfinished_collateral() {
            let mut fixture = Fixture::start("collateral");
            fixture.admit_worker();
            let status = fixture.wait();
            let abort = fs::read_to_string(fixture.root.join("abort-entered")).unwrap();
            let collateral = fs::read_to_string(fixture.root.join("collateral-entered")).unwrap();
            assert_eq!(abort, collateral, "mutations did not run in the same worker");
            let journal = fs::read_to_string(fixture.root.join("journal-before-abort")).unwrap();
            let events = journal
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert!(
                events.iter().any(|event| event.get("started") == Some(&serde_json::json!([1, 2]))),
                "both mutations were not journaled before abort"
            );
            assert!(!events.iter().any(|event| event.get("finished").is_some()), "collateral already finished");
            assert!(
                !status.code().is_some_and(mutest_exit_code::analysis_completed),
                "aborted batch silently completed collateral: {status}\n{}",
                fixture.output()
            );
        }
    }
}
