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
use std::hash::{BuildHasher, RandomState};
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
    /// A new journal in the temporary directory, readable by this user alone. Others can write there
    /// too, and a file or link they put where the journal goes would be opened in its place, and
    /// truncated: so the journal goes under a name no one can guess, and only where nothing is yet.
    pub fn create() -> io::Result<Self> {
        let random = RandomState::new();
        let names = (0..8).map(|attempt| format!("mutest-journal-{}-{:016x}", process::id(), random.hash_one(attempt)));
        Self::create_in(&env::temp_dir(), names)
    }

    /// Under the first of the names that nothing is at yet.
    fn create_in(dir: &Path, names: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);

        let mut taken = io::Error::new(io::ErrorKind::AlreadyExists, "every name for the mutation journal is taken");
        for name in names {
            let path = dir.join(name);
            match options.open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => taken = err,
                Err(err) => return Err(err),
            }
        }
        Err(taken)
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
    path: PathBuf,
    file: Mutex<Option<File>>,
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
        WorkerJournal::open(PathBuf::from(path))
    });
    let _ = WORKER_JOURNAL.set(journal);
}

/// The journal of this worker, if a supervisor gave it one.
pub fn worker() -> Option<&'static WorkerJournal> {
    WORKER_JOURNAL.get().and_then(Option::as_ref)
}

impl WorkerJournal {
    fn open(path: PathBuf) -> Option<Self> {
        let Entries { started: _, finished, crashed } = read(&path);
        let file = OpenOptions::new().append(true).open(&path).ok()?;
        Some(Self { path, file: Mutex::new(Some(file)), finished, crashed })
    }

    /// Once a line cannot be written, as on a full disk, the journal is removed rather than left
    /// without what this worker goes on to do: from it, the supervisor would count mutations that
    /// finished as crashed. Without a journal the run goes on, and a crash ends it.
    fn append(&self, line: &Value) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let Some(journal_file) = file.as_mut() else { return; };
        if let Err(err) = append(journal_file, line) {
            *file = None;
            let _ = fs::remove_file(&self.path);
            println!("cannot write to the mutation journal: {err}: a mutation that crashes the test harness now ends the run");
        }
    }

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
        self.append(&json!({ "started": mutation_ids }));
    }

    pub fn finished(&self, mutation_id: u32, results: &MutationTestResults) {
        let tests = results.results_per_test.iter()
            .map(|(name, result)| json!([name.as_slice(), result.map(result_name)]))
            .collect::<Vec<_>>();
        self.append(&json!({ "finished": mutation_id, "result": result_name(results.result), "tests": tests }));
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::{Journal, WorkerJournal};

    #[test]
    fn a_worker_that_cannot_write_its_journal_goes_on_without_one() {
        let journal = Journal::create().unwrap();
        journal.record_crashed(&[1]).unwrap();
        // Writing through a handle opened for reading fails, as it does on a full disk.
        let worker_journal = WorkerJournal::open(journal.path.clone()).unwrap();
        *worker_journal.file.lock().unwrap() = Some(File::open(&journal.path).unwrap());

        worker_journal.started(&[2]);

        // Mutation 2 is not taken for crashed should the worker now crash on another.
        assert_eq!(journal.unfinished(), [] as [u32; 0]);
        worker_journal.started(&[3]);
        assert_eq!(journal.unfinished(), [] as [u32; 0]);
        // The journal is gone, not left with some of what this worker did and not the rest.
        assert!(!journal.path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_journal_is_never_opened_through_a_file_already_at_its_path() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("mutest-journal-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target");
        fs::write(&target, "kept").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("taken")).unwrap();

        let journal = Journal::create_in(&dir, ["taken".to_owned(), "free".to_owned()]).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "kept");
        assert_eq!(journal.path, dir.join("free"));
        assert_eq!(fs::metadata(&journal.path).unwrap().permissions().mode() & 0o777, 0o600);
        drop(journal);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn journals_are_created_under_names_of_their_own() {
        let first = Journal::create().unwrap();
        let second = Journal::create().unwrap();

        assert_ne!(first.path, second.path);
        assert!(first.path.exists() && second.path.exists());
    }
}
