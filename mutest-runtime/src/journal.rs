//! What the worker has evaluated so far, in a file the supervisor gave it. When a mutation takes the
//! worker down, the supervisor reads which mutations were started and never finished, records them
//! as crashed, and starts a new worker. That worker carries every recorded result over rather than
//! evaluating the mutation again, and goes on with the rest.
//!
//! One JSON object per line, appended and flushed as it happens:
//! `{"started":[ids]}`, `{"finished":id,"result":"detected","tests":[[name,result|null],...]}`,
//! and, written by the supervisor, `{"crashed":[ids]}`. A line cut short by a crash is skipped.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use crate::harness::{MutationTestResult, MutationTestResults};
use crate::test_runner;

/// Set in the worker's environment, to the path of the journal.
const JOURNAL_VAR: &str = "__MUTEST_JOURNAL";

fn result_name(result: MutationTestResult) -> &'static str {
    match result {
        MutationTestResult::Undetected => "undetected",
        MutationTestResult::Detected => "detected",
        MutationTestResult::TimedOut => "timed_out",
        MutationTestResult::Crashed => "crashed",
    }
}

fn result_from_name(name: &str) -> Option<MutationTestResult> {
    match name {
        "undetected" => Some(MutationTestResult::Undetected),
        "detected" => Some(MutationTestResult::Detected),
        "timed_out" => Some(MutationTestResult::TimedOut),
        "crashed" => Some(MutationTestResult::Crashed),
        _ => None,
    }
}

fn append(file: &mut File, line: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(line).map_err(io::Error::other)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.flush()
}

/// A finished mutation's result, with the result of each test that ran against it, by test name.
struct CarriedResult {
    result: MutationTestResult,
    tests: Vec<(String, Option<MutationTestResult>)>,
}

#[derive(Default)]
struct Entries {
    started: BTreeSet<u32>,
    finished: HashMap<u32, CarriedResult>,
    crashed: BTreeSet<u32>,
}

fn read(path: &Path) -> Entries {
    let mut entries = Entries::default();
    let Ok(file) = File::open(path) else { return entries; };
    let ids = |value: &Value| value.as_array().into_iter().flatten().filter_map(|id| id.as_u64()).map(|id| id as u32).collect::<Vec<_>>();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break; };
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue; };
        if let Some(started) = value.get("started") {
            entries.started.extend(ids(started));
        } else if let Some(crashed) = value.get("crashed") {
            entries.crashed.extend(ids(crashed));
        } else if let Some(id) = value.get("finished").and_then(Value::as_u64) {
            let Some(result) = value.get("result").and_then(Value::as_str).and_then(result_from_name) else { continue; };
            let tests = value.get("tests").and_then(Value::as_array).into_iter().flatten()
                .filter_map(|test| {
                    let [name, result] = test.as_array()?.as_slice() else { return None; };
                    Some((name.as_str()?.to_owned(), result.as_str().and_then(result_from_name)))
                })
                .collect();
            entries.finished.insert(id as u32, CarriedResult { result, tests });
        }
    }
    entries
}

/// The supervisor's side.
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    pub fn create() -> io::Result<Self> {
        let path = env::temp_dir().join(format!("mutest-journal-{}", process::id()));
        File::create(&path)?;
        Ok(Self { path })
    }

    pub fn pass_to(&self, worker: &mut process::Command) {
        worker.env(JOURNAL_VAR, &self.path);
    }

    /// The mutations the worker started and never finished, other than those already crashed.
    pub fn unfinished(&self) -> Vec<u32> {
        let entries = read(&self.path);
        entries.started.into_iter()
            .filter(|id| !entries.finished.contains_key(id) && !entries.crashed.contains(id))
            .collect()
    }

    pub fn record_crashed(&self, mutation_ids: &[u32]) -> io::Result<()> {
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        append(&mut file, &json!({ "crashed": mutation_ids }))
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The worker's side.
pub struct WorkerJournal {
    file: Mutex<File>,
    finished: HashMap<u32, CarriedResult>,
    crashed: BTreeSet<u32>,
}

static WORKER_JOURNAL: OnceLock<Option<WorkerJournal>> = OnceLock::new();

/// Opens the journal the supervisor passed, reading what earlier workers recorded. It must be
/// called before any thread is started: it removes the variable, so that no test inherits it.
pub fn open_for_worker() {
    let journal = env::var_os(JOURNAL_VAR).and_then(|path| {
        // SAFETY: No other thread is running yet.
        unsafe { env::remove_var(JOURNAL_VAR) };
        let path = PathBuf::from(path);
        let Entries { started: _, finished, crashed } = read(&path);
        let file = OpenOptions::new().append(true).open(&path).ok()?;
        Some(WorkerJournal { file: Mutex::new(file), finished, crashed })
    });
    let _ = WORKER_JOURNAL.set(journal);
}

/// The journal of this worker, if a supervisor gave it one.
pub fn worker() -> Option<&'static WorkerJournal> {
    WORKER_JOURNAL.get().and_then(Option::as_ref)
}

impl WorkerJournal {
    /// What an earlier worker recorded for the mutation, if anything, against these tests.
    pub fn carried(&self, mutation_id: u32, tests: &[test_runner::Test]) -> Option<MutationTestResults> {
        if self.crashed.contains(&mutation_id) {
            return Some(MutationTestResults { result: MutationTestResult::Crashed, results_per_test: HashMap::new() });
        }
        let carried = self.finished.get(&mutation_id)?;
        Some(MutationTestResults {
            result: carried.result,
            results_per_test: carried.tests.iter()
                .filter_map(|(name, result)| {
                    let test = tests.iter().find(|test| test.desc.name.as_slice() == name)?;
                    Some((test.desc.name.clone(), *result))
                })
                .collect(),
        })
    }

    pub fn started(&self, mutation_ids: &[u32]) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        append(&mut file, &json!({ "started": mutation_ids })).expect("cannot write to the mutation journal");
    }

    pub fn finished(&self, mutation_id: u32, results: &MutationTestResults) {
        let tests = results.results_per_test.iter()
            .map(|(name, result)| json!([name.as_slice(), result.map(result_name)]))
            .collect::<Vec<_>>();
        let line = json!({ "finished": mutation_id, "result": result_name(results.result), "tests": tests });
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        append(&mut file, &line).expect("cannot write to the mutation journal");
    }
}
