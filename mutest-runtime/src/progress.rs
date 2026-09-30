//! Progress records for a caller watching the run; `docs/runtime-progress.md` describes the protocol.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{Value, json};

use super::{CompletedTest, Test, TestResult, subprocess, test};

const DIRECTORY: &str = "MUTEST_PROGRESS_DIR";
const NONCE: &str = "MUTEST_PROGRESS_NONCE";
const RECORD_LIMIT: usize = 16 * 1024;
const FILE_LIMIT: u64 = 64 * 1024 * 1024;
static WRITER: OnceLock<Option<Mutex<Writer>>> = OnceLock::new();

struct Writer {
    file: File,
    nonce: String,
    harness: String,
    start: Instant,
    sequence: u64,
    bytes: u64,
    invocation: u64,
    active: HashSet<u64>,
    phase: Option<&'static str>,
    terminal: bool,
    healthy: bool,
}

impl Writer {
    /// Writes one record; after any failure the writer refuses every later one.
    fn append(&mut self, event: Value) -> io::Result<()> {
        if !self.healthy || self.terminal {
            return Err(io::Error::other("progress writer is closed or failed"));
        }
        let result = self.write_record(event);
        self.healthy = result.is_ok();
        result
    }

    fn write_record(&mut self, mut event: Value) -> io::Result<()> {
        let elapsed = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let object = event.as_object_mut().ok_or_else(|| io::Error::other("progress event is not an object"))?;
        object.extend([
            ("schema".to_owned(), json!("mutest-progress")),
            ("version".to_owned(), json!(1)),
            ("nonce".to_owned(), json!(self.nonce)),
            ("instance_id".to_owned(), json!(self.harness)),
            ("seq".to_owned(), json!(self.sequence)),
            ("elapsed_ms".to_owned(), json!(elapsed)),
        ]);
        let mut record = serde_json::to_vec(&event).map_err(io::Error::other)?;
        record.push(b'\n');
        if record.len() > RECORD_LIMIT {
            return Err(io::Error::other("progress record limit exceeded"));
        }
        let bytes = self.bytes + record.len() as u64;
        if bytes > FILE_LIMIT {
            return Err(io::Error::other("progress file limit exceeded"));
        }
        self.file.write_all(&record)?;
        self.sequence += 1;
        self.bytes = bytes;
        Ok(())
    }

    fn set_phase(&mut self, phase: &'static str) -> io::Result<()> {
        if self.phase == Some(phase) {
            return Ok(());
        }
        if !self.active.is_empty() {
            return Err(io::Error::other("progress phase changed with active tests"));
        }
        self.append(json!({"event": "phase", "phase": phase}))?;
        self.phase = Some(phase);
        Ok(())
    }

    fn start_test(&mut self, test: &Test, mutations: &[u32], isolated: bool) -> io::Result<u64> {
        let invocation = self.invocation + 1;
        let timeout = test.timeout.map(|timeout| u64::try_from(timeout.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX));
        self.append(json!({"event": "test_start", "phase": self.phase, "invocation_id": invocation, "test_name": test.desc.name.as_slice(),
            "mutation_ids": mutations, "strategy": if isolated { "isolated" } else { "in_process" }, "execution_timeout_ms": timeout,
            "startup_timeout_ms": if isolated { subprocess::STARTUP_TIMEOUT.as_millis() } else { 0 },
            "cleanup_timeout_ms": if isolated { subprocess::CLEANUP_TIMEOUT.as_millis() } else { 0 },
            "report_timeout_ms": if isolated { subprocess::REPORT_TIMEOUT.as_millis() } else { 0 },
            "join_timeout_ms": if isolated { subprocess::REPORT_TIMEOUT.as_millis() } else { 0 }}))?;
        self.invocation = invocation;
        self.active.insert(invocation);
        Ok(invocation)
    }

    fn end_test(&mut self, invocation: u64, result: &TestResult, complete: bool) -> io::Result<()> {
        if !self.active.contains(&invocation) {
            return Err(io::Error::other(
                "progress completion has no active invocation",
            ));
        }
        let result = match result {
            TestResult::Ok => "ok",
            TestResult::Failed | TestResult::FailedMsg(_) => "failed",
            TestResult::Ignored => "ignored",
            TestResult::TimedOut => "timed_out",
            TestResult::CrashedMsg(_) => "crashed",
        };
        self.append(
            json!({"event": "test_end", "invocation_id": invocation, "result": result,
            "cleanup": if complete { "complete" } else { "pending" }}),
        )?;
        if complete {
            self.active.remove(&invocation);
        }
        Ok(())
    }

    fn ready(&self) -> io::Result<()> {
        if !self.healthy || self.terminal || !self.active.is_empty() {
            return Err(io::Error::other(
                "progress contains incomplete tests or failed output",
            ));
        }
        Ok(())
    }

    fn finish(&mut self, complete: bool, code: i32) -> io::Result<()> {
        if complete {
            self.ready()?;
        }
        self.append(json!({"event": "terminal", "status": if complete { "completed" } else { "incomplete" }, "exit_code": code}))?;
        self.terminal = true;
        Ok(())
    }
}

fn configuration(
    directory: Option<PathBuf>,
    nonce: Option<String>,
) -> io::Result<Option<(PathBuf, String)>> {
    match (directory, nonce) {
        (None, None) => Ok(None),
        (Some(directory), Some(nonce))
            if directory.is_absolute()
                && nonce.len() == 32
                && nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            Ok(Some((directory, nonce)))
        }
        _ => Err(io::Error::other(
            "progress requires an absolute directory and a 32-character lowercase hex nonce",
        )),
    }
}

/// This process's start time in clock ticks since boot, which with its id tells it apart from a later process.
#[cfg(target_os = "linux")]
fn process_start_ticks() -> io::Result<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|ticks| ticks.parse().ok())
        .ok_or_else(|| io::Error::other("process start time unavailable"))
}

#[cfg(target_os = "linux")]
fn create(directory: &Path, nonce: String) -> io::Result<Writer> {
    use std::hash::{BuildHasher, RandomState};
    use std::os::unix::fs::OpenOptionsExt;

    let ticks = process_start_ticks()?;
    let executable = std::fs::canonicalize(env::current_exe()?)?;
    let executable = executable.to_str().ok_or_else(|| io::Error::other("executable path is not UTF-8"))?;
    let random = RandomState::new();
    let harness = format!("{:016x}{:016x}", random.hash_one(0), random.hash_one(1));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(format!("{nonce}-{}-{harness}.jsonl", process::id())))?;
    let mut writer = Writer {
        file,
        nonce,
        harness,
        start: Instant::now(),
        sequence: 0,
        bytes: 0,
        invocation: 0,
        active: HashSet::new(),
        phase: None,
        terminal: false,
        healthy: true,
    };
    writer.append(json!({"event": "header", "pid": process::id(), "process_start": {"kind": "linux-proc-starttime", "ticks": ticks},
        "exe": executable, "record_limit_bytes": RECORD_LIMIT, "file_limit_bytes": FILE_LIMIT}))?;
    Ok(writer)
}

#[cfg(not(target_os = "linux"))]
fn create(_directory: &Path, _nonce: String) -> io::Result<Writer> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "progress records a process start time only on Linux"))
}

pub(crate) fn initialize() {
    WRITER.get_or_init(|| {
        let directory = env::var_os(DIRECTORY).map(PathBuf::from);
        let nonce = env::var_os(NONCE);
        if directory.is_none() && nonce.is_none() {
            return None;
        }
        // SAFETY: The harness initializes progress before starting threads or invoking tested code.
        unsafe {
            env::remove_var(DIRECTORY);
            env::remove_var(NONCE);
        }
        let configured = nonce
            .map(|nonce| {
                nonce
                    .into_string()
                    .map_err(|_| io::Error::other("non-UTF-8 progress nonce"))
            })
            .transpose()
            .and_then(|nonce| configuration(directory, nonce))
            .and_then(|config| config.map(|(dir, nonce)| create(&dir, nonce)).transpose());
        match configured {
            Ok(writer) => writer.map(Mutex::new),
            Err(error) => {
                eprintln!("mutation analysis incomplete: progress initialization: {error}");
                process::exit(101);
            }
        }
    });
}

fn with_writer<T>(action: impl FnOnce(&mut Writer) -> io::Result<T>) -> io::Result<Option<T>> {
    let Some(Some(writer)) = WRITER.get() else {
        return Ok(None);
    };
    let mut writer = writer
        .lock()
        .map_err(|_| io::Error::other("progress writer lock poisoned"))?;
    action(&mut writer).map(Some)
}

pub(crate) fn fail(error: impl std::fmt::Display) -> ! {
    let _ = terminal(false, 101);
    eprintln!("mutation analysis incomplete: progress: {error}");
    process::exit(101)
}

fn require_enabled(required: bool, enabled: bool) -> io::Result<()> {
    if required && !enabled {
        return Err(io::Error::other(
            "--require-progress needs enabled progress output",
        ));
    }
    Ok(())
}

pub(crate) fn require(required: bool) {
    if let Err(error) = require_enabled(required, matches!(WRITER.get(), Some(Some(_)))) {
        fail(error);
    }
}

#[cfg(test)]
#[test]
fn required_progress_cannot_run_disabled() {
    assert!(require_enabled(false, false).is_ok());
    assert!(require_enabled(false, true).is_ok());
    assert!(require_enabled(true, true).is_ok());
    assert!(require_enabled(true, false).is_err());
}

pub(crate) fn ready() -> io::Result<()> {
    with_writer(|writer| writer.ready()).map(drop)
}
pub(crate) fn terminal(complete: bool, code: i32) -> io::Result<()> {
    with_writer(|writer| writer.finish(complete, code)).map(drop)
}

pub(crate) struct Context<'a> {
    pub phase: &'static str,
    pub mutation_ids: &'a dyn Fn(&test::TestDesc) -> Vec<u32>,
}

#[derive(Default)]
struct Run {
    mutations: HashMap<String, Vec<u32>>,
    invocations: HashMap<test::TestId, u64>,
}
thread_local! { static RUN: RefCell<Option<Run>> = const { RefCell::new(None) }; }
pub(crate) struct Scope(Option<Option<Run>>);
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            RUN.with(|run| *run.borrow_mut() = previous);
        }
    }
}

pub(crate) fn enter(context: Context<'_>, tests: &[Test]) -> Scope {
    if !matches!(WRITER.get(), Some(Some(_))) {
        return Scope(None);
    }
    if let Err(error) = with_writer(|writer| writer.set_phase(context.phase)) {
        fail(error);
    }
    let mutations = tests
        .iter()
        .map(|test| {
            (
                test.desc.name.as_slice().to_owned(),
                (context.mutation_ids)(&test.desc),
            )
        })
        .collect();
    Scope(Some(RUN.with(|run| {
        run.replace(Some(Run {
            mutations,
            invocations: HashMap::new(),
        }))
    })))
}

pub(super) fn start(id: test::TestId, test: &Test, isolated: bool) {
    RUN.with(|run| {
        let mut run = run.borrow_mut();
        let Some(run) = run.as_mut() else {
            return;
        };
        let mutations = run
            .mutations
            .get(test.desc.name.as_slice())
            .map(Vec::as_slice)
            .unwrap_or_default();
        match with_writer(|writer| writer.start_test(test, mutations, isolated)) {
            Ok(Some(invocation)) => {
                run.invocations.insert(id, invocation);
            }
            Ok(None) => {}
            Err(error) => fail(error),
        }
    });
}

pub(super) fn end(test: &CompletedTest, complete: bool) {
    RUN.with(|run| {
        let mut run = run.borrow_mut();
        let Some(run) = run.as_mut() else {
            return;
        };
        let Some(invocation) = run.invocations.remove(&test.id) else {
            return;
        };
        if let Err(error) =
            with_writer(|writer| writer.end_test(invocation, &test.result, complete))
        {
            fail(error);
        }
    });
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> (PathBuf, Writer) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../test-scratch")
            .join(format!(
                "progress-{}-{}",
                process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let writer = create(&root, "0123456789abcdef0123456789abcdef".to_owned()).unwrap();
        (root, writer)
    }

    #[test]
    fn disabled_and_invalid_configuration_do_not_open_paths() {
        assert_eq!(configuration(None, None).unwrap(), None);
        for (directory, nonce) in [
            (Some(PathBuf::from("/missing")), None),
            (None, Some("a".repeat(32))),
            (Some(PathBuf::from("relative")), Some("a".repeat(32))),
            (Some(PathBuf::from("/missing")), Some("A".repeat(32))),
        ] {
            assert!(configuration(directory, nonce).is_err());
        }
    }

    #[test]
    fn ordered_records_preserve_out_of_order_completion_and_pending_cleanup() {
        let (root, mut writer) = fixture();
        writer.set_phase("evaluation").unwrap();
        let mut test = super::super::tests::descriptor(Some(std::time::Duration::from_micros(1)));
        let first = writer.start_test(&test, &[7], true).unwrap();
        let second = writer.start_test(&test, &[8], true).unwrap();
        writer.end_test(second, &TestResult::Failed, true).unwrap();
        assert!(writer.ready().is_err());
        writer.end_test(first, &TestResult::Ok, true).unwrap();
        writer.set_phase("simulation").unwrap();
        test.timeout = None;
        let pending = writer.start_test(&test, &[9], false).unwrap();
        writer
            .end_test(pending, &TestResult::TimedOut, false)
            .unwrap();
        assert!(writer.finish(true, 0).is_err());
        writer.finish(false, 101).unwrap();
        let path = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        let records = fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        for (sequence, record) in records.iter().enumerate() {
            assert_eq!(record["seq"], sequence as u64);
            assert_eq!(record["nonce"], "0123456789abcdef0123456789abcdef");
        }
        assert!(
            records
                .windows(2)
                .all(|records| records[0]["elapsed_ms"].as_u64()
                    <= records[1]["elapsed_ms"].as_u64())
        );
        assert_eq!(records[2]["execution_timeout_ms"], 1);
        assert_eq!(records[4]["invocation_id"], second);
        assert_eq!(records[7]["execution_timeout_ms"], Value::Null);
        assert_eq!(records.last().unwrap()["status"], "incomplete");
        assert_eq!(records.last().unwrap()["exit_code"], 101);
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_record_or_file_over_its_limit_fails_the_writer() {
        for case in ["record", "file"] {
            let (root, mut writer) = fixture();
            match case {
                "record" => assert!(writer.append(json!({"oversized": "x".repeat(RECORD_LIMIT)})).is_err()),
                _ => {
                    writer.bytes = FILE_LIMIT;
                    assert!(writer.set_phase("reference").is_err());
                }
            }
            assert!(writer.ready().is_err());
            assert!(writer.finish(true, 0).is_err());
            drop(writer);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
