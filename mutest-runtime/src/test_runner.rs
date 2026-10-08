use std::any::Any;
use std::cell::Cell;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::panic;
use std::process::{self, Command};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{self, AtomicBool, AtomicUsize};
use std::sync::mpsc;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use crate::metadata::{SubstLocIdx, SubstMap, SubstMeta};
use crate::thread_pool::{self, ThreadPool};

#[path = "subprocess.rs"]
mod subprocess;
#[path = "progress.rs"]
pub(crate) mod progress;
#[path = "mutation_child.rs"]
mod mutation_child;
pub(crate) use subprocess::{TEST_RUNNER_PID_VAR, dispatch as dispatch_subprocess_owner};
pub(crate) use mutation_child::{TESTS_OF_CHILD_VAR, run_tests_in_spawned_subprocess, tests_of_child};
#[cfg(windows)]
pub(crate) use subprocess::job::Job;

enum MonitorMessage {
    Completed(CompletedTest),
    Incomplete { id: test::TestId, message: String },
}

mod test {
    #![allow(unused_imports, reason = "a glob shim over the unstable `test` crate, of which this crate uses only a part")]

    pub use ::test::*;
    pub use ::test::test::*;
}

#[derive(Clone)]
pub enum TestRunStrategy {
    InProcess(Option<ThreadPool>),
    InIsolatedChildProcess(Arc<dyn Fn(&mut process::Command) + Send + Sync>),
}

impl fmt::Debug for TestRunStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InProcess(thread_pool) => {
                f.debug_tuple("InProcess")
                    .field(thread_pool).finish()
            }
            Self::InIsolatedChildProcess(_) => {
                f.debug_tuple("InIsolatedChildProcess")
                    .field(&format_args!("_")).finish()
            }
        }
    }
}

#[derive(Debug)]
pub enum ThreadHandle {
    StandaloneThread(thread::JoinHandle<()>),
    ThreadPoolThread(thread_pool::JobHandle),
}

impl ThreadHandle {
    pub fn thread_id(&self) -> ThreadId {
        match self {
            Self::StandaloneThread(join_handle) => join_handle.thread().id(),
            Self::ThreadPoolThread(job_handle) => job_handle.thread_id(),
        }
    }

    pub fn join(self) -> Result<(), Box<dyn Any + Send + 'static>> {
        match self {
            Self::StandaloneThread(join_handle) => join_handle.join(),
            Self::ThreadPoolThread(job_handle) => job_handle.join(),
        }
    }

    pub fn is_finished(&self) -> bool {
        match self {
            Self::StandaloneThread(join_handle) => join_handle.is_finished(),
            Self::ThreadPoolThread(job_handle) => job_handle.is_finished(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestResult {
    Ok,
    Ignored,
    Failed,
    FailedMsg(String),
    CrashedMsg(String),
    TimedOut,
}

const TR_OK: i32 = 50;
const TR_FAILED: i32 = 51;

impl TestResult {
    pub fn from_task<'a>(
        test_should_panic: test::ShouldPanic,
        task_result: Result<(), &'a (dyn Any + Send + 'static)>,
        test_timeout: Option<Duration>,
        task_exec_time: Option<Duration>,
    ) -> Self {
        let result = match (test_should_panic, task_result) {
            | (test::ShouldPanic::No, Ok(_))
            | (test::ShouldPanic::Yes, Err(_)) => TestResult::Ok,

            | (test::ShouldPanic::Yes, Ok(_))
            | (test::ShouldPanic::YesWithMessage(_), Ok(_)) => {
                TestResult::FailedMsg("test did not panic as expected".to_string())
            }

            (test::ShouldPanic::YesWithMessage(msg), Err(ref err)) => {
                let maybe_panic_str = err
                    .downcast_ref::<String>()
                    .map(|e| &**e)
                    .or_else(|| err.downcast_ref::<&'static str>().copied());

                match maybe_panic_str {
                    Some(panic_str) if panic_str.contains(msg) => TestResult::Ok,
                    Some(panic_str) => {
                        TestResult::FailedMsg(format!(
                            r#"panic did not contain expected string
      panic message: `{panic_str:?}`,
 expected substring: `{msg:?}`"#
                        ))
                    }
                    _ => {
                        let panic_ty = (**err).type_id();
                        TestResult::FailedMsg(format!(
                            r#"expected panic with string value
 found non-string value: `{panic_ty:?}`
     expected substring: `{msg:?}`"#
                        ))
                    }
                }
            }

            _ => TestResult::Failed,
        };

        if result != TestResult::Ok { return result; }

        if let (Some(test_timeout), Some(task_time)) = (test_timeout, task_exec_time) {
            if task_time > test_timeout {
                return TestResult::TimedOut;
            }
        }

        result
    }

    pub fn from_exit_status(
        exit_status: process::ExitStatus,
        test_timeout: Option<Duration>,
        task_exec_time: Option<Duration>,
    ) -> Self {
        #[cfg(not(unix))]
        let exit_code = exit_status.code().expect("received no exit code");
        #[cfg(unix)]
        let Some(exit_code) = exit_status.code() else {
            use std::os::unix::process::ExitStatusExt;
            match exit_status.signal() {
                Some(signal) => return TestResult::CrashedMsg(format!("received signal {signal}")),
                None => return TestResult::CrashedMsg("received unknown signal".to_owned()),
            }
        };

        let result = match exit_code {
            TR_OK => TestResult::Ok,
            TR_FAILED => TestResult::Failed,
            _ => TestResult::CrashedMsg(format!("got unexpected exit code {exit_code}")),
        };

        if result != TestResult::Ok { return result; }

        if let (Some(test_timeout), Some(task_time)) = (test_timeout, task_exec_time) {
            if task_time > test_timeout {
                return TestResult::TimedOut;
            }
        }

        result
    }
}

#[derive(Debug)]
pub struct Test {
    pub desc: test::TestDesc,
    pub test_fn: test::TestFn,
    pub timeout: Option<Duration>,
    /// The test fails when it runs again in one process, so the harness runs it in a child process for each mutation.
    pub unrepeatable: bool,
}

#[derive(Debug)]
pub struct CompletedTest {
    pub id: test::TestId,
    pub desc: test::TestDesc,
    pub result: TestResult,
    pub exec_time: Option<Duration>,
    pub stdout: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlMsg {
    KillChildProcess,
}

thread_local! {
    /// The signal of the in-process test this thread runs, or null when it runs none.
    static TEST_THREAD_ACTIVE: Cell<*const AtomicBool> = const { Cell::new(ptr::null()) };
}

/// The in-process tests the run gave up whose threads still run; while there are none, no thread reads its signal.
static ABANDONED_TEST_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Gives this thread the signal of its in-process test, until dropped.
struct TestThreadSignal(Arc<AtomicBool>);

impl TestThreadSignal {
    fn set(active_signal: Arc<AtomicBool>) -> Self {
        TEST_THREAD_ACTIVE.set(Arc::as_ptr(&active_signal));
        Self(active_signal)
    }
}

impl Drop for TestThreadSignal {
    fn drop(&mut self) {
        TEST_THREAD_ACTIVE.set(ptr::null());
        if !self.0.swap(false, atomic::Ordering::SeqCst) {
            ABANDONED_TEST_THREADS.fetch_sub(1, atomic::Ordering::SeqCst);
        }
    }
}

/// Clears the signal of a test that has not ended yet, and counts its thread as abandoned until the test ends.
fn abandon_signal(active_signal: &AtomicBool) {
    if active_signal.swap(false, atomic::Ordering::SeqCst) {
        ABANDONED_TEST_THREADS.fetch_add(1, atomic::Ordering::SeqCst);
    }
}

/// Whether the run gave up the test of this thread, after its timeout or after the run stopped early.
/// Callers check `any_test_thread_abandoned` first, as only then can it be true.
fn test_thread_abandoned() -> bool {
    // SAFETY: The `TestThreadSignal` of this thread owns the signal, and clears the pointer before it drops it.
    unsafe { TEST_THREAD_ACTIVE.get().as_ref() }.is_some_and(|active_signal| !active_signal.load(atomic::Ordering::SeqCst))
}

/// Whether the run gave up a test whose thread still runs.
/// Inlined even in unoptimized builds, as every substitution point calls it.
#[inline(always)]
fn any_test_thread_abandoned() -> bool {
    ABANDONED_TEST_THREADS.load(atomic::Ordering::Relaxed) != 0
}

/// The substitution of the active mutant at the location, or none for a test the run gave up, which an unwinding
/// abandoned test reaches past `is_test_thread_active`.
#[inline(always)]
pub(crate) fn active_subst<S: SubstMap>(subst_map: &Option<S>, subst_loc_idx: SubstLocIdx) -> Option<SubstMeta> {
    let subst = match subst_map { Some(subst_map) => subst_map.subst_at(subst_loc_idx), None => None };
    match subst {
        Some(_) if any_test_thread_abandoned() && test_thread_abandoned() => None,
        subst => subst,
    }
}

/// Whether this thread may go on past a substitution point, where an abandoned test is cancelled by a panic.
#[inline(always)]
pub fn is_test_thread_active() -> bool {
    !any_test_thread_abandoned() || test_thread_goes_on()
}

/// Whether the test of this thread goes on: the run did not give it up, or it unwinds already,
/// as a second panic would abort the process.
fn test_thread_goes_on() -> bool {
    !test_thread_abandoned() || thread::panicking()
}

fn run_test_in_process(
    id: test::TestId,
    desc: test::TestDesc,
    test_fn: Box<dyn FnOnce() -> Result<(), String> + Send>,
    monitor_ch: mpsc::Sender<MonitorMessage>,
    test_timeout: Option<Duration>,
    active_signal: Option<Arc<AtomicBool>>,
    no_capture: bool,
) {
    let io_buffer = Arc::new(Mutex::new(Vec::new()));

    if !no_capture {
        io::set_output_capture(Some(io_buffer.clone()));
    }

    let _active_signal = active_signal.map(TestThreadSignal::set);

    fn fold_err<T, E>(result: Result<Result<T, E>, Box<dyn Any + Send>>) -> Result<T, Box<dyn Any + Send>>
    where
        E: Send + 'static,
    {
        match result {
            Ok(Err(e)) => Err(Box::new(e)),
            Ok(Ok(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    let start = Instant::now();
    let result = fold_err(panic::catch_unwind(panic::AssertUnwindSafe(test_fn)));
    let exec_time = start.elapsed();

    io::set_output_capture(None);

    let test_result = match result {
        Ok(_) => TestResult::from_task(desc.should_panic, Ok(()), test_timeout, Some(exec_time)),
        Err(e) => TestResult::from_task(desc.should_panic, Err(e.as_ref()), test_timeout, Some(exec_time)),
    };

    let stdout = io_buffer.lock().unwrap_or_else(|e| e.into_inner()).to_vec();
    let completed_test = CompletedTest { id, desc, result: test_result, exec_time: Some(exec_time), stdout };

    match monitor_ch.send(MonitorMessage::Completed(completed_test)) {
        Ok(()) => {}
        // Send errors will only occur if the test execution outlives the test run, closing the receiver early. This
        // happens if the test runner was stopped early, leaving already running tests lingering until completion. This
        // behavior is considered intended and so send errors are explicitly ignored.
        Err(mpsc::SendError(_)) => {}
    };
}

pub static TEST_SUBPROCESS_INVOCATION: &str = "__ISOLATED_TEST_CASE";

/// The command that runs the test `name` in a child process, which runs the tests in `list` in turn, if given.
fn isolated_command(name: &test::TestName, cmd_hook: &(dyn Fn(&mut Command) + Send + Sync), list: Option<&std::path::Path>, no_capture: bool) -> io::Result<Command> {
    let mut command = Command::new(env::current_exe()?);
    let name_tests = |command: &mut Command| {
        command.env(TEST_SUBPROCESS_INVOCATION, name.as_slice());
        if let Some(list) = list { command.env(TESTS_OF_CHILD_VAR, list); }
    };
    name_tests(&mut command);
    cmd_hook(&mut command);
    // Set again, in case the hook cleared the environment.
    name_tests(&mut command);
    if no_capture {
        command.stdout(process::Stdio::inherit()).stderr(process::Stdio::inherit());
    } else {
        command.stdout(process::Stdio::piped()).stderr(process::Stdio::piped());
    }
    Ok(command)
}

fn spawn_test_subprocess(
    id: test::TestId,
    desc: test::TestDesc,
    cmd_hook: Arc<dyn Fn(&mut process::Command) + Send + Sync>,
    control_ch: Option<&mpsc::Receiver<ControlMsg>>,
    monitor_ch: mpsc::Sender<MonitorMessage>,
    test_timeout: Option<Duration>,
    no_capture: bool,
) {
    let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        subprocess::run(isolated_command(&desc.name, &*cmd_hook, None, no_capture)?, control_ch, test_timeout)
    }));
    let message = match outcome {
        Ok(Ok((result, exec_time, stdout))) => MonitorMessage::Completed(CompletedTest { id, desc, result, exec_time: Some(exec_time), stdout }),
        Ok(Err(error)) => MonitorMessage::Incomplete { id, message: error.to_string() },
        Err(_) => MonitorMessage::Incomplete { id, message: "isolated test monitor panicked".to_owned() },
    };
    let _ = monitor_ch.send(message);
}

/// Fixed frame used to clean the backtrace with `RUST_BACKTRACE=1`.
#[inline(never)]
fn __rust_begin_short_backtrace<T, F: FnOnce() -> T>(f: F) -> T {
    let result = f();

    // Prevent this frame from being tail-call optimized away.
    test::black_box(result)
}

fn run_test(
    id: test::TestId,
    test: Test,
    control_ch: Option<mpsc::Receiver<ControlMsg>>,
    monitor_ch: mpsc::Sender<MonitorMessage>,
    test_run_strategy: TestRunStrategy,
    active_signal: Option<Arc<AtomicBool>>,
    no_capture: bool,
) -> Option<ThreadHandle> {
    progress::start(id, &test, matches!(&test_run_strategy, TestRunStrategy::InIsolatedChildProcess(_)));
    let Test { desc, test_fn, timeout, .. } = test;

    let ignore_because_no_process_support = match desc.should_panic {
        test::ShouldPanic::Yes | test::ShouldPanic::YesWithMessage(_) => {
            // Emscripten can catch panics but other WASM targets cannot.
            cfg!(target_family = "wasm") && !cfg!(target_os = "emscripten")
        }
        _ => false,
    };

    if desc.ignore || ignore_because_no_process_support {
        let message = CompletedTest { id, desc, result: TestResult::Ignored, exec_time: None, stdout: Vec::new() };
        monitor_ch.send(MonitorMessage::Completed(message)).unwrap();
        return None;
    }

    fn run_test_impl(
        id: test::TestId,
        desc: test::TestDesc,
        test_fn: Box<dyn FnOnce() -> Result<(), String> + Send>,
        test_run_strategy: TestRunStrategy,
        control_ch: Option<mpsc::Receiver<ControlMsg>>,
        monitor_ch: mpsc::Sender<MonitorMessage>,
        test_timeout: Option<Duration>,
        active_signal: Option<Arc<AtomicBool>>,
        no_capture: bool,
    ) -> Option<ThreadHandle> {
        let thread_pool = match &test_run_strategy {
            TestRunStrategy::InProcess(thread_pool) => thread_pool.clone(),
            TestRunStrategy::InIsolatedChildProcess(_) => None,
        };

        let name = desc.name.clone();
        let run_test = move || {
            match test_run_strategy {
                TestRunStrategy::InProcess(_)
                => run_test_in_process(id, desc, test_fn, monitor_ch, test_timeout, active_signal, no_capture),

                TestRunStrategy::InIsolatedChildProcess(cmd_hook)
                => spawn_test_subprocess(id, desc, cmd_hook, control_ch.as_ref(), monitor_ch, test_timeout, no_capture),
            }
        };

        let supports_threads = !cfg!(target_os = "emscripten") && !cfg!(target_family = "wasm");

        if supports_threads {
            let mut run_test = Arc::new(Mutex::new(Some(run_test)));
            let run_test_on_thread = run_test.clone();
            let job = move || run_test_on_thread.lock().unwrap().take().unwrap()();

            match thread_pool {
                Some(thread_pool) => {
                    let handle = thread_pool.execute(job);
                    Some(ThreadHandle::ThreadPoolThread(handle))
                }
                None => {
                    let thread = thread::Builder::new().name(name.as_slice().to_owned());
                    match thread.spawn(job) {
                        Ok(handle) => Some(ThreadHandle::StandaloneThread(handle)),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            // `ErrorKind::WouldBlock` means hitting the thread limit on some platforms, so run the test
                            // synchronously on this thread instead.
                            Arc::get_mut(&mut run_test).unwrap().get_mut().unwrap().take().unwrap()();
                            None
                        }
                        Err(e) => panic!("failed to spawn thread for test run: {e}"),
                    }
                }
            }
        } else {
            run_test();
            None
        }
    }

    match test_fn {
        test::TestFn::StaticTestFn(f) => {
            let test_fn = Box::new(move || __rust_begin_short_backtrace(f));
            run_test_impl(id, desc, test_fn, test_run_strategy, control_ch, monitor_ch, timeout, active_signal, no_capture)
        }

        test::TestFn::DynTestFn(_) => {
            panic!("dynamic tests are not supported");
        }

        | test::TestFn::StaticBenchFn(_)
        | test::TestFn::StaticBenchAsTestFn(_)
        | test::TestFn::DynBenchFn(_)
        | test::TestFn::DynBenchAsTestFn(_) => {
            panic!("benchmarks are not supported");
        }
    }
}

pub fn concurrency() -> usize {
    match env::var("RUST_TEST_THREADS").ok() {
        Some(value) => {
            value.parse::<NonZeroUsize>().ok().map(NonZeroUsize::get)
                .expect("RUST_TEST_THREADS must be a positive, non-zero integer")
        }
        None => thread::available_parallelism().map(NonZeroUsize::get).unwrap_or(1)
    }
}

#[derive(Debug)]
pub struct RunningTest {
    pub desc: test::TestDesc,
    pub timeout: Option<Duration>,
    pub start_time: Instant,
    pub control_tx: mpsc::Sender<ControlMsg>,
    pub join_handle: Option<ThreadHandle>,
    pub active_signal: Option<Arc<AtomicBool>>,
}

#[derive(Debug)]
pub enum TestEvent {
    Queue(usize, usize),
    Wait(test::TestDesc, Option<ThreadId>),
    Result(CompletedTest),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

pub fn run_tests<E, F>(
    tests: Vec<Test>,
    on_test_event: F,
    test_run_strategy: TestRunStrategy,
    no_capture: bool,
) -> Result<(Vec<Test>, Vec<RunningTest>), E>
where
    F: FnMut(TestEvent, &mut Vec<(test::TestId, Test)>) -> Result<Flow, E>,
{
    let concurrency = match &test_run_strategy {
        TestRunStrategy::InProcess(Some(thread_pool)) => thread_pool.max_thread_count(),
        _ => concurrency(),
    };
    run_tests_with_concurrency(tests, on_test_event, test_run_strategy, no_capture, concurrency)
}

/// `concurrency` caps the tests run at once, for a caller whose other runs share the same slots.
pub(crate) fn run_tests_with_progress<E, F>(tests: Vec<Test>, on_test_event: F, strategy: TestRunStrategy, no_capture: bool, context: progress::Context<'_>, concurrency: Option<usize>) -> Result<(Vec<Test>, Vec<RunningTest>), E>
where F: FnMut(TestEvent, &mut Vec<(test::TestId, Test)>) -> Result<Flow, E> {
    let _scope = progress::enter(context, &tests);
    match concurrency {
        Some(concurrency) => run_tests_with_concurrency(tests, on_test_event, strategy, no_capture, concurrency),
        None => run_tests(tests, on_test_event, strategy, no_capture),
    }
}

fn incomplete_isolation(message: &str) -> ! {
    let _ = progress::terminal(false, mutest_exit_code::PANIC);
    eprintln!("mutation analysis incomplete: {message}");
    process::exit(mutest_exit_code::PANIC)
}

/// Cleans up every isolated test after one of them failed to report, then ends the run as incomplete.
fn abort_isolated_tests(running: &mut HashMap<test::TestId, RunningTest>, receiver: &mpsc::Receiver<MonitorMessage>, message: &str) -> ! {
    if let Err(cleanup) = cleanup_isolated_tests(running, receiver) { eprintln!("additional isolated cleanup failure: {cleanup}"); }
    incomplete_isolation(message);
}

fn join_isolated(handle: ThreadHandle) -> Result<(), String> {
    let deadline = Instant::now() + subprocess::REPORT_TIMEOUT;
    while !handle.is_finished() {
        if Instant::now() >= deadline { return Err("isolated monitor did not finish after completion".to_owned()); }
        thread::sleep(subprocess::POLL_INTERVAL);
    }
    handle.join().map_err(|_| "isolated monitor panicked".to_owned())
}

/// Whether an isolated test's monitor has outlived every stage its timeout allows for.
fn monitor_overdue(test: &RunningTest) -> bool {
    test.timeout.is_some_and(|timeout| {
        test.start_time.elapsed() >= timeout.saturating_add(subprocess::STARTUP_TIMEOUT + subprocess::CLEANUP_TIMEOUT + subprocess::REPORT_TIMEOUT)
    })
}

fn receive_isolated(running: &HashMap<test::TestId, RunningTest>, receiver: &mpsc::Receiver<MonitorMessage>, deadline: Option<Instant>) -> MonitorMessage {
    loop {
        if deadline.is_none() && let Some((&id, test)) = running.iter().find(|(_, test)| monitor_overdue(test)) {
            let _ = test.control_tx.send(ControlMsg::KillChildProcess);
            return MonitorMessage::Incomplete { id, message: "isolated monitor missed its execution deadline".to_owned() };
        }
        match receiver.recv_timeout(subprocess::POLL_INTERVAL) {
            Ok(message) => return message,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let id = *running.keys().next().unwrap();
                return MonitorMessage::Incomplete { id, message: "isolated monitor channel disconnected".to_owned() };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some((&id, _)) = running.iter().find(|(_, test)| test.join_handle.as_ref().is_none_or(ThreadHandle::is_finished)) {
            return receiver.try_recv().unwrap_or_else(|_| MonitorMessage::Incomplete { id, message: "isolated monitor exited without completion".to_owned() });
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            incomplete_isolation("isolated monitors missed their cleanup deadline");
        }
    }
}

fn cleanup_isolated_tests(running: &mut HashMap<test::TestId, RunningTest>, receiver: &mpsc::Receiver<MonitorMessage>) -> Result<(), String> {
    for test in running.values() { let _ = test.control_tx.send(ControlMsg::KillChildProcess); }
    let deadline = Instant::now() + subprocess::STARTUP_TIMEOUT + subprocess::CLEANUP_TIMEOUT * 2 + subprocess::REPORT_TIMEOUT * 2;
    let mut failed = None;
    while !running.is_empty() {
        let (id, completed) = match receive_isolated(running, receiver, Some(deadline)) {
            MonitorMessage::Completed(test) => (test.id, Some(test)),
            MonitorMessage::Incomplete { id, message } => { failed.get_or_insert(message); (id, None) }
        };
        if let Some(test) = running.remove(&id) {
            let joined = test.join_handle.map(join_isolated).transpose();
            if let Err(message) = joined { failed.get_or_insert(message); }
            else if let Some(completed) = completed { progress::end(&completed, true); }
        }
    }
    failed.map_or(Ok(()), Err)
}

/// A test that panics after reporting success has failed.
fn join_in_process(join_handle: ThreadHandle, completed_test: &mut CompletedTest) {
    if join_handle.join().is_err() && completed_test.result == TestResult::Ok {
        completed_test.result = TestResult::FailedMsg("panicked after reporting success".to_owned());
    }
}

/// In-process tests past their timeout whose threads still run, which can only be abandoned.
fn timed_out_in_process_tests(running_tests: &HashMap<test::TestId, RunningTest>) -> Vec<test::TestId> {
    running_tests.iter()
        .filter(|(_, running_test)| running_test.timeout.is_some_and(|test_timeout| running_test.start_time.elapsed() > test_timeout))
        .filter_map(|(&test_id, running_test)| match &running_test.join_handle {
            Some(join_handle) => (!join_handle.is_finished()).then_some(test_id),
            None => {
                eprintln!("test timed out but it cannot be halted as it is not running concurrently");
                None
            }
        })
        .collect()
}

/// Stops an in-process test from reading the active substitutions, which may soon belong to another mutation.
fn abandon(running_test: &RunningTest) {
    if let Some(active_signal) = &running_test.active_signal {
        abandon_signal(active_signal);
    }
}

/// Reports a timed-out in-process test, and abandons it.
fn abandon_timed_out_test(test_id: test::TestId, running_test: &RunningTest) -> CompletedTest {
    abandon(running_test);
    CompletedTest {
        id: test_id,
        desc: running_test.desc.clone(),
        result: TestResult::TimedOut,
        exec_time: Some(running_test.start_time.elapsed()),
        // TODO: Propagate stdout from `run_test`.
        stdout: vec![],
    }
}

/// The next message from a running test, or `None` if an in-process test's timeout came due first.
fn receive_monitor_message(running_tests: &HashMap<test::TestId, RunningTest>, test_rx: &mpsc::Receiver<MonitorMessage>, test_run_strategy: &TestRunStrategy) -> Option<MonitorMessage> {
    if let TestRunStrategy::InIsolatedChildProcess(_) = test_run_strategy {
        return Some(receive_isolated(running_tests, test_rx, None));
    }
    let deadline = running_tests.values().filter_map(|test| test.timeout.map(|test_timeout| test.start_time + test_timeout)).reduce(Ord::min);
    let received = match deadline {
        Some(deadline) => test_rx.recv_deadline(deadline),
        None => test_rx.recv().map_err(|mpsc::RecvError| mpsc::RecvTimeoutError::Disconnected),
    };
    match received {
        Ok(message) => Some(message),
        Err(mpsc::RecvTimeoutError::Timeout) => None,
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("test monitor channel disconnected"),
    }
}

fn run_tests_with_concurrency<E, F>(
    tests: Vec<Test>,
    on_test_event: F,
    test_run_strategy: TestRunStrategy,
    no_capture: bool,
    concurrency: usize,
) -> Result<(Vec<Test>, Vec<RunningTest>), E>
where
    F: FnMut(TestEvent, &mut Vec<(test::TestId, Test)>) -> Result<Flow, E>,
{
    assert!(concurrency > 0, "test concurrency must be positive");
    let mut remaining = tests.into_iter().enumerate()
        .map(|(i, test)| (test::TestId(i), test))
        .filter(|(_, test)| matches!(test.test_fn, test::TestFn::StaticTestFn(_) | test::TestFn::DynTestFn(_)))
        .collect::<Vec<_>>();
    // Reverse the list of remaining tests so that we can `pop` from the queue in order.
    remaining.reverse();

    let supports_threads = !cfg!(target_os = "emscripten") && !cfg!(target_family = "wasm");
    let isolated = matches!(&test_run_strategy, TestRunStrategy::InIsolatedChildProcess(_));
    let synchronous = !isolated && remaining.iter().all(|(_, test)| test.timeout.is_none());
    if !supports_threads && !synchronous {
        panic!("isolated tests and timeouts require thread support");
    }

    let (sender, receiver) = mpsc::channel::<MonitorMessage>();
    let scheduler = Scheduler {
        remaining,
        running: HashMap::new(),
        lingering: HashMap::new(),
        sender,
        receiver,
        strategy: test_run_strategy,
        no_capture,
        on_test_event,
    };
    match (concurrency, synchronous, isolated) {
        (1, true, _) => scheduler.run_serially(),
        (1, _, true) => scheduler.run_in_mutation_children(),
        _ => scheduler.run_concurrently(concurrency),
    }
}

/// The tests of one run: those still queued, those running, and in-process tests abandoned at their timeout.
struct Scheduler<F> {
    remaining: Vec<(test::TestId, Test)>,
    running: HashMap<test::TestId, RunningTest>,
    lingering: HashMap<test::TestId, RunningTest>,
    sender: mpsc::Sender<MonitorMessage>,
    receiver: mpsc::Receiver<MonitorMessage>,
    strategy: TestRunStrategy,
    no_capture: bool,
    on_test_event: F,
}

impl<E, F> Scheduler<F>
where
    F: FnMut(TestEvent, &mut Vec<(test::TestId, Test)>) -> Result<Flow, E>,
{
    fn emit(&mut self, event: TestEvent) -> Result<Flow, E> {
        (self.on_test_event)(event, &mut self.remaining)
    }

    fn emit_queue(&mut self) -> Result<Flow, E> {
        self.emit(TestEvent::Queue(self.running.len(), self.remaining.len()))
    }

    /// The tests not yet started, and the tests still running when the run ended.
    fn finish(self) -> (Vec<Test>, Vec<RunningTest>) {
        (self.remaining.into_iter().map(|(_, test)| test).collect(), self.lingering.into_values().collect())
    }

    /// Runs in-process tests without timeouts one after another, joining each before starting the next.
    fn run_serially(mut self) -> Result<(Vec<Test>, Vec<RunningTest>), E> {
        if let Flow::Continue = self.run_each_to_its_end()? {
            self.emit(TestEvent::Queue(0, 0))?;
        }
        Ok(self.finish())
    }

    fn run_each_to_its_end(&mut self) -> Result<Flow, E> {
        while let Some((id, test)) = self.remaining.pop() {
            let Flow::Continue = self.emit(TestEvent::Queue(1, self.remaining.len()))? else { return Ok(Flow::Stop) };

            let desc = test.desc.clone();
            let join_handle = run_test(id, test, None, self.sender.clone(), self.strategy.clone(), None, self.no_capture);
            let Flow::Continue = self.emit(TestEvent::Wait(desc, join_handle.as_ref().map(|h| h.thread_id())))? else { return Ok(Flow::Stop) };
            let MonitorMessage::Completed(mut completed_test) = self.receiver.recv().unwrap() else { unreachable!() };

            if let Some(join_handle) = join_handle {
                join_in_process(join_handle, &mut completed_test);
            }

            progress::end(&completed_test, true);
            let Flow::Continue = self.emit(TestEvent::Result(completed_test))? else { return Ok(Flow::Stop) };
        }
        Ok(Flow::Continue)
    }

    /// Keeps up to `concurrency` tests running at once. A run the caller stops, or whose callback fails,
    /// first cleans up its isolated tests, and abandons the tests still running, returned as lingering.
    fn run_concurrently(mut self, concurrency: usize) -> Result<(Vec<Test>, Vec<RunningTest>), E> {
        let flow = self.run_until_all_finish(concurrency);
        if let TestRunStrategy::InIsolatedChildProcess(_) = &self.strategy
            && let Err(message) = cleanup_isolated_tests(&mut self.running, &self.receiver)
        {
            incomplete_isolation(&message);
        }
        self.running.values().for_each(abandon);
        flow?;
        self.lingering.extend(self.running.drain());
        Ok(self.finish())
    }

    fn run_until_all_finish(&mut self, concurrency: usize) -> Result<Flow, E> {
        while !self.running.is_empty() || !self.remaining.is_empty() {
            let Flow::Continue = self.emit_queue()? else { return Ok(Flow::Stop) };
            let Flow::Continue = self.start_queued_tests(concurrency)? else { return Ok(Flow::Stop) };
            let Flow::Continue = self.abandon_timed_out_tests()? else { return Ok(Flow::Stop) };
            if self.running.is_empty() { break; }
            let Flow::Continue = self.finish_next_test()? else { return Ok(Flow::Stop) };
        }
        Ok(Flow::Continue)
    }

    fn start_queued_tests(&mut self, concurrency: usize) -> Result<Flow, E> {
        while self.running.len() < concurrency && let Some((id, test)) = self.remaining.pop() {
            let Flow::Continue = self.emit_queue()? else { return Ok(Flow::Stop) };

            let desc = test.desc.clone();
            let timeout = test.timeout;

            let (control_tx, control_rx) = mpsc::channel::<ControlMsg>();
            let active_signal = match &self.strategy {
                TestRunStrategy::InProcess(_) => Some(Arc::new(AtomicBool::new(true))),
                TestRunStrategy::InIsolatedChildProcess(_) => None,
            };
            let join_handle = run_test(id, test, Some(control_rx), self.sender.clone(), self.strategy.clone(), active_signal.clone(), self.no_capture);
            let thread_id = join_handle.as_ref().map(|handle| handle.thread_id());
            self.running.insert(id, RunningTest { desc: desc.clone(), timeout, start_time: Instant::now(), control_tx, join_handle, active_signal });
            let Flow::Continue = self.emit(TestEvent::Wait(desc, thread_id))? else { return Ok(Flow::Stop) };
        }
        Ok(Flow::Continue)
    }

    /// Reports in-process tests past their timeout as timed out, and leaves their threads lingering.
    fn abandon_timed_out_tests(&mut self) -> Result<Flow, E> {
        let TestRunStrategy::InProcess(_) = &self.strategy else { return Ok(Flow::Continue) };
        for test_id in timed_out_in_process_tests(&self.running) {
            let running_test = self.running.remove(&test_id).unwrap();
            let Flow::Continue = self.emit_queue()? else { return Ok(Flow::Stop) };

            let completed_test = abandon_timed_out_test(test_id, &running_test);
            self.lingering.insert(test_id, running_test);
            progress::end(&completed_test, false);
            let Flow::Continue = self.emit(TestEvent::Result(completed_test))? else { return Ok(Flow::Stop) };
        }
        Ok(Flow::Continue)
    }

    fn finish_next_test(&mut self) -> Result<Flow, E> {
        let Some(message) = receive_monitor_message(&self.running, &self.receiver, &self.strategy) else { return Ok(Flow::Continue) };
        let mut completed_test = match message {
            MonitorMessage::Completed(test) => test,
            MonitorMessage::Incomplete { id: _, message } => abort_isolated_tests(&mut self.running, &self.receiver, &message),
        };

        let Some(running_test) = self.running.remove(&completed_test.id) else {
            // The test completion corresponds to a test that has been previously marked as timed out.
            // In this case, the completion was caused by changes in the active mutations and should be considered bogus.
            return Ok(Flow::Continue);
        };

        match (running_test.join_handle, &self.strategy) {
            (Some(join_handle), TestRunStrategy::InIsolatedChildProcess(_)) => {
                if let Err(message) = join_isolated(join_handle) { abort_isolated_tests(&mut self.running, &self.receiver, &message); }
            }
            (Some(join_handle), TestRunStrategy::InProcess(_)) => join_in_process(join_handle, &mut completed_test),
            (None, _) => {}
        }

        let Flow::Continue = self.emit_queue()? else { return Ok(Flow::Stop) };
        progress::end(&completed_test, true);
        self.emit(TestEvent::Result(completed_test))
    }
}

#[cfg(test)]
mod abandon_tests {
    use super::*;

    static RELEASE: AtomicBool = AtomicBool::new(false);

    fn named_test(name: &'static str, test_fn: fn() -> Result<(), String>) -> Test {
        Test {
            desc: test::TestDesc {
                name: test::StaticTestName(name), ignore: false, ignore_message: None,
                source_file: file!(), start_line: 0, start_col: 0, end_line: 0, end_col: 0,
                should_panic: test::ShouldPanic::No, compile_fail: false, no_run: false,
                test_type: test::TestType::UnitTest,
            },
            test_fn: test::TestFn::StaticTestFn(test_fn), timeout: None, unrepeatable: false,
        }
    }

    #[test]
    fn a_run_stopped_early_abandons_the_tests_it_leaves_running() {
        let blocks = || {
            while !RELEASE.load(atomic::Ordering::SeqCst) { thread::sleep(Duration::from_millis(1)); }
            Ok(())
        };
        let (_, lingering) = run_tests_with_concurrency(vec![named_test("finishes", || Ok(())), named_test("blocks", blocks)], |event, _| -> Result<Flow, ()> {
            Ok(match event { TestEvent::Result(_) => Flow::Stop, _ => Flow::Continue })
        }, TestRunStrategy::InProcess(None), false, 2).unwrap();
        RELEASE.store(true, atomic::Ordering::SeqCst);
        let [lingering] = &lingering[..] else { panic!("the blocked test must linger") };
        assert!(!lingering.active_signal.as_ref().unwrap().load(atomic::Ordering::SeqCst));
    }

    #[test]
    fn an_abandoned_test_thread_unwinds_without_a_second_cancellation() {
        struct Probe(mpsc::Sender<(bool, bool)>);
        impl Drop for Probe {
            fn drop(&mut self) {
                let _ = self.0.send((is_test_thread_active(), test_thread_abandoned()));
            }
        }
        let (tx, rx) = mpsc::channel();
        let joined = thread::spawn(move || {
            let active_signal = TestThreadSignal::set(Arc::new(AtomicBool::new(true)));
            abandon_signal(&active_signal.0);
            let probe = Probe(tx);
            probe.0.send((is_test_thread_active(), test_thread_abandoned())).unwrap();
            panic!("cancelled");
        }).join();
        assert!(joined.is_err());
        assert_eq!(rx.iter().collect::<Vec<_>>(), [(false, true), (true, true)]);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    /// The body of a fixture test that a child process runs; a crash or a hang ends the child before it writes coverage.
    pub(super) fn fixture_fn(name: &str) -> fn() -> Result<(), String> {
        match name {
            "fail" => || Err("fails".to_owned()),
            "crash" => || process::exit(3),
            "should-panic" => || panic!("expected"),
            "hang" => || loop { thread::park(); },
            _ => || Ok(()),
        }
    }

    pub(super) fn descriptor(timeout: Option<Duration>) -> Test {
        Test {
            desc: test::TestDesc {
                name: test::StaticTestName("serial-isolated"), ignore: false, ignore_message: None,
                source_file: file!(), start_line: 0, start_col: 0, end_line: 0, end_col: 0,
                should_panic: test::ShouldPanic::No, compile_fail: false, no_run: false,
                test_type: test::TestType::UnitTest,
            },
            test_fn: test::TestFn::StaticTestFn(|| Ok(())), timeout, unrepeatable: false,
        }
    }

    #[test]
    fn isolated_serial_child() {
        dispatch_subprocess_owner();
        let Some(marker) = env::var_os("MUTEST_SERIAL_CHILD") else { return; };
        let marker = PathBuf::from(marker);
        let pending = marker.with_extension("pending");
        fs::write(&pending, process::id().to_string()).unwrap();
        fs::rename(pending, marker).unwrap();
        if env::var_os("MUTEST_SERIAL_ABORT").is_some() {
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            // SAFETY: `getrlimit` writes only the given `rlimit`.
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) }, 0);
            // A core dump would hold this crash past its timeout on hosts with a pipe handler.
            if limit.rlim_cur != limit.rlim_max.min(1) { process::exit(TR_FAILED); }
            process::abort();
        }
        if env::var_os("MUTEST_SERIAL_HANG").is_some() {
            loop { thread::park(); }
        }
        process::exit(TR_OK);
    }

    #[test]
    fn serial_isolated_success_timeout_crash_and_cancellation_reap_children() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-scratch").join(format!(
            "serial-scheduler-{}-{}", process::id(), NEXT.fetch_add(1, atomic::Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        let scratch = Scratch(root);
        for (case, cancel, timeout, expected) in [
            ("success", false, Some(Duration::from_secs(5)), Some(TestResult::Ok)),
            ("timeout", false, Some(Duration::from_secs(2)), Some(TestResult::TimedOut)),
            ("abort", false, Some(Duration::from_secs(5)), Some(TestResult::CrashedMsg("received signal 6".to_owned()))),
            ("cancel", true, Some(Duration::from_secs(5)), None),
            ("cancel-without-timeout", true, None, None),
        ] {
            let marker = scratch.0.join(case);
            let child_marker = marker.clone();
            let strategy = TestRunStrategy::InIsolatedChildProcess(Arc::new(move |command| {
                command.env_clear().args(["--exact", "test_runner::tests::isolated_serial_child", "--test-threads=1", "--nocapture"])
                    .env("MUTEST_SERIAL_CHILD", &child_marker);
                match case {
                    "success" => {}
                    "abort" => { command.env("MUTEST_SERIAL_ABORT", "1"); }
                    _ => { command.env("MUTEST_SERIAL_HANG", "1"); }
                }
                keep_loader_environment(command);
            }));
            let mut results = Vec::new();
            let mut observed_wait = false;
            let (remaining, lingering) = run_tests_with_concurrency(vec![descriptor(timeout)], |event, _| -> Result<Flow, ()> {
                match event {
                    TestEvent::Wait(_, _) if cancel => {
                        observed_wait = true;
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while !marker.exists() && Instant::now() < deadline { thread::sleep(Duration::from_millis(1)); }
                        return Ok(Flow::Stop);
                    }
                    TestEvent::Wait(_, _) => observed_wait = true,
                    TestEvent::Result(test) => results.push(test.result),
                    _ => {},
                }
                Ok(Flow::Continue)
            }, strategy, false, 1).unwrap();
            assert!(observed_wait);
            assert!(remaining.is_empty());
            assert!(lingering.is_empty(), "{case} left monitors running");
            assert_eq!(results, expected.into_iter().collect::<Vec<_>>(), "{case}");
            assert_reaped(&marker, &format!("{case}: child"));
        }
    }

    const LIFECYCLE_ENTRY: &str = "test_runner::tests::isolated_lifecycle_fixture";
    pub(super) const FIXTURE_RUN_BOUND: Duration = Duration::from_secs(12);
    const FIXTURE_CLEANUP_BOUND: Duration = Duration::from_secs(4);
    const FLOOD_BYTES: usize = 256 * 1024;

    /// A child started with a cleared environment still needs the loader paths this test binary runs with.
    fn keep_loader_environment(command: &mut Command) {
        for name in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
            if let Some(value) = env::var_os(name) { command.env(name, value); }
        }
    }

    fn fixture_command(root: &Path, role: &str, scenario: &str) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args(["--exact", LIFECYCLE_ENTRY, "--test-threads=1", "--nocapture"])
            .env_clear().env("MUTEST_LIFECYCLE_ROOT", root).env("MUTEST_LIFECYCLE_ROLE", role)
            .env("MUTEST_LIFECYCLE_SCENARIO", scenario).stdin(process::Stdio::null());
        keep_loader_environment(&mut command);
        command
    }

    fn assert_reaped(marker: &PathBuf, what: &str) {
        let pid: u32 = fs::read_to_string(marker).unwrap().parse().unwrap();
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists(), "{what} {pid} was left alive or unreaped");
    }

    fn fixture_marker(root: &Path, name: &str) {
        let path = root.join(name);
        fs::write(path.with_extension("pending"), process::id().to_string()).unwrap();
        fs::rename(path.with_extension("pending"), path).unwrap();
    }

    fn fixture_wait(root: &Path, name: &str) {
        let deadline = Instant::now() + FIXTURE_RUN_BOUND;
        while !root.join(name).exists() {
            assert!(Instant::now() < deadline, "fixture handshake expired: {name}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    // This disposable subreaper owns only fixture processes, never another libtest test's children.
    fn fixture_cleanup() -> io::Result<()> {
        let deadline = Instant::now() + FIXTURE_CLEANUP_BOUND;
        loop {
            for pid in crate::supervisor::children() {
                // SAFETY: No other thread reaps adopted holders; direct children belong to this failing fixture.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            // SAFETY: WNOHANG cannot block, and this process owns only the fixture subtree.
            let reaped = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
            if reaped < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) { return Ok(()); }
            if Instant::now() >= deadline { return Err(io::Error::other("fixture cleanup expired")); }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Runs each test as a lifecycle fixture child in its own directory under `root`.
    fn lifecycle_strategy(root: PathBuf, scenario: String) -> TestRunStrategy {
        TestRunStrategy::InIsolatedChildProcess(Arc::new(move |command| {
            if scenario == "failure-panic" { panic!("injected monitor panic"); }
            let name = command.get_envs().find(|(key, _)| *key == TEST_SUBPROCESS_INVOCATION)
                .and_then(|(_, value)| value).unwrap().to_owned();
            let child_root = root.join(name);
            fs::create_dir_all(&child_root).unwrap();
            let replacement = fixture_command(&child_root, "child", &scenario);
            command.env_clear().args(replacement.get_args()).envs(replacement.get_envs().filter_map(|(key, value)| value.map(|value| (key, value))))
                .env("MUTEST_LIFECYCLE_SIBLINGS", &root);
        }))
    }

    /// After libtest's own output, the child's stdout flood then its stderr flood, each captured whole.
    fn assert_flood_captured_whole(output: &[u8]) {
        let flood = output.iter().position(|&byte| byte == 0xfe).map_or(&[][..], |start| &output[start..]);
        assert!(flood == [vec![0xfe; FLOOD_BYTES], vec![0xfd; FLOOD_BYTES]].concat(), "flood output was not captured whole");
    }

    /// One lifecycle scenario's reactions to scheduler events, and the results it saw.
    struct ScenarioRun<'a> {
        root: &'a Path,
        scenario: &'a str,
        concurrency: usize,
        cancelled: bool,
        waits: usize,
        results: Vec<(usize, TestResult)>,
    }

    impl ScenarioRun<'_> {
        fn observe(&mut self, event: TestEvent) -> Result<Flow, &'static str> {
            match event {
                TestEvent::Wait(_, _) => {
                    self.waits += 1;
                    if self.waits == self.concurrency && self.cancelled { return self.cancel(); }
                }
                TestEvent::Result(test) => self.record(test),
                TestEvent::Queue(_, _) => {}
            }
            Ok(Flow::Continue)
        }

        /// Stops the run once every test holds a descendant: `callback-error` by failing, the others with `Stop`.
        fn cancel(&self) -> Result<Flow, &'static str> {
            for id in 0..self.concurrency { fixture_wait(&self.root.join(id.to_string()), "holder"); }
            if self.scenario == "callback-error" { Err("callback rejected") } else { Ok(Flow::Stop) }
        }

        fn record(&mut self, test: CompletedTest) {
            assert!(!self.scenario.starts_with("failure-"), "infrastructure failure delivered a test result");
            if self.scenario == "flood" { assert_flood_captured_whole(&test.stdout); }
            fixture_marker(self.root, &format!("result-{}", test.id.0));
            self.results.push((test.id.0, test.result));
        }

        fn expected(&self) -> Vec<(usize, TestResult)> {
            match self.scenario {
                _ if self.cancelled => vec![],
                "out-of-order" => vec![(1, TestResult::Ok), (0, TestResult::Ok)],
                "timeout" => (0..self.concurrency).map(|id| (id, TestResult::TimedOut)).collect(),
                _ => (0..self.concurrency).map(|id| (id, TestResult::Ok)).collect(),
            }
        }
    }

    fn lifecycle_scheduler(root: PathBuf, scenario: String) {
        let concurrency = if scenario.ends_with("-serial") { 1 } else { 2 };
        let scenario = scenario.trim_end_matches("-serial");
        let timeout = match scenario {
            "cancel-unlimited" | "failure-owner-exit" => None,
            _ => Some(Duration::from_secs(2)),
        };
        let tests = (0..concurrency).map(|id| {
            let mut test = descriptor(timeout);
            test.desc.name = test::DynTestName(id.to_string());
            test
        }).collect();
        let cancelled = matches!(scenario, "cancel" | "cancel-unlimited" | "callback-error");
        let mut run = ScenarioRun { root: &root, scenario, concurrency, cancelled, waits: 0, results: Vec::new() };
        let returned = run_tests_with_concurrency(tests, |event, _| run.observe(event), lifecycle_strategy(root.clone(), scenario.to_owned()), false, concurrency);
        if scenario == "callback-error" {
            assert_eq!(returned.unwrap_err(), "callback rejected");
        } else {
            let (remaining, lingering) = returned.unwrap();
            assert!(remaining.is_empty());
            assert!(lingering.is_empty(), "isolated monitors outlived scheduler");
        }

        let expected = run.expected();
        if scenario != "out-of-order" { run.results.sort_by_key(|(id, _)| *id); }
        assert_eq!(run.results, expected, "{scenario}");

        for id in 0..concurrency {
            let test_root = root.join(id.to_string());
            assert_reaped(&test_root.join("child"), &format!("{scenario}: child"));
            if !matches!(scenario, "flood" | "out-of-order") {
                assert_reaped(&test_root.join("holder"), &format!("{scenario}: holder"));
            }
        }
    }

    #[test]
    fn isolated_lifecycle_fixture() {
        dispatch_subprocess_owner();
        let Ok(role) = env::var("MUTEST_LIFECYCLE_ROLE") else { return; };
        let root = PathBuf::from(env::var_os("MUTEST_LIFECYCLE_ROOT").unwrap());
        let scenario = env::var("MUTEST_LIFECYCLE_SCENARIO").unwrap();
        match role.as_str() {
            "container" => lifecycle_container(&root, &scenario),
            "scheduler" => lifecycle_scheduler(root, scenario),
            "holder" => lifecycle_holder(&root, &scenario),
            "orphan-parent" => lifecycle_orphan_parent(&root, &scenario),
            _ => lifecycle_child(&root, &scenario),
        }
    }

    /// Runs the scheduler as a child of this subreaper, and fails if anything it started outlived it.
    fn lifecycle_container(root: &Path, scenario: &str) {
        crate::supervisor::adopt_orphans().unwrap();
        let mut scheduler = fixture_command(root, "scheduler", scenario).spawn().unwrap();
        let deadline = Instant::now() + FIXTURE_RUN_BOUND;
        let outcome = loop {
            match scheduler.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
                _ => { let _ = scheduler.kill(); break None; }
            }
        };
        let remaining = crate::supervisor::children();
        let cleanup = fixture_cleanup();
        assert!(cleanup.is_ok(), "outer containment cleanup failed: {cleanup:?}");
        let expected = if scenario.starts_with("failure-") { mutest_exit_code::PANIC } else { 0 };
        assert_eq!(outcome.and_then(|status| status.code()), Some(expected), "scheduler did not complete cleanly within {FIXTURE_RUN_BOUND:?}");
        assert!(remaining.is_empty(), "scheduler needed outer cleanup for {remaining:?}");
    }

    fn lifecycle_holder(root: &Path, scenario: &str) -> ! {
        fixture_marker(root, "holder");
        if scenario == "orphan-reap" { process::exit(0); }
        thread::sleep(FIXTURE_RUN_BOUND * 3);
        process::exit(125);
    }

    fn lifecycle_orphan_parent(root: &Path, scenario: &str) -> ! {
        drop(fixture_command(root, "holder", scenario).spawn().unwrap());
        fixture_wait(root, "holder");
        process::exit(0);
    }

    fn lifecycle_child(root: &Path, scenario: &str) -> ! {
        fixture_marker(root, "child");
        match scenario {
            _ if scenario == "flood" || scenario.starts_with("failure-") => flood_output(),
            "orphan-reap" => {
                let mut parent = fixture_command(root, "orphan-parent", scenario).spawn().unwrap();
                assert!(parent.wait().unwrap().success());
                let pid: u32 = fs::read_to_string(root.join("holder")).unwrap().parse().unwrap();
                let deadline = Instant::now() + Duration::from_secs(1);
                while PathBuf::from(format!("/proc/{pid}")).exists() {
                    assert!(Instant::now() < deadline, "exited orphan remained unreaped while direct test was alive");
                    thread::sleep(Duration::from_millis(2));
                }
            }
            "out-of-order" => {
                let siblings = PathBuf::from(env::var_os("MUTEST_LIFECYCLE_SIBLINGS").unwrap());
                if root.file_name().unwrap() == "0" { fixture_wait(&siblings, "result-1"); }
                else { fixture_wait(&siblings.join("0"), "child"); }
            }
            _ => {
                use std::os::unix::process::CommandExt;
                let mut holder = fixture_command(root, "holder", scenario);
                // SAFETY: setsid is async-signal-safe; the child deliberately escapes its original process group.
                unsafe { holder.pre_exec(|| if libc::setsid() < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }); }
                drop(holder.spawn().unwrap());
                fixture_wait(root, "holder");
                if scenario != "exit" { thread::sleep(FIXTURE_RUN_BOUND * 3); }
            }
        }
        process::exit(TR_OK);
    }

    fn flood_output() {
        use std::io::Write;
        let chunk = [0xfe; 16384];
        let error_chunk = [0xfd; 16384];
        for _ in 0..FLOOD_BYTES / chunk.len() {
            io::stdout().write_all(&chunk).unwrap();
            io::stderr().write_all(&error_chunk).unwrap();
        }
    }

    #[test]
    fn isolated_lifecycle_is_bounded_and_reaps_detached_pipe_holders() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-scratch").join(format!(
            "isolated-lifecycle-{}-{}", process::id(), NEXT.fetch_add(1, atomic::Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        let scratch = Scratch(root);
        for scenario in ["timeout-serial", "exit-serial", "flood-serial", "cancel-serial", "callback-error-serial",
                         "timeout", "exit", "flood", "cancel", "callback-error", "out-of-order", "cancel-unlimited-serial", "cancel-unlimited",
                         "failure-setup", "failure-cleanup", "failure-panic", "failure-completion", "failure-owner-exit", "orphan-reap"] {
            let root = scratch.0.join(scenario);
            fs::create_dir(&root).unwrap();
            let output = fs::File::create(root.join("output")).unwrap();
            let mut child = fixture_command(&root, "container", scenario)
                .stdout(output.try_clone().unwrap()).stderr(output).spawn().unwrap();
            let deadline = Instant::now() + FIXTURE_RUN_BOUND + FIXTURE_CLEANUP_BOUND + Duration::from_secs(4);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() { break status; }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("outer fixture exceeded containment deadline: {}", root.display());
                }
                thread::sleep(Duration::from_millis(5));
            };
            let output = fs::read_to_string(root.join("output")).unwrap();
            assert!(status.success(), "{scenario}: {status}\n{output}");
        }
    }

    #[test]
    fn missing_monitor_completion_and_stalled_join_are_bounded() {
        let (done_tx, done_rx) = mpsc::channel();
        let handle = ThreadHandle::StandaloneThread(thread::spawn(move || { let _ = done_rx.recv(); }));
        let (control_tx, control_rx) = mpsc::channel();
        let id = test::TestId(7);
        let mut running = HashMap::from([(id, RunningTest {
            desc: descriptor(None).desc, timeout: Some(Duration::ZERO),
            start_time: Instant::now() - subprocess::STARTUP_TIMEOUT - subprocess::CLEANUP_TIMEOUT - subprocess::REPORT_TIMEOUT,
            control_tx, join_handle: Some(handle), active_signal: None,
        })]);
        let (_tx, rx) = mpsc::channel();
        assert!(matches!(receive_isolated(&running, &rx, None), MonitorMessage::Incomplete { id: test::TestId(7), .. }));
        assert_eq!(control_rx.try_recv(), Ok(ControlMsg::KillChildProcess));
        let started = Instant::now();
        let error = join_isolated(running.remove(&id).unwrap().join_handle.unwrap()).unwrap_err();
        done_tx.send(()).unwrap();
        assert_eq!(error, "isolated monitor did not finish after completion");
        assert!(started.elapsed() < subprocess::REPORT_TIMEOUT + Duration::from_secs(1));

        let finished = ThreadHandle::StandaloneThread(thread::spawn(|| {}));
        while !finished.is_finished() { thread::yield_now(); }
        running.insert(id, RunningTest {
            desc: descriptor(None).desc, timeout: None, start_time: Instant::now(),
            control_tx: mpsc::channel().0, join_handle: Some(finished), active_signal: None,
        });
        assert!(matches!(receive_isolated(&running, &rx, None), MonitorMessage::Incomplete { id: test::TestId(7), .. }));
        join_isolated(running.remove(&id).unwrap().join_handle.unwrap()).unwrap();
    }

    #[test]
    fn serial_in_process_test_keeps_its_timeout() {
        let mut results = Vec::new();
        let (remaining, lingering) = run_tests_with_concurrency(vec![descriptor(Some(Duration::from_secs(5)))], |event, _| -> Result<Flow, ()> {
            if let TestEvent::Result(test) = event { results.push(test.result); }
            Ok(Flow::Continue)
        }, TestRunStrategy::InProcess(None), false, 1).unwrap();
        assert_eq!(results, [TestResult::Ok]);
        assert!(remaining.is_empty());
        assert!(lingering.is_empty());
    }
}
