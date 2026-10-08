//! Runs the isolated tests of one run in turn in one child process. A new child starts only after a test fails,
//! crashes or times out, or after the queue changes, so a mutation costs one process start, not one for each test.

use std::any::Any;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::iter;
use std::panic;
use std::path::PathBuf;
use std::process::{self, Command};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{CompletedTest, Flow, RunningTest, Scheduler, TR_FAILED, TR_OK, Test, TestEvent, TestResult, TestRunStrategy};
use super::{__rust_begin_short_backtrace, incomplete_isolation, progress, test};

/// The file that lists the tests a child runs in turn. The child appends a line with the run time of each test
/// that passes to the same path with the extension `passed`.
pub(crate) const TESTS_OF_CHILD_VAR: &str = "__MUTEST_ISOLATED_TESTS";

static NEXT_LIST: AtomicU64 = AtomicU64::new(0);

#[path = "mutation_child/child.rs"]
mod child;
use child::Child;


impl<E, F> Scheduler<F>
where
    F: FnMut(TestEvent, &mut Vec<(test::TestId, Test)>) -> Result<Flow, E>,
{
    /// Runs the queue one test at a time, each test in the child process of the tests before it while they pass.
    pub(super) fn run_in_mutation_children(mut self) -> Result<(Vec<Test>, Vec<RunningTest>), E> {
        let TestRunStrategy::InIsolatedChildProcess(cmd_hook) = self.strategy.clone() else { unreachable!() };
        let mut child = None;
        let flow = self.run_each_in_a_child(&cmd_hook, &mut child);
        drop(child);
        flow?;
        Ok(self.finish())
    }

    fn run_each_in_a_child(&mut self, cmd_hook: &Arc<dyn Fn(&mut Command) + Send + Sync>, child: &mut Option<Child>) -> Result<Flow, E> {
        while let Some((id, test)) = self.remaining.pop() {
            // A child runs the queue as it was when the child started; once the queue changes, a new child runs it.
            if child.as_ref().is_some_and(|child| child.tests.front() != Some(&id)) { *child = None; }
            let Flow::Continue = self.emit(TestEvent::Queue(1, self.remaining.len()))? else { return Ok(Flow::Stop) };
            progress::start(id, &test, true);
            if !test.desc.ignore && child.is_none() {
                let spawned = Child::spawn(cmd_hook.clone(), &self.batch(id, &test), self.no_capture);
                *child = Some(spawned.unwrap_or_else(|error| incomplete_isolation(&error.to_string())));
            }
            let started = Instant::now();
            let Flow::Continue = self.emit(TestEvent::Wait(test.desc.clone(), None))? else { return Ok(Flow::Stop) };
            let (result, exec_time, stdout) = result_in(child, &test, started);
            let completed = CompletedTest { id, desc: test.desc, result, exec_time, stdout };
            let Flow::Continue = self.emit(TestEvent::Queue(0, self.remaining.len()))? else { return Ok(Flow::Stop) };
            progress::end(&completed, true);
            let Flow::Continue = self.emit(TestEvent::Result(completed))? else { return Ok(Flow::Stop) };
        }
        Ok(Flow::Continue)
    }

    /// The tests a new child runs in turn from `test` on. A test that fails when it runs again in one process runs
    /// alone; the others run with the queued tests after them, up to the first that is ignored or runs alone.
    fn batch<'a>(&'a self, id: test::TestId, test: &'a Test) -> Vec<(test::TestId, &'a test::TestName)> {
        let following = self.remaining.iter().rev().take_while(|(_, next)| !test.unrepeatable && !next.unrepeatable && !next.desc.ignore);
        iter::once((id, &test.desc.name)).chain(following.map(|(id, next)| (*id, &next.desc.name))).collect()
    }
}

/// The result of `test` from `child`, which ends once it has exited.
fn result_in(child: &mut Option<Child>, test: &Test, started: Instant) -> (TestResult, Option<Duration>, Vec<u8>) {
    let Some(running) = child.as_mut().filter(|_| !test.desc.ignore) else { return (TestResult::Ignored, None, Vec::new()) };
    let (result, exec_time, stdout, alive) = running.next_result(test.timeout, started);
    if !alive { *child = None; }
    (result, Some(exec_time), stdout)
}

/// The tests this child runs in turn, and the file it reports each pass to. Unless the list and the report
/// both open, the child runs only `first`, without a report.
pub(crate) fn tests_of_child<'a>(tests: &[&'a test::TestDescAndFn], first: &str, list: Option<OsString>) -> (Vec<&'a test::TestDescAndFn>, Option<File>) {
    let by_name = tests.iter().map(|test| (test.desc.name.as_slice(), *test)).collect::<HashMap<_, _>>();
    let find = |name: &str| *by_name.get(name).unwrap_or_else(|| panic!("cannot find test with name `{name}`"));
    let list = list.map(PathBuf::from);
    let listed = list.and_then(|list| Some((fs::read_to_string(&list).ok()?, fs::OpenOptions::new().append(true).open(list.with_extension("passed")).ok()?)));
    match listed {
        Some((names, report)) => (names.split('\n').map(find).collect(), Some(report)),
        None => (vec![find(first)], None),
    }
}

/// Runs `tests` in turn in this child process, and reports the run time of each that passes to `report`. The first
/// test that does not pass, or that passes with no report, ends the process with its result.
pub fn run_tests_in_spawned_subprocess(tests: Vec<test::TestDescAndFn>, mut report: Option<File>) -> ! {
    for test in tests {
        let test::TestFn::StaticTestFn(f) = test.testfn else { unreachable!() };
        let start = Instant::now();
        let outcome = run_on_own_thread(test.desc.name.as_slice().to_owned(), f);
        let result = TestResult::from_task(test.desc.should_panic, outcome.as_ref().map(|_| ()).map_err(|payload| &**payload), None, None);
        if let TestResult::FailedMsg(msg) = &result {
            eprintln!("{msg}");
        }
        let reported = result == TestResult::Ok && report.as_mut().is_some_and(|report| report.write_all(format!("{}\n", start.elapsed().as_nanos()).as_bytes()).is_ok());
        if !reported { process::exit(if result == TestResult::Ok { TR_OK } else { TR_FAILED }); }
    }
    process::exit(TR_OK);
}

/// Runs a test on a new thread named after it, as a test that runs in process does, so no thread-local state carries
/// over to the next test. A panic that the test does not catch, or an error it returns, is its outcome.
fn run_on_own_thread(name: String, f: fn() -> Result<(), String>) -> Result<(), Box<dyn Any + Send>> {
    let run = move || __rust_begin_short_backtrace(f);
    let outcome = thread::Builder::new().name(name).spawn(run).map_or_else(|_| panic::catch_unwind(run), |handle| handle.join());
    outcome.and_then(|returned| returned.map_err(|error| Box::new(error) as Box<dyn Any + Send>))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::env;
    use std::path::Path;
    use std::sync::atomic;

    use super::*;
    use super::super::{TEST_SUBPROCESS_INVOCATION, dispatch_subprocess_owner, run_tests_with_concurrency};
    use super::super::tests::fixture_fn;

    const ENTRY: &str = "test_runner::mutation_child::tests::mutation_child_fixture";

    fn fixture(name: &'static str, unrepeatable: bool) -> Test {
        let mut test = super::super::tests::descriptor(Some(Duration::from_secs(if name == "hang" { 1 } else { 10 })));
        test.desc.name = test::StaticTestName(name);
        test.desc.ignore = name == "ignored";
        test.desc.should_panic = if name == "should-panic" { test::ShouldPanic::Yes } else { test::ShouldPanic::No };
        test.test_fn = test::TestFn::StaticTestFn(fixture_fn(name));
        test.unrepeatable = unrepeatable;
        test
    }

    /// Runs the tests the parent listed, as the harness of an analyzed crate does, and counts each child start.
    #[test]
    fn mutation_child_fixture() {
        dispatch_subprocess_owner();
        let Some(root) = env::var_os("MUTEST_MUTATION_CHILD") else { return; };
        fs::OpenOptions::new().append(true).create(true).open(Path::new(&root).join("starts")).unwrap().write_all(b"started\n").unwrap();
        let names = ["pass-0", "fail", "pass-1", "crash", "pass-2", "hang", "should-panic", "pass-3", "once"];
        let fixtures = names.map(|name| { let test = fixture(name, false); test::TestDescAndFn { desc: test.desc, testfn: test.test_fn } });
        let (tests, report) = tests_of_child(&fixtures.iter().collect::<Vec<_>>(), &env::var(TEST_SUBPROCESS_INVOCATION).unwrap(), env::var_os(TESTS_OF_CHILD_VAR));
        let tests = tests.into_iter().map(|test| test::TestDescAndFn { desc: test.desc.clone(), testfn: test::TestFn::StaticTestFn(fixture_fn(test.desc.name.as_slice())) });
        run_tests_in_spawned_subprocess(tests.collect(), report);
    }

    /// Runs `tests` in mutation children, stopping or removing tests as `on_result` asks, and returns the results,
    /// the tests left in the queue, and how many children started.
    fn run(tests: Vec<Test>, mut on_result: impl FnMut(&str, &mut Vec<(test::TestId, Test)>) -> Flow) -> (Vec<(String, TestResult)>, usize, usize) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-scratch")
            .join(format!("mutation-child-{}-{}", process::id(), NEXT_LIST.fetch_add(1, atomic::Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        let child_root = root.clone();
        let strategy = TestRunStrategy::InIsolatedChildProcess(Arc::new(move |command| {
            command.args(["--exact", ENTRY, "--test-threads=1", "--nocapture"]).env("MUTEST_MUTATION_CHILD", &child_root);
        }));
        let mut results = Vec::new();
        let (remaining, lingering) = run_tests_with_concurrency(tests, |event, queue| -> Result<Flow, ()> {
            let TestEvent::Result(test) = event else { return Ok(Flow::Continue) };
            results.push((test.desc.name.as_slice().to_owned(), test.result));
            Ok(on_result(test.desc.name.as_slice(), queue))
        }, strategy, false, 1).unwrap();
        assert!(lingering.is_empty());
        let starts = fs::read_to_string(root.join("starts")).unwrap().lines().count();
        fs::remove_dir_all(&root).unwrap();
        (results, remaining.len(), starts)
    }

    #[test]
    fn a_child_runs_tests_in_turn_until_one_fails_crashes_or_times_out() {
        let names = ["pass-0", "fail", "pass-1", "crash", "pass-2", "hang", "should-panic", "pass-3"];
        let (results, remaining, starts) = run(names.map(|name| fixture(name, false)).into(), |_, _| Flow::Continue);
        let expected = [TestResult::Ok, TestResult::Failed, TestResult::Ok, TestResult::CrashedMsg("got unexpected exit code 3".to_owned()),
            TestResult::Ok, TestResult::TimedOut, TestResult::Ok, TestResult::Ok];
        assert_eq!(results, names.iter().map(|name| name.to_string()).zip(expected).collect::<Vec<_>>());
        assert_eq!((remaining, starts), (0, 4));
    }

    #[test]
    fn a_changed_queue_or_a_test_that_runs_alone_starts_a_new_child() {
        let tests = vec![fixture("pass-0", false), fixture("pass-1", false), fixture("once", true), fixture("ignored", false), fixture("pass-2", false), fixture("pass-3", false)];
        let (results, remaining, starts) = run(tests, |name, queue| match name {
            "pass-0" => { queue.retain(|(_, test)| test.desc.name.as_slice() != "pass-1"); Flow::Continue }
            "pass-2" => Flow::Stop,
            _ => Flow::Continue,
        });
        let expected = [("pass-0", TestResult::Ok), ("once", TestResult::Ok), ("ignored", TestResult::Ignored), ("pass-2", TestResult::Ok)];
        assert_eq!(results, expected.map(|(name, result)| (name.to_owned(), result)));
        assert_eq!((remaining, starts), (1, 3));
    }

    #[test]
    fn a_child_without_its_list_runs_only_the_named_test() {
        let fixtures = ["pass-0", "pass-1"].map(|name| { let test = fixture(name, false); test::TestDescAndFn { desc: test.desc, testfn: test.test_fn } });
        let (tests, report) = tests_of_child(&fixtures.iter().collect::<Vec<_>>(), "pass-1", None);
        assert_eq!(tests.iter().map(|test| test.desc.name.as_slice()).collect::<Vec<_>>(), ["pass-1"]);
        assert!(report.is_none());
    }
}
