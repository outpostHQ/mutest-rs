//! The mutations a worker started and finished, so that a crash can name those left unfinished,
//! and a restarted worker can skip those finished and isolate those that crashed the worker before.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Mutex, OnceLock};

use crate::config::MutationIsolation;
use crate::harness::{MutationAnalysisResults, MutationTestResult, MutationTestResults};
use crate::json;
use crate::metadata::MutationMeta;
use crate::test_runner;

pub(crate) const JOURNAL_VAR: &str = "__MUTEST_JOURNAL";

fn result_name(result: MutationTestResult) -> &'static str {
    match result {
        MutationTestResult::Undetected => "undetected",
        MutationTestResult::Detected => "detected",
        MutationTestResult::TimedOut => "timed_out",
        MutationTestResult::Crashed => "crashed",
    }
}

/// The results `result_name` writes, so that a row naming any other is refused.
#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecordedResult {
    Undetected,
    Detected,
    TimedOut,
    Crashed,
}

impl From<RecordedResult> for MutationTestResult {
    fn from(result: RecordedResult) -> Self {
        match result {
            RecordedResult::Undetected => Self::Undetected,
            RecordedResult::Detected => Self::Detected,
            RecordedResult::TimedOut => Self::TimedOut,
            RecordedResult::Crashed => Self::Crashed,
        }
    }
}

/// The worker writes the `started` and `finished` rows; the supervisor writes `isolate` before it restarts a crashed worker.
#[derive(serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Record {
    Started {
        started: Vec<u32>,
    },
    Finished {
        finished: u32,
        result: RecordedResult,
        tests: Vec<(String, Option<RecordedResult>)>,
        #[serde(default)]
        timeout_rerun: bool,
    },
    Isolate {
        isolate: Vec<u32>,
    },
}

struct Finished {
    result: RecordedResult,
    tests: Vec<(String, Option<RecordedResult>)>,
    timeout_rerun: bool,
}

fn append(mut file: impl Write, line: json::Object) -> io::Result<()> {
    let mut bytes = line.into_vec()?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.flush()
}

#[derive(Default)]
struct Entries {
    started: BTreeSet<u32>,
    finished: BTreeMap<u32, Finished>,
    isolated: BTreeSet<u32>,
}

impl Entries {
    fn add(&mut self, record: Record) -> io::Result<()> {
        match record {
            Record::Started { started } => self.started.extend(started),
            Record::Finished { finished, result, tests, timeout_rerun } => {
                if self.finished.insert(finished, Finished { result, tests, timeout_rerun }).is_some() {
                    return Err(io::Error::other("duplicate finished journal result"));
                }
            }
            Record::Isolate { isolate } => self.isolated.extend(isolate),
        }
        Ok(())
    }

    fn unfinished(&self) -> impl Iterator<Item = u32> {
        self.started.iter().copied().filter(|id| !self.finished.contains_key(id))
    }
}

/// Any row that cannot be read makes the whole journal unreadable, never a shorter one.
fn read(mut file: &File) -> io::Result<Entries> {
    file.seek(SeekFrom::Start(0))?;
    let mut entries = Entries::default();
    for line in BufReader::new(file).lines() {
        entries.add(json::from_str(&line?)?)?;
    }
    Ok(entries)
}

/// What the journal holds after a worker crashed.
pub struct Crash {
    /// The unfinished mutations that no worker ran in isolation before, now marked for isolation.
    pub isolated: Vec<u32>,
    pub unfinished: usize,
    pub finished: usize,
}

pub struct Journal {
    path: PathBuf,
    file: File,
    retained: bool,
}

impl Journal {
    /// Creates the journal under a new name in the temporary directory; on Unix only its owner can read it.
    pub fn create() -> io::Result<Self> {
        let random = RandomState::new();
        let names = (0..8).map(|attempt| {
            format!(
                "mutest-journal-{}-{:016x}",
                process::id(),
                random.hash_one(attempt)
            )
        });
        Self::create_in(&env::temp_dir(), names)
    }

    fn create_in(dir: &Path, names: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);

        let mut taken = io::Error::new(
            io::ErrorKind::AlreadyExists,
            "every name for the mutation journal is taken",
        );
        for name in names {
            let path = dir.join(name);
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        retained: false,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => taken = err,
                Err(err) => return Err(err),
            }
        }
        Err(taken)
    }

    pub fn pass_to(&self, worker: &mut process::Command) {
        worker.env(JOURNAL_VAR, &self.path);
    }

    pub fn unfinished(&self) -> io::Result<Vec<u32>> {
        Ok(read(&self.file)?.unfinished().collect())
    }

    /// Records the unfinished mutations that are not isolated yet as isolated, for the next worker.
    pub fn isolate_unfinished(&self) -> io::Result<Crash> {
        let entries = read(&self.file)?;
        let isolated = entries.unfinished().filter(|id| !entries.isolated.contains(id)).collect::<Vec<_>>();
        if !isolated.is_empty() {
            let mut file = &self.file;
            file.seek(SeekFrom::End(0))?;
            append(file, json::Object::new().field("isolate", &isolated))?;
        }
        Ok(Crash { isolated, unfinished: entries.unfinished().count(), finished: entries.finished.len() })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Whether no worker wrote a row, so that the journal holds nothing to examine.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.file.metadata()?.len() == 0)
    }

    pub(crate) fn preserve(mut self) {
        self.retained = true;
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        if !self.retained {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct WorkerJournal {
    path: PathBuf,
    file: Mutex<Option<File>>,
    /// The rows of the workers before this one, which crashed.
    earlier: Entries,
}

static WORKER_JOURNAL: OnceLock<Option<WorkerJournal>> = OnceLock::new();

/// Must be called before any thread starts.
pub fn open_for_worker(path: Option<OsString>) {
    let journal = path.map(|path| {
        WorkerJournal::open(PathBuf::from(path)).unwrap_or_else(|| {
            eprintln!("cannot open supplied mutation journal");
            process::exit(mutest_exit_code::PANIC);
        })
    });
    let _ = WORKER_JOURNAL.set(journal);
}

pub fn worker() -> Option<&'static WorkerJournal> {
    WORKER_JOURNAL.get().and_then(Option::as_ref)
}

/// Whether an earlier worker finished `mutation`.
pub(crate) fn finished_before(journal: Option<&WorkerJournal>, mutation: &MutationMeta) -> bool {
    journal.is_some_and(|journal| journal.earlier.finished.contains_key(&mutation.id))
}

/// Whether the tests of `mutations` run each in a process of their own: as `isolation` asks, or because they crashed an earlier worker.
pub(crate) fn isolates(journal: Option<&WorkerJournal>, isolation: MutationIsolation, mutations: &[&MutationMeta]) -> bool {
    journal.is_some_and(|journal| mutations.iter().any(|mutation| journal.earlier.isolated.contains(&mutation.id)))
        || isolation.isolates(mutations)
}

/// Records the results of the mutations that the crashed workers before this one finished.
pub(crate) fn record_earlier_results(journal: Option<&WorkerJournal>, results: &mut MutationAnalysisResults, mutations: &[&'static MutationMeta], tests: &[test_runner::Test]) {
    let Some(journal) = journal else { return; };
    for (mutation, mutation_result) in journal.earlier_results(mutations, tests) {
        if journal.earlier.finished[&mutation.id].timeout_rerun {
            results.timeout_reruns.record(mutation_result.result);
        }
        results.record_mutation_results(mutation, mutation_result);
    }
}

impl WorkerJournal {
    fn open(path: PathBuf) -> Option<Self> {
        let file = OpenOptions::new().read(true).append(true).open(&path).ok()?;
        let earlier = read(&file).ok()?;
        Some(Self { path, file: Mutex::new(Some(file)), earlier })
    }

    /// Whether this worker takes over the analysis from a worker that crashed.
    pub(crate) fn resumes(&self) -> bool {
        !self.earlier.started.is_empty()
    }

    /// The results of the earlier workers, with the names of `tests`, so that this worker does not run those mutations again.
    pub(crate) fn earlier_results<'m>(&self, mutations: &[&'m MutationMeta], tests: &[test_runner::Test]) -> Vec<(&'m MutationMeta, MutationTestResults)> {
        let names = tests.iter().map(|test| (test.desc.name.as_slice(), &test.desc.name)).collect::<HashMap<_, _>>();
        let test_name = |name: &String| names.get(name.as_str()).map_or_else(|| test::DynTestName(name.clone()), |&name| name.clone());
        mutations.iter()
            .filter_map(|&mutation| Some((mutation, self.earlier.finished.get(&mutation.id)?)))
            .map(|(mutation, finished)| (mutation, MutationTestResults {
                result: finished.result.into(),
                results_per_test: finished.tests.iter().map(|(name, result)| (test_name(name), result.map(Into::into))).collect(),
            }))
            .collect()
    }

    /// Stops recording after a failed write, but keeps the earlier rows for diagnosing the run.
    fn append(&self, line: json::Object) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let Some(journal_file) = file.as_mut() else {
            return;
        };
        if let Err(err) = append(journal_file, line) {
            *file = None;
            eprintln!(
                "cannot write to the mutation journal: {err}; earlier results retained at {}",
                self.path.display()
            );
        }
    }

    pub(crate) fn healthy(&self) -> bool {
        self.file.lock().map(|file| file.is_some()).unwrap_or(false)
    }

    pub fn started(&self, mutation_ids: &[u32]) {
        self.append(json::Object::new().field("started", mutation_ids));
    }

    pub fn finished(&self, mutation_id: u32, results: &MutationTestResults, timeout_rerun: bool) {
        let tests = results
            .results_per_test
            .iter()
            .map(|(name, result)| (name.as_slice(), result.map(result_name)))
            .collect::<Vec<_>>();
        let line = json::Object::new().field("finished", &mutation_id).field("result", result_name(results.result)).field("tests", &tests);
        self.append(if timeout_rerun { line.field("timeout_rerun", &true) } else { line });
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn open_fixture_worker() {
    let path = env::var_os(JOURNAL_VAR)
        .map(PathBuf::from)
        .expect("fixture journal not supplied");
    let opened = WorkerJournal::open(path).expect("fixture journal could not be opened");
    if env::var("MUTEST_REGRESSION_SCENARIO")
        .is_ok_and(|scenario| scenario.ends_with("-journal-loss"))
    {
        *opened.file.lock().unwrap() = Some(File::open(&opened.path).unwrap());
        opened.started(&[99]);
        assert!(
            !opened.healthy(),
            "fixture failed to reject unwritable journal"
        );
        assert!(
            opened.path.exists(),
            "failed append removed earlier evidence"
        );
        let root = PathBuf::from(env::var_os("MUTEST_REGRESSION_ROOT").unwrap());
        fs::write(
            root.join("journal-write-failed"),
            b"read-only handle rejected append; journal retained",
        )
        .unwrap();
    }
    assert!(
        WORKER_JOURNAL.set(Some(opened)).is_ok(),
        "fixture journal opened twice"
    );
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::{Journal, WorkerJournal};

    #[test]
    fn failed_append_retains_prior_results_and_prevents_completion() {
        let journal = Journal::create().unwrap();
        let worker_journal = WorkerJournal::open(journal.path.clone()).unwrap();
        worker_journal.started(&[1]);
        // Writing through a handle opened for reading fails, as it does on a full disk.
        *worker_journal.file.lock().unwrap() = Some(File::open(&journal.path).unwrap());

        worker_journal.started(&[2]);

        assert!(!worker_journal.healthy());
        worker_journal.started(&[3]);
        assert_eq!(journal.unfinished().unwrap(), [1]);
        assert_eq!(std::fs::read_to_string(&journal.path).unwrap(), "{\"started\":[1]}\n");
    }

    #[test]
    fn each_crashed_mutation_is_isolated_once_and_a_restart_keeps_the_finished_results() {
        let journal = Journal::create().unwrap();
        let worker_journal = WorkerJournal::open(journal.path.clone()).unwrap();
        assert!(journal.is_empty().unwrap());
        worker_journal.started(&[1, 2, 4]);
        assert!(!journal.is_empty().unwrap());
        worker_journal.finished(1, &Default::default(), false);
        worker_journal.finished(4, &Default::default(), true);
        let crash = journal.isolate_unfinished().unwrap();
        assert_eq!((crash.isolated, crash.unfinished, crash.finished), (vec![2], 1, 2));
        assert_eq!(journal.isolate_unfinished().unwrap().isolated, [] as [u32; 0]);

        let restarted = WorkerJournal::open(journal.path.clone()).unwrap();
        assert!(restarted.resumes());
        assert!(restarted.earlier.finished.contains_key(&1) && restarted.earlier.isolated.contains(&2));
        assert!(restarted.earlier.finished[&4].timeout_rerun && !restarted.earlier.finished[&1].timeout_rerun);
        restarted.started(&[2, 3]);
        let crash = journal.isolate_unfinished().unwrap();
        assert_eq!((crash.isolated, crash.unfinished, crash.finished), (vec![3], 2, 2));
    }

    #[test]
    fn unreadable_or_malformed_journal_is_not_an_empty_success() {
        use std::io::Write;
        let mut journal = Journal::create().unwrap();
        journal.file.write_all(b"{\"finished\":").unwrap();
        assert!(journal.unfinished().is_err());
        journal.file = std::fs::OpenOptions::new()
            .write(true)
            .open(&journal.path)
            .unwrap();
        assert!(journal.unfinished().is_err());
    }

    #[test]
    fn invalid_semantic_rows_fail_without_discarding_earlier_bytes() {
        use std::io::{Seek, SeekFrom, Write};
        let prefix = "{\"started\":[1,2]}\n{\"finished\":1,\"result\":\"detected\",\"tests\":[[\"known\",\"detected\"]]}\n";
        for invalid in [
            r#"{"started":null}"#,
            r#"{"started":[-1]}"#,
            r#"{"started":[4294967296]}"#,
            r#"{"crashed":[2]}"#,
            r#"{"finished":4294967296,"result":"detected","tests":[]}"#,
            r#"{"finished":2,"result":"unknown","tests":[]}"#,
            r#"{"finished":2,"result":"detected","tests":null}"#,
            r#"{"finished":2,"result":"detected","tests":[["test",42]]}"#,
            r#"{"finished":2,"result":"detected","tests":[["test","unknown"]]}"#,
            r#"{"finished":2,"result":"detected","tests":[["test"]]}"#,
            r#"{"started":[2],"finished":2,"result":"detected","tests":[]}"#,
            r#"{"started":[2],"extra":true}"#,
            r#"{}"#,
        ] {
            let mut journal = Journal::create().unwrap();
            let bytes = format!("{prefix}{invalid}\n");
            journal.file.write_all(bytes.as_bytes()).unwrap();
            assert!(
                journal.unfinished().is_err(),
                "accepted invalid row: {invalid}"
            );
            assert_eq!(std::fs::read_to_string(&journal.path).unwrap(), bytes);
            journal.file.set_len(prefix.len() as u64).unwrap();
            journal.file.seek(SeekFrom::Start(0)).unwrap();
            assert_eq!(journal.unfinished().unwrap(), [2]);
        }
    }

    #[test]
    fn a_journal_skips_a_name_that_is_taken_and_leaves_that_file_alone() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("mutest-journal-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("taken"), "kept").unwrap();

        let journal = Journal::create_in(&dir, ["taken".to_owned(), "free".to_owned()]).unwrap();

        assert_eq!(fs::read_to_string(dir.join("taken")).unwrap(), "kept");
        assert_eq!(journal.path, dir.join("free"));
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
