use std::fs;
use std::sync::{Barrier, OnceLock};

use super::*;
use crate::metadata::{BatchedMutantMeta, EntryPoints};

type Map = [Option<SubstMeta>; 2];
static ACTIVE: ActiveMutantHandle<Map> = ActiveMutantHandle::empty();
static ABORT: MutationMeta = MutationMeta {
    id: 1, safety: MutationSafety::Safe, op_name: "fixture", display_name: "abort",
    display_location: "fixture:1", undetected_diagnostic: "abort survived",
    reachable_from: EntryPoints::InternalTests(phf::phf_map! { "abort" => 0usize }),
};
static COLLATERAL: MutationMeta = MutationMeta {
    id: 2, safety: MutationSafety::Safe, op_name: "fixture", display_name: "collateral",
    display_location: "fixture:2", undetected_diagnostic: "collateral survived",
    reachable_from: EntryPoints::InternalTests(phf::phf_map! { "collateral" => 0usize }),
};
static SOLO: StandaloneMutantMeta = StandaloneMutantMeta {
    mutation: &ABORT, substitutions: &[(0, SubstMeta { mutation: &ABORT })],
};
static EMPTY: MetaMutant<Map> = MetaMutant {
    cargo_package_name: None, cargo_target_kind: None, crate_name: "runtime_fixture",
    active_mutant_handle: &ACTIVE, mutations: &[], mutation_parallelism: MutationParallelism::None(&[]),
};
static BATCH: MetaMutant<Map> = MetaMutant {
    cargo_package_name: None, cargo_target_kind: None, crate_name: "runtime_fixture",
    active_mutant_handle: &ACTIVE, mutations: &[&ABORT, &COLLATERAL],
    mutation_parallelism: MutationParallelism::Batched(&[BatchedMutantMeta {
        batch_id: 1, mutations: &[&ABORT, &COLLATERAL],
        substitutions: &[(0, SubstMeta { mutation: &ABORT }), (1, SubstMeta { mutation: &COLLATERAL })],
    }]),
};
static SINGLE: MetaMutant<Map> = MetaMutant {
    cargo_package_name: None, cargo_target_kind: None, crate_name: "runtime_fixture",
    active_mutant_handle: &ACTIVE, mutations: &[&ABORT],
    mutation_parallelism: MutationParallelism::None(&[StandaloneMutantMeta {
        mutation: &ABORT, substitutions: &[(0, SubstMeta { mutation: &ABORT })],
    }]),
};
static UNSAFE: MutationMeta = MutationMeta {
    id: 1, safety: MutationSafety::Unsafe, op_name: "fixture", display_name: "isolated abort",
    display_location: "fixture:1", undetected_diagnostic: "abort survived",
    reachable_from: EntryPoints::InternalTests(phf::phf_map! { "isolated" => 0usize }),
};
static UNSAFE_SOLO: StandaloneMutantMeta = StandaloneMutantMeta {
    mutation: &UNSAFE, substitutions: &[(0, SubstMeta { mutation: &UNSAFE })],
};
static UNSAFE_META: MetaMutant<Map> = MetaMutant {
    cargo_package_name: None, cargo_target_kind: None, crate_name: "runtime_fixture",
    active_mutant_handle: &ACTIVE, mutations: &[&UNSAFE],
    mutation_parallelism: MutationParallelism::None(&[StandaloneMutantMeta {
        mutation: &UNSAFE, substitutions: &[(0, SubstMeta { mutation: &UNSAFE })],
    }]),
};
static RENDEZVOUS: OnceLock<Barrier> = OnceLock::new();

fn mark(name: &str) {
    let root = PathBuf::from(env::var_os("MUTEST_REGRESSION_ROOT").unwrap());
    fs::write(root.join(name), process::id().to_string()).unwrap();
}

pub(crate) fn after_mutations() {
    if env::var("MUTEST_REGRESSION_SCENARIO").is_ok_and(|scenario| scenario == "finished-cancel") {
        mark("finished-before-cancel");
        loop { thread::park(); }
    }
}

fn completion_path() -> PathBuf {
    PathBuf::from(env::var_os("__MUTEST_COMPLETION_PATH").unwrap())
}

fn write_malformed_completion(scenario: &str) -> ! {
    let token = env::var("__MUTEST_COMPLETION_TOKEN").unwrap();
    let pid = process::id();
    let text = match scenario {
        "completion-partial" => format!("{token} {pid} 0"),
        "completion-duplicate" => format!("{token} {pid} 0\n{token} {pid} 0\n"),
        _ => format!("{token} {} 0\n", pid + 1),
    };
    fs::write(completion_path(), text).unwrap();
    process::exit(0);
}

fn replace_completion_with_fifo() -> ! {
    use std::os::unix::ffi::OsStrExt;
    let path = completion_path();
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: SIG_IGN is a valid disposition, used only in this fixture worker.
    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN); }
    fs::remove_file(&path).unwrap();
    // SAFETY: The owned scratch path is terminated by CString and is not retained by mkfifo.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    mark("fifo-replaced");
    loop { thread::park(); }
}

fn leave_an_orphan() -> ! {
    let mut command = process::Command::new(env::current_exe().unwrap());
    command.args(["--exact", "supervisor::tests::regressions::runtime_fixture_entry", "--test-threads=1"])
        .env("MUTEST_REGRESSION_ORPHAN_PARENT", "1");
    let mut child = command.spawn().unwrap();
    assert!(child.wait().unwrap().success());
    mark("orphan-parent-reaped");
    loop { thread::park(); }
}

fn resist_cancellation(scenario: &str) -> ! {
    extern "C" fn exit_zero(_: libc::c_int) {
        // SAFETY: `_exit` is async-signal-safe and terminates only this fixture worker.
        unsafe { libc::_exit(0) };
    }
    let handler = if scenario == "cancel-zero" { exit_zero as *const () as libc::sighandler_t } else { libc::SIG_IGN };
    // SAFETY: This handler is installed only in the disposable fixture worker.
    unsafe { assert_ne!(libc::signal(libc::SIGTERM, handler), libc::SIG_ERR); }
    mark("signal-ready");
    loop { thread::park(); }
}

fn reference() -> Result<(), String> {
    let scenario = env::var("MUTEST_REGRESSION_SCENARIO").unwrap();
    mark("reference-entered");
    let requested_exit = scenario.strip_prefix("exit-").and_then(|s| s.split('-').next()).and_then(|s| s.parse::<i32>().ok());
    match scenario.as_str() {
        "completion-partial" | "completion-duplicate" | "completion-wrong-worker" => write_malformed_completion(&scenario),
        "completion-fifo" => replace_completion_with_fifo(),
        "completion-write-failure" => {
            let path = completion_path();
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
        }
        "orphan-reaping" => leave_an_orphan(),
        _ if let Some(code) = requested_exit => process::exit(code),
        _ if scenario.starts_with("cancel-") => resist_cancellation(&scenario),
        _ => {}
    }
    Ok(())
}

fn completed_mutation() -> Result<(), String> {
    if ACTIVE.subst_at(0).is_some() {
        mark("mutation-entered");
        let scenario = env::var("MUTEST_REGRESSION_SCENARIO").unwrap();
        if scenario == "evaluate-timeout" { loop { thread::park(); } }
        // Slower than the first limit of about one second, but faster than the limit of the rerun.
        if scenario == "evaluate-slow" { thread::sleep(Duration::from_secs(2)); }
    }
    Ok(())
}

fn isolated_abort() -> Result<(), String> {
    mark("isolated-abort-entered");
    process::abort();
}

pub(crate) fn isolated_entry() {
    match env::var(test_runner::TEST_SUBPROCESS_INVOCATION).unwrap().as_str() {
        "abort" => mutest_isolated_worker(case("abort", aborting), &BATCH),
        "collateral" => mutest_isolated_worker(case("collateral", collateral), &BATCH),
        _ => mutest_isolated_worker(case("isolated", isolated_abort), &UNSAFE_META),
    }
}

/// Makes the isolated child of a fixture worker run only the fixture entry, not every test of this binary.
pub(crate) fn run_fixture_entry_only(cmd: &mut process::Command) {
    if env::var_os("MUTEST_REGRESSION_SCENARIO").is_some() {
        cmd.args(["--exact", "supervisor::tests::regressions::runtime_fixture_entry", "--test-threads=1", "--nocapture"]);
    }
}

fn isolated() -> bool {
    env::var_os(MUTEST_ISOLATED_WORKER_MUTATION_ID).is_some()
}

fn aborting() -> Result<(), String> {
    if ACTIVE.subst_at(0).is_some() && isolated() {
        mark("isolated-abort-entered");
        process::abort();
    }
    if ACTIVE.subst_at(0).is_some() {
        mark("abort-entered");
        let root = PathBuf::from(env::var_os("MUTEST_REGRESSION_ROOT").unwrap());
        let journal = env::var_os("__MUTEST_JOURNAL").expect("fixture journal missing");
        fs::copy(journal, root.join("journal-before-abort")).unwrap();
        RENDEZVOUS.get_or_init(|| Barrier::new(2)).wait();
        process::abort();
    }
    mark("abort-reference");
    Ok(())
}

fn collateral() -> Result<(), String> {
    if ACTIVE.subst_at(1).is_some() && isolated() {
        mark("isolated-collateral-entered");
        return Ok(());
    }
    if ACTIVE.subst_at(1).is_some() {
        mark("collateral-entered");
        RENDEZVOUS.get_or_init(|| Barrier::new(2)).wait();
        loop { thread::park(); }
    }
    mark("collateral-reference");
    Ok(())
}

fn case(name: &'static str, function: fn() -> Result<(), String>) -> test::TestDescAndFn {
    test::TestDescAndFn {
        desc: test::TestDesc {
            name: test::StaticTestName(name), ignore: false, ignore_message: None,
            source_file: file!(), start_line: 0, start_col: 0, end_line: 0, end_col: 0,
            should_panic: test::ShouldPanic::No, compile_fail: false, no_run: false,
            test_type: test::TestType::UnitTest,
        },
        testfn: test::TestFn::StaticTestFn(function),
    }
}

#[test]
fn finishing_monitor_waits_for_final_callback() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let (finished_tx, finished_rx) = mpsc::channel();
    let monitor = Arc::new(LingeringTestMonitoringThread::set_up(move |_| {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    }));
    let (control_tx, _) = mpsc::channel();
    monitor.submit_lingering_tests(vec![(test_runner::RunningTest {
        desc: case("reference", reference).desc, timeout: None, start_time: Instant::now(),
        control_tx, join_handle: None, active_signal: None,
    }, &ABORT)]);
    let finisher = thread::spawn(move || {
        LingeringTestMonitoringThread::finish(monitor);
        finished_tx.send(()).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(finished_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release_tx.send(()).unwrap();
    finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    finisher.join().unwrap();
}

pub(crate) fn run(scenario: &str) {
    let (tests, meta_mutant): (_, &'static MetaMutant<Map>) = match scenario {
        "collateral" => (vec![case("abort", aborting), case("collateral", collateral)], &BATCH),
        "unsafe-simulate" => (vec![case("isolated", isolated_abort)], &EMPTY),
        "evaluate-missed" | "evaluate-slow" | "evaluate-timeout" | "finished-output-failure" | "finished-cancel" => (vec![case("abort", completed_mutation)], &SINGLE),
        _ => (vec![case("reference", reference)], &EMPTY),
    };
    let mut args = if scenario.ends_with("-flakes") || scenario == "zero-flakes" { vec!["--flakes=2"] } else { vec![] };
    let output = format!("--metadata-out-root-dir={}/metadata", env::var("MUTEST_REGRESSION_ROOT").unwrap());
    if scenario == "output-order" {
        fs::create_dir(PathBuf::from(env::var_os("MUTEST_REGRESSION_ROOT").unwrap()).join("metadata")).unwrap();
        args.extend([output.as_str(), "--Zwrite-json-eval-stream"]);
    }
    if scenario == "finished-output-failure" { args.push(&output); }
    match scenario {
        "unsafe-simulate" => mutest_simulate_main(&[], tests, &UNSAFE_SOLO, &ACTIVE),
        _ if scenario.ends_with("-simulate") => mutest_simulate_main(&[], tests, &SOLO, &ACTIVE),
        _ => mutest_main(&args, tests, None, meta_mutant),
    }
    mark("harness-returned");
    if scenario == "late-stop" { loop { thread::park(); } }
}
