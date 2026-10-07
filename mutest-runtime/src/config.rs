use std::path::PathBuf;
use std::time::Duration;

use crate::metadata::MutationMeta;

pub enum Mode {
    Evaluate,
    Flakes { iterations_count: usize },
}

pub struct PrintOptions {
    pub detection_matrix: Option<()>,
    pub subsumption_matrix: Option<()>,
}

pub struct WriteOptions {
    pub out_dir: PathBuf,
    pub eval_stream: Option<()>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TestOrdering {
    ExecTime,
    MutationDistance,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TestTimeout {
    None,
    Auto,
    Explicit(Duration),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MutationIsolation {
    Unsafe,
    All,
}

impl MutationIsolation {
    /// Whether the tests of `mutations`, which run in one loop, run each in a child process.
    pub fn isolates(self, mutations: &[&MutationMeta]) -> bool {
        match (self, mutations) {
            (MutationIsolation::All, [_, _, ..]) => panic!("cannot isolate multiple mutations into separate processes from one run_tests loop"),
            // NOTE: Encountered batch of multiple mutations, so we assume none of them are unsafe,
            //       as that would be an invalid mutation batch.
            (_, [_, _, ..]) => false,
            (MutationIsolation::Unsafe, [mutation]) => mutation.is_unsafe(),
            (_, _) => true,
        }
    }
}

pub struct Options {
    pub mode: Mode,
    pub verbosity: u8,
    pub report_timings: bool,
    pub print_opts: PrintOptions,
    pub write_opts: Option<WriteOptions>,
    pub exhaustive: bool,
    pub test_ordering: TestOrdering,
    pub test_timeout: TestTimeout,
    pub mutation_isolation: MutationIsolation,
    pub use_thread_pool: bool,
}
