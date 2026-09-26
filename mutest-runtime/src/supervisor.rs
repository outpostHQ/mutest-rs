//! A harness run is two processes. The one Cargo starts, the supervisor, starts the same binary
//! again as the worker, which runs the mutation analysis, and outlives it. Everything the tests
//! start descends from the supervisor, which on Linux adopts what they orphan, whatever process
//! group it is in, and kills all of it once the worker has exited, however the worker exited.
//!
//! A mutation can take the worker down with it: a stack overflow aborts the process, not just the
//! test. The supervisor then counts the mutations the worker was evaluating as crashed, and starts
//! a new worker, which goes on after them; see `journal`.
//!
//! The supervisor ends as its last worker did, and so it is the one to record the harness's exit
//! code for `cargo mutest`, however the worker ended; see `mutest_exit_code`.

use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitStatus};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use mutest_exit_code as exit_code;

use crate::journal::Journal;

#[cfg(target_os = "linux")]
pub use sys::children;

/// Set in the worker's environment, to the supervisor's process id.
const SUPERVISOR_PID_VAR: &str = "__MUTEST_SUPERVISOR_PID";
/// Set in the worker's environment, to how long the run had gone on when the worker was started, in
/// nanoseconds: a worker that goes on after one that crashed goes on with its times too.
const RUN_ELAPSED_VAR: &str = "__MUTEST_RUN_ELAPSED_NANOS";

static RUN_START: OnceLock<Instant> = OnceLock::new();

/// When the run started: for the worker, when its supervisor did.
pub fn run_start() -> Instant {
    *RUN_START.get_or_init(Instant::now)
}

/// When a run that had gone on for `elapsed_nanos` by `now` started.
fn run_start_before(now: Instant, elapsed_nanos: &str) -> Option<Instant> {
    now.checked_sub(Duration::from_nanos(elapsed_nanos.parse().ok()?))
}

/// Whether this process is the worker. It must be called before any thread is started: it removes
/// the marker, so that a process a test starts from this binary is not taken for a worker too.
pub fn is_worker() -> bool {
    let Ok(supervisor_pid) = env::var(SUPERVISOR_PID_VAR) else { return false; };
    let run_elapsed = env::var(RUN_ELAPSED_VAR);
    // SAFETY: No other thread is running yet.
    unsafe {
        env::remove_var(SUPERVISOR_PID_VAR);
        env::remove_var(RUN_ELAPSED_VAR);
    }
    if let Some(run_start) = run_elapsed.ok().and_then(|elapsed| run_start_before(Instant::now(), &elapsed)) {
        let _ = RUN_START.set(run_start);
    }
    sys::die_with(supervisor_pid.parse().ok());
    true
}

/// Starts this binary again as the worker, with the same arguments, and waits for it; then kills
/// what the worker left running. If the worker went down in the middle of mutations, it counts them
/// as crashed and starts a new worker; otherwise it exits as the worker did.
pub fn supervise() -> ! {
    let run_start = run_start();
    sys::adopt_orphans();
    sys::forward_stop_signals();

    // Should this process end before it records its exit, `cargo mutest` counts it as a panic.
    let exit_code_log = env::var_os(exit_code::LOG_VAR).map(PathBuf::from);
    if let Some(exit_code_log) = &exit_code_log {
        let _ = exit_code::record_start(exit_code_log, process::id());
    }

    // Without a journal the run still happens, but a mutation that crashes the worker ends it.
    let journal = Journal::create().ok();

    loop {
        let status = run_worker(run_start, journal.as_ref());
        sys::kill_descendants();

        let crashed = match &journal {
            Some(journal) if !sys::stopped_on_request(status) => journal.unfinished(),
            _ => vec![],
        };
        if crashed.is_empty() {
            drop(journal);
            exit_as(status, exit_code_log.as_deref());
        }

        // NOTE: How it ended goes to stderr: what a crash is called depends on the platform.
        eprintln!("the test harness {ended}", ended = describe_end(status));
        println!("the test harness crashed while evaluating {mutations} {ids}: counted as crashed; going on without {them}",
            mutations = match crashed.len() { 1 => "mutation", _ => "mutations" },
            ids = crashed.iter().map(u32::to_string).collect::<Vec<_>>().join(", "),
            them = match crashed.len() { 1 => "it", _ => "them" },
        );
        println!();
        let _ = io::stdout().flush();

        if let Err(err) = journal.as_ref().map_or(Ok(()), |journal| journal.record_crashed(&crashed)) {
            println!("cannot record the crash in the mutation journal: {err}");
            drop(journal);
            exit_as(status, exit_code_log.as_deref());
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
    if let Some(journal) = journal { journal.pass_to(&mut cmd); }

    match sys::start_worker(&mut cmd) {
        Ok(worker) => sys::wait_reaping_orphans(worker),
        Err(stopped) => stopped,
    }
}

/// Not `ExitStatus`'s own words, which also say whether a core was dumped.
fn describe_end(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("was killed by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("exited with code {code}"),
        None => "ended".to_owned(),
    }
}

/// Ends this process as the worker ended, once its exit code is recorded for `cargo mutest`.
fn exit_as(status: ExitStatus, exit_code_log: Option<&Path>) -> ! {
    let code = exit_code_of(status);
    if let Some(exit_code_log) = exit_code_log {
        let _ = exit_code::record_exit(exit_code_log, process::id(), code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            sys::raise_with_default_action(signal);
        }
    }
    process::exit(code)
}

/// As a shell reports it: a process killed by a signal ends with 128 plus the signal's number.
fn exit_code_of(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(exit_code::PANIC)
}

#[cfg(unix)]
mod sys {
    use std::io;
    use std::mem;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Child, Command, ExitStatus};
    use std::sync::atomic::{AtomicI32, Ordering};

    use libc::{c_int, pid_t};

    const STOP_SIGNALS: [c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

    /// The running worker, which a stop signal is forwarded to; 0 while there is none.
    static WORKER_PID: AtomicI32 = AtomicI32::new(0);
    /// The stop signal received last; 0 until one is.
    static STOP_SIGNAL: AtomicI32 = AtomicI32::new(0);

    extern "C" fn forward_signal(signal: c_int) {
        STOP_SIGNAL.store(signal, Ordering::Relaxed);
        let worker_pid = WORKER_PID.load(Ordering::Relaxed);
        // SAFETY: `kill` is async-signal-safe. The pid is that of a worker that has not been reaped,
        //         which is the only way its id could go to another process: it is stored once the
        //         worker has started, and cleared before the worker is reaped.
        if worker_pid > 0 { unsafe { libc::kill(worker_pid, signal) }; }
    }

    /// A signal meant to stop the run stops the worker, and the supervisor goes on to clean up after
    /// it; the supervisor itself stopping first would leave the worker running unsupervised.
    pub(super) fn forward_stop_signals() {
        for signal_number in STOP_SIGNALS {
            // SAFETY: The handler only touches atomics and calls `kill`, which is async-signal-safe.
            unsafe { libc::signal(signal_number, forward_signal as extern "C" fn(c_int) as libc::sighandler_t) };
        }
    }

    /// Starts a worker, unless the run has been asked to stop: then it ends as the stop signal would
    /// have ended the worker.
    pub(super) fn start_worker(cmd: &mut Command) -> Result<Child, ExitStatus> {
        if let Some(signal) = stop_signal() { return Err(ExitStatus::from_raw(signal)); }

        let worker = cmd.spawn().expect("cannot start the mutation analysis worker");
        WORKER_PID.store(worker.id() as pid_t, Ordering::Relaxed);

        // A stop that came while the worker was starting found no worker to forward it to.
        if let Some(signal) = stop_signal() {
            // SAFETY: The worker has not been reaped, so its id is still its own.
            unsafe { libc::kill(worker.id() as pid_t, signal) };
        }
        Ok(worker)
    }

    fn stop_signal() -> Option<c_int> {
        match STOP_SIGNAL.load(Ordering::Relaxed) {
            0 => None,
            signal => Some(signal),
        }
    }

    /// Waits until the child `waitid` selects has exited, and says which it was, without reaping
    /// it: until it is reaped, its id cannot go to another process.
    fn wait_for_exit(idtype: libc::idtype_t, id: libc::id_t) -> pid_t {
        loop {
            // SAFETY: `siginfo_t` is plain data, for which all zeroes is a valid value.
            let mut info = unsafe { mem::zeroed::<libc::siginfo_t>() };
            // SAFETY: `info` is a valid place for `waitid` to write to.
            if unsafe { libc::waitid(idtype, id, &mut info, libc::WEXITED | libc::WNOWAIT) } == 0 {
                // SAFETY: `waitid` succeeded, so `info` describes the child that exited.
                return unsafe { info.si_pid() };
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                panic!("cannot wait for the mutation analysis worker: {error}");
            }
        }
    }

    /// Reaps the worker once it has exited. Its id is no longer where a stop signal goes by then,
    /// as reaping it frees the id for another process.
    fn reap_worker(mut worker: Child) -> ExitStatus {
        WORKER_PID.store(0, Ordering::Relaxed);
        worker.wait().expect("cannot wait for the mutation analysis worker")
    }

    /// Whether the worker was stopped by someone, rather than by what it ran.
    pub(super) fn stopped_on_request(status: ExitStatus) -> bool {
        stop_signal().is_some() || status.signal().is_some_and(|signal| STOP_SIGNALS.contains(&signal))
    }

    pub(super) fn raise_with_default_action(signal_number: i32) {
        // SAFETY: Restoring the default action, and raising the signal the worker died of.
        unsafe {
            libc::signal(signal_number, libc::SIG_DFL);
            libc::raise(signal_number);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) use linux::*;
    #[cfg(target_os = "linux")]
    pub use linux::children;

    #[cfg(target_os = "linux")]
    mod linux {
        use std::fs;
        use std::process;
        use std::ptr;
        use std::thread;
        use std::time::{Duration, Instant};

        use libc::c_ulong;

        use super::*;

        /// What the tests orphan is reparented to the supervisor, rather than to init.
        pub(crate) fn adopt_orphans() {
            // SAFETY: `prctl` with `PR_SET_CHILD_SUBREAPER` only sets a flag on this process. It reads
            //         four more arguments as `unsigned long`, whichever it uses, so four are passed.
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1 as c_ulong, 0 as c_ulong, 0 as c_ulong, 0 as c_ulong) };
        }

        /// The worker is killed with its supervisor, should the supervisor itself be killed outright.
        pub(crate) fn die_with(supervisor_pid: Option<pid_t>) {
            // SAFETY: `prctl` with `PR_SET_PDEATHSIG` only sets a flag on this process. It reads four
            //         more arguments as `unsigned long`, whichever it uses, so four are passed.
            unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as c_ulong, 0 as c_ulong, 0 as c_ulong, 0 as c_ulong) };
            // The supervisor may have died before the flag was set.
            // SAFETY: `getppid` cannot fail.
            if supervisor_pid.is_some_and(|supervisor_pid| unsafe { libc::getppid() } != supervisor_pid) {
                process::exit(101);
            }
        }

        /// Waits for the worker, reaping the orphans that exit meanwhile, so that none of them lingers
        /// as a zombie holding a process slot until the end of a long run.
        pub(crate) fn wait_reaping_orphans(worker: Child) -> ExitStatus {
            let worker_pid = worker.id() as pid_t;
            loop {
                let exited = wait_for_exit(libc::P_ALL, 0);
                if exited == worker_pid { return reap_worker(worker); }
                // An orphan. Should reaping it be interrupted, `waitid` finds it again.
                // SAFETY: A null status is allowed.
                unsafe { libc::waitpid(exited, ptr::null_mut(), 0) };
            }
        }

        /// Every process whose parent is this one, adopted or not. The UI tests look through them
        /// too, for what a harness left running.
        pub fn children() -> Vec<pid_t> {
            let Ok(entries) = fs::read_dir("/proc") else { return vec![]; };
            let me = process::id() as pid_t;
            entries
                .filter_map(|entry| {
                    let pid = entry.ok()?.file_name().to_str()?.parse::<pid_t>().ok()?;
                    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                    // `pid (comm) state ppid ...`, where `comm` may itself hold spaces and parentheses.
                    let (_, after_comm) = stat.rsplit_once(')')?;
                    let ppid = after_comm.split_whitespace().nth(1)?.parse::<pid_t>().ok()?;
                    (ppid == me).then_some(pid)
                })
                .collect()
        }

        /// Kills every descendant. A killed process's children are reparented to this one before it
        /// can be reaped, so killing the children, reaping them, and repeating reaches the whole tree;
        /// it is done when `waitpid` finds no child at all.
        pub(crate) fn kill_descendants() {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                for child in children() {
                    // SAFETY: `child` is a child of this process that has not been reaped, so its id
                    //         cannot have gone to another process.
                    unsafe { libc::kill(child, libc::SIGKILL) };
                }
                loop {
                    // SAFETY: A null status is allowed.
                    match unsafe { libc::waitpid(-1, ptr::null_mut(), libc::WNOHANG) } {
                        // Children remain, and none has exited yet.
                        0 => break,
                        -1 if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) => continue,
                        // No child remains.
                        -1 => return,
                        _reaped => continue,
                    }
                }
                // A process stuck in the kernel dies when it leaves it; do not wait for it forever.
                if Instant::now() >= deadline { return; }
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) use elsewhere::*;

    /// Without a child subreaper, what the tests orphan goes to init, out of reach.
    #[cfg(not(target_os = "linux"))]
    mod elsewhere {
        use super::*;

        pub(crate) fn adopt_orphans() {}
        pub(crate) fn die_with(_supervisor_pid: Option<pid_t>) {}
        pub(crate) fn wait_reaping_orphans(worker: Child) -> ExitStatus {
            wait_for_exit(libc::P_PID, worker.id() as libc::id_t);
            reap_worker(worker)
        }
        pub(crate) fn kill_descendants() {}
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, Stdio};
        use std::sync::Mutex;
        use std::sync::atomic::Ordering;

        use super::*;

        /// On Linux, waiting for the worker reaps any child of this process that exits meanwhile,
        /// so the tests that start processes take turns.
        static STARTING_PROCESSES: Mutex<()> = Mutex::new(());

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

            // As the handler runs for a Ctrl+C that comes before the next worker has started.
            forward_signal(libc::SIGINT);
            let started = start_worker(Command::new("sleep").arg("60").stdin(Stdio::null()));
            STOP_SIGNAL.store(0, Ordering::Relaxed);
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

    pub(super) fn adopt_orphans() {}
    pub(super) fn die_with(_supervisor_pid: Option<i32>) {}
    pub(super) fn forward_stop_signals() {}
    pub(super) fn start_worker(cmd: &mut Command) -> Result<Child, ExitStatus> {
        Ok(cmd.spawn().expect("cannot start the mutation analysis worker"))
    }
    pub(super) fn stopped_on_request(_status: ExitStatus) -> bool { false }
    pub(super) fn wait_reaping_orphans(mut worker: Child) -> ExitStatus {
        worker.wait().expect("cannot wait for the mutation analysis worker")
    }
    pub(super) fn kill_descendants() {}
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
}
