//! The mutations a worker started and finished, so that a crash can name those left unfinished.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Mutex, OnceLock};

use crate::harness::{MutationTestResult, MutationTestResults};
use crate::json;

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
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecordedResult {
    Undetected,
    Detected,
    TimedOut,
    Crashed,
}

/// The results are kept for diagnosing a crashed run; only the ids are read back.
#[derive(serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Record {
    Started {
        started: Vec<u32>,
    },
    Finished {
        finished: u32,
        #[serde(rename = "result")]
        _result: RecordedResult,
        #[serde(rename = "tests")]
        _tests: Vec<(String, Option<RecordedResult>)>,
    },
}

fn append(file: &mut File, line: json::Object) -> io::Result<()> {
    let mut bytes = line.into_vec()?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.flush()
}

#[derive(Default)]
struct Entries {
    started: BTreeSet<u32>,
    finished: BTreeSet<u32>,
}

impl Entries {
    fn add(&mut self, record: Record) -> io::Result<()> {
        match record {
            Record::Started { started } => self.started.extend(started),
            Record::Finished { finished, .. } => {
                if !self.finished.insert(finished) {
                    return Err(io::Error::other("duplicate finished journal result"));
                }
            }
        }
        Ok(())
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
        let entries = read(&self.file)?;
        Ok(entries
            .started
            .into_iter()
            .filter(|id| !entries.finished.contains(id))
            .collect())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
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

impl WorkerJournal {
    fn open(path: PathBuf) -> Option<Self> {
        let file = OpenOptions::new().append(true).open(&path).ok()?;
        Some(Self { path, file: Mutex::new(Some(file)) })
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

    pub fn finished(&self, mutation_id: u32, results: &MutationTestResults) {
        let tests = results
            .results_per_test
            .iter()
            .map(|(name, result)| (name.as_slice(), result.map(result_name)))
            .collect::<Vec<_>>();
        self.append(json::Object::new().field("finished", &mutation_id).field("result", result_name(results.result)).field("tests", &tests));
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
