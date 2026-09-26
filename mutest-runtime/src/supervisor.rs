//! A harness run is two processes. The one Cargo starts, the supervisor, starts the same binary
//! again as the worker, which runs the mutation analysis, and outlives it. Everything the tests
//! start descends from the supervisor, which on Linux adopts what they orphan, whatever process
//! group it is in, and kills all of it once the worker has exited, however the worker exited.
//!
//! A mutation can take the worker down with it: a stack overflow aborts the process, not just the
//! test. The supervisor then counts the mutations the worker was evaluating as crashed, and starts
//! a new worker, which goes on after them; see `journal`.

use std::env;
use std::io::{self, Write};
use std::process::{self, Command, ExitStatus};

use crate::journal::Journal;

#[cfg(target_os = "linux")]
pub use sys::children;

/// Set in the worker's environment, to the supervisor's process id.
const SUPERVISOR_PID_VAR: &str = "__MUTEST_SUPERVISOR_PID";

/// Whether this process is the worker. It must be called before any thread is started: it removes
/// the marker, so that a process a test starts from this binary is not taken for a worker too.
pub fn is_worker() -> bool {
    let Ok(supervisor_pid) = env::var(SUPERVISOR_PID_VAR) else { return false; };
    // SAFETY: No other thread is running yet.
    unsafe { env::remove_var(SUPERVISOR_PID_VAR) };
    sys::die_with(supervisor_pid.parse().ok());
    true
}

/// Starts this binary again as the worker, with the same arguments, and waits for it; then kills
/// what the worker left running. If the worker went down in the middle of mutations, it counts them
/// as crashed and starts a new worker; otherwise it exits as the worker did.
pub fn supervise() -> ! {
    sys::adopt_orphans();

    // Without a journal the run still happens, but a mutation that crashes the worker ends it.
    let journal = Journal::create().ok();

    loop {
        let status = run_worker(journal.as_ref());
        sys::kill_descendants();

        let crashed = match &journal {
            Some(journal) if !sys::stopped_on_request(status) => journal.unfinished(),
            _ => vec![],
        };
        if crashed.is_empty() {
            drop(journal);
            exit_as(status);
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
            exit_as(status);
        }
    }
}

fn run_worker(journal: Option<&Journal>) -> ExitStatus {
    let current_exe = env::current_exe().expect("cannot resolve test executable path");
    let mut cmd = Command::new(current_exe);
    cmd.args(env::args_os().skip(1));
    cmd.env(SUPERVISOR_PID_VAR, process::id().to_string());
    if let Some(journal) = journal { journal.pass_to(&mut cmd); }
    let worker = cmd.spawn().expect("cannot start the mutation analysis worker");

    sys::forward_signals_to(worker.id());
    sys::wait_reaping_orphans(worker)
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

fn exit_as(status: ExitStatus) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            sys::raise_with_default_action(signal);
            process::exit(128 + signal);
        }
    }
    process::exit(status.code().unwrap_or(101))
}

#[cfg(unix)]
mod sys {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Child, ExitStatus};
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    use libc::{c_int, pid_t};

    const STOP_SIGNALS: [c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

    static WORKER_PID: AtomicI32 = AtomicI32::new(0);
    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

    extern "C" fn forward_signal(signal: c_int) {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
        let worker_pid = WORKER_PID.load(Ordering::Relaxed);
        // SAFETY: `kill` is async-signal-safe, and the pid is a child of this process that has not
        //         been reaped, which is the only way its id could go to another process.
        if worker_pid > 0 { unsafe { libc::kill(worker_pid, signal) }; }
    }

    /// A signal meant to stop the run stops the worker, and the supervisor goes on to clean up after
    /// it; the supervisor itself stopping first would leave the worker running unsupervised.
    pub(super) fn forward_signals_to(worker_pid: u32) {
        WORKER_PID.store(worker_pid as pid_t, Ordering::Relaxed);
        for signal_number in STOP_SIGNALS {
            // SAFETY: The handler only touches atomics and calls `kill`, which is async-signal-safe.
            unsafe { libc::signal(signal_number, forward_signal as extern "C" fn(c_int) as libc::sighandler_t) };
        }
    }

    /// Whether the worker was stopped by someone, rather than by what it ran.
    pub(super) fn stopped_on_request(status: ExitStatus) -> bool {
        STOP_REQUESTED.load(Ordering::Relaxed) || status.signal().is_some_and(|signal| STOP_SIGNALS.contains(&signal))
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
        use std::io;
        use std::os::unix::process::ExitStatusExt;
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
                let mut status: c_int = 0;
                // SAFETY: `status` is a valid place for `waitpid` to write to.
                let reaped = unsafe { libc::waitpid(-1, &mut status, 0) };
                if reaped == worker_pid { return ExitStatus::from_raw(status); }
                if reaped == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    panic!("cannot wait for the mutation analysis worker: {}", io::Error::last_os_error());
                }
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
        pub(crate) fn wait_reaping_orphans(mut worker: Child) -> ExitStatus {
            worker.wait().expect("cannot wait for the mutation analysis worker")
        }
        pub(crate) fn kill_descendants() {}
    }
}

#[cfg(not(unix))]
mod sys {
    use std::process::{Child, ExitStatus};

    pub(super) fn adopt_orphans() {}
    pub(super) fn die_with(_supervisor_pid: Option<i32>) {}
    pub(super) fn forward_signals_to(_worker_pid: u32) {}
    pub(super) fn stopped_on_request(_status: ExitStatus) -> bool { false }
    pub(super) fn wait_reaping_orphans(mut worker: Child) -> ExitStatus {
        worker.wait().expect("cannot wait for the mutation analysis worker")
    }
    pub(super) fn kill_descendants() {}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::process::{Command, Stdio};

    #[test]
    fn a_process_this_one_started_is_among_its_children() {
        let mut child = Command::new("sleep").arg("60").stdin(Stdio::null()).spawn().unwrap();
        let children = super::children();
        let _ = child.kill();
        let _ = child.wait();

        assert!(children.contains(&(child.id() as libc::pid_t)), "{} is not among {children:?}", child.id());
    }
}
