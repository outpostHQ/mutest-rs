#![feature(iter_collect_into)]
#![feature(iter_intersperse)]

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::iter;
use std::path::{self, Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::str;
use std::time::Instant;

mod diff;

// Adopt leaked children so they cannot escape the test runner's cleanup.
mod orphans {
    #[cfg(target_os = "linux")]
    mod sys {
        use std::ptr;

        use libc::pid_t;

        pub fn adopt() {
            mutest_runtime::adopt_orphans().expect("UI runner child-subreaper setup failed");
        }

        pub fn adopted() -> Vec<u32> {
            mutest_runtime::children().into_iter().map(|pid| pid as u32).collect()
        }

        pub fn kill_and_reap(pid: u32) {
            // SAFETY: This child is unreaped, so its pid cannot have been reused.
            unsafe {
                libc::kill(pid as pid_t, libc::SIGKILL);
                libc::waitpid(pid as pid_t, ptr::null_mut(), 0);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    mod sys {
        pub fn adopt() {}
        pub fn adopted() -> Vec<u32> { vec![] }
        pub fn kill_and_reap(_pid: u32) {}
    }

    pub use sys::{adopt, adopted};

    /// Kill the adopted children that were not adopted before, returning how many there were.
    // NOTE: Reparented descendants may appear after their parents are killed.
    pub fn kill_adopted_since(before: &[u32]) -> usize {
        let mut left_running = 0;
        loop {
            let adopted = adopted().into_iter().filter(|pid| !before.contains(pid)).collect::<Vec<_>>();
            if adopted.is_empty() { return left_running; }
            if left_running == 0 { left_running = adopted.len(); }
            for pid in adopted {
                sys::kill_and_reap(pid);
            }
        }
    }
}

#[derive(Debug)]
enum ExpectationVerdict {
    Met,
    Unblessed,
    Unmet { reason: String, error: Option<String> },
}

#[derive(Debug)]
enum BlessVerdict {
    New,
    Changed(String),
    UpToDate,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Expectation {
    /// //@ stdout
    StdOut { empty: bool },
    /// //@ stderr
    StdErr { empty: bool },
    /// //@ eval-stream
    EvalStream,
}

struct Outputs<'a> {
    stdout: &'a str,
    stderr: &'a str,
    eval_stream: &'a str,
}

impl Expectation {
    pub fn display_name(&self) -> &str {
        match self {
            Expectation::StdOut { .. } => "stdout",
            Expectation::StdErr { .. } => "stderr",
            Expectation::EvalStream => "evaluation stream",
        }
    }

    fn output<'a>(&self, path: &Path, outputs: &Outputs<'a>) -> (&'a str, PathBuf, bool) {
        match *self {
            Expectation::StdOut { empty } => (outputs.stdout, path.with_extension("stdout"), empty),
            Expectation::StdErr { empty } => (outputs.stderr, path.with_extension("stderr"), empty),
            Expectation::EvalStream => (outputs.eval_stream, path.with_extension("jsonl"), false),
        }
    }

    pub fn check(&self, path: &Path, outputs: &Outputs<'_>) -> ExpectationVerdict {
        let out_name = self.display_name();
        let (out, out_path, expect_empty) = self.output(path, outputs);

        if expect_empty {
            if !out.is_empty() {
                let diff_text = diff::display_diff("", out).unwrap();
                return ExpectationVerdict::Unmet {
                    reason: format!("{out_name} is not empty"),
                    error: Some(diff_text),
                };
            }
        } else {
            if !out_path.exists() { return ExpectationVerdict::Unblessed; }

            let expected_out = fs::read_to_string(&out_path).expect(&format!("cannot read {}", out_path.display()));
            if *out != expected_out {
                let diff_text = diff::display_diff(&expected_out, out).unwrap();
                return ExpectationVerdict::Unmet {
                    reason: format!("{out_name} does not match expected output"),
                    error: Some(diff_text),
                };
            }
        }

        ExpectationVerdict::Met
    }

    pub fn bless(&self, path: &Path, outputs: &Outputs<'_>, dry_run: bool) -> BlessVerdict {
        let (out, out_path, expect_empty) = self.output(path, outputs);
        if expect_empty { return BlessVerdict::UpToDate; }

        let previous_out = out_path.exists().then(|| fs::read_to_string(&out_path).expect(&format!("cannot read {}", out_path.display())));

        if previous_out.as_deref() != Some(out) {
            if !dry_run {
                fs::write(&out_path, out).expect(&format!("cannot write {}", out_path.display()));
            }

            return match previous_out {
                Some(previous_out) => {
                    let diff_text = diff::display_diff(&previous_out, out).unwrap();
                    BlessVerdict::Changed(diff_text)
                }
                None => BlessVerdict::New
            }
        }

        BlessVerdict::UpToDate
    }
}

/// Remove the fields of evaluation stream events that differ from run to run.
fn normalize_eval_stream(stream: &str) -> String {
    stream.lines()
        .map(|line| {
            let Ok(serde_json::Value::Object(mut event)) = serde_json::from_str(line) else { return format!("{line}\n"); };
            for varying in ["time", "thread_id", "test_exec_time"] {
                event.remove(varying);
            }
            format!("{}\n", serde_json::Value::Object(event))
        })
        .collect()
}

#[test]
fn test_normalize_eval_stream() {
    let stream = concat!(
        "{\"format_version\":1}\n",
        "{\"event\":\"test_start\",\"time\":1203,\"mutation_id\":1,\"test_name\":\"test\",\"thread_id\":2}\n",
        "{\"event\":\"test_result\",\"time\":5821,\"mutation_id\":1,\"test_name\":\"test\",\"test_exec_time\":4618,\"test_result\":\"failed\"}\n",
    );

    assert_eq!(normalize_eval_stream(stream), concat!(
        "{\"format_version\":1}\n",
        "{\"event\":\"test_start\",\"mutation_id\":1,\"test_name\":\"test\"}\n",
        "{\"event\":\"test_result\",\"mutation_id\":1,\"test_name\":\"test\",\"test_result\":\"failed\"}\n",
    ));
}

/// Replace the paths of retained analysis journals, which differ from run to run, and remove the journals.
fn normalize_retained_journal_paths(stderr: &str) -> String {
    const PREFIX: &str = "incomplete analysis journal retained at ";
    stderr.split_inclusive('\n')
        .map(|line| {
            let Some(path) = line.strip_prefix(PREFIX) else { return line.to_owned(); };
            let path = Path::new(path.trim_end_matches('\n'));
            if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("mutest-journal-")) {
                let _ = fs::remove_file(path);
            }
            format!("{PREFIX}$RETAINED_JOURNAL{}", if line.ends_with('\n') { "\n" } else { "" })
        })
        .collect()
}

#[test]
fn test_normalize_retained_journal_paths() {
    let journal = env::temp_dir().join(format!("mutest-journal-{}-test", process::id()));
    fs::write(&journal, "").unwrap();
    let stderr = format!("mutation analysis incomplete\nincomplete analysis journal retained at {}\nother warning\n", journal.display());

    assert_eq!(normalize_retained_journal_paths(&stderr), "mutation analysis incomplete\nincomplete analysis journal retained at $RETAINED_JOURNAL\nother warning\n");
    assert!(!journal.exists());
}

const BUILD_OUT_DIR: &str = "target/mutest_test/debug/deps";
const AUX_OUT_DIR: &str = "target/mutest_test/debug/deps/auxiliary";
const EVAL_STREAM_OUT_DIR: &str = "target/mutest_test/json";
const MUTATIONS_OUT_DIR: &str = "target/mutest_test/mutations";

struct Opts {
    pub filters: Option<Vec<String>>,
    pub bless: bool,
    pub dry_run: bool,
    pub verbosity: u8,
    /// The `mutest-driver` binary in Cargo's target directory.
    pub driver: PathBuf,
    /// The `cargo-mutest` binary, passed to generated programs in `CARGO_MUTEST_VAR`.
    pub cargo_mutest: PathBuf,
}

const CARGO_MUTEST_VAR: &str = "MUTEST_TESTS_CARGO_MUTEST";

struct TestRunResults {
    pub ignored_tests_count: usize,
    pub passed_tests_count: usize,
    pub failed_tests_count: usize,
    pub new_tests_count: usize,
    pub blessed_tests_count: usize,
    pub total_tests_count: usize,
}

enum TestResult {
    Ignored,
    Failed,
    Ok,
    New,
    Blessed,
}

fn log_test(test_name: &str, result: TestResult, reason: Option<&str>) {
    eprintln!("test {test_name} ... {result}{reason}",
        result = match result {
            TestResult::Ignored => "\x1b[1;33mignored\x1b[0m",
            TestResult::Failed => "\x1b[1;31mFAILED\x1b[0m",
            TestResult::Ok => "\x1b[1;32mok\x1b[0m",
            TestResult::New => "\x1b[1;33mNEW\x1b[0m",
            TestResult::Blessed => "\x1b[1;35mBLESSED\x1b[0m",
        },
        reason = reason.map(|s| format!(" ({s})")).unwrap_or("".to_owned()),
    );
}

fn parse_directives(path: &Path) -> Vec<String> {
    let source = fs::File::open(path).expect(&format!("cannot open `{}`", path.display()));
    let mut reader = BufReader::with_capacity(1024, source);

    let mut directives = vec![];
    let mut line = String::new();
    while reader.read_line(&mut line).expect(&format!("cannot read contents of `{}`", path.display())) >= 1 {
        if let Some(directive) = line.trim_start().strip_prefix("//@").map(str::trim) {
            directives.push(directive.to_owned());
        }
        line.clear();
    }

    directives
}

fn parse_args(args_str: &str) -> Vec<&str> {
    let mut args = vec![];

    let mut char_indices_iter = args_str.char_indices();
    while let Some((c_idx, c)) = char_indices_iter.next() {
        // NOTE: Skip past multiple consecutive whitespace separators between args.
        if c.is_whitespace() { continue; }

        let (start_idx, end_idx) = match c {
            '"' => {
                let start_idx = char_indices_iter.offset();
                let end_idx = 'end_idx: {
                    while let Some((c_idx, c)) = char_indices_iter.next() {
                        if c != '"' { continue; }
                        match char_indices_iter.clone().next() {
                            None => { break 'end_idx c_idx; }
                            Some((_, next_c)) if next_c.is_whitespace() => {
                                // NOTE: Skip past the whitespace character after `"`.
                                let _ = char_indices_iter.next();
                                break 'end_idx c_idx;
                            }
                            _ => { continue; }
                        }
                    }
                    // NOTE: This is the fallback value if `"` is not closed by the end of the args list.
                    args_str.len()
                };
                (start_idx, end_idx)
            }
            _ => {
                let end_idx = 'end_idx: {
                    while let Some((c_idx, c)) = char_indices_iter.next() {
                        if c.is_whitespace() { break 'end_idx c_idx; }
                    }
                    args_str.len()
                };
                (c_idx, end_idx)
            }
        };

        args.push(&args_str[start_idx..end_idx]);
    }

    args
}

#[test]
fn test_parse_args() {
    assert_eq!(["foo", "bar", "baz"], parse_args("foo bar baz")[..]);
    assert_eq!(["foo", "bar", "baz"], parse_args("foo   bar   baz")[..]);
    assert_eq!(["foo", "bar baz", "abc"], parse_args("foo \"bar baz\" abc")[..]);
    assert_eq!(["foo", "bar=\"baz\"", "abc"], parse_args("foo \"bar=\"baz\"\" abc")[..]);
    assert_eq!(["foo", "bar baz"], parse_args("foo \"bar baz\"")[..]);
    assert_eq!(["foo", "bar baz"], parse_args("foo \"bar baz")[..]);
}

struct AuxDirectives<'d> {
    pub edition: Option<&'d str>,
}

struct MutestTargetDirectives<'d> {
    pub no_harness: bool,
    pub mutest_prints: BTreeSet<&'d str>,
    pub mutest_outputs: Vec<&'d str>,
}

enum TestTarget<'d> {
    Rustc,
    Mutest(MutestTargetDirectives<'d>),
}

struct TestDirectives<'d> {
    pub edition: Option<&'d str>,
    pub bin: bool,
    pub expect_build_fail: bool,
    pub exec_build_artifact: bool,
    pub expected_run_exit_code: i32,
    /// Set by `//@ mutations: none`. A test that selects mutation operators must otherwise produce mutations.
    pub expect_no_mutations: bool,
    pub expectations: BTreeSet<Expectation>,
    pub target: TestTarget<'d>,
}

fn read_mutations_count(dir: &Path) -> Result<u64, String> {
    let path = dir.join("mutations.json");
    let json = fs::read_to_string(&path).map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let mutations_info = serde_json::from_str::<serde_json::Value>(&json).map_err(|error| format!("cannot parse `{}`: {error}", path.display()))?;
    mutations_info["stats"]["total_mutations_count"].as_u64().ok_or_else(|| format!("`{}` has no mutation count", path.display()))
}

fn run_test(path: &Path, aux_dir_path: &Path, root_dir: &Path, opts: &Opts, results: &mut TestRunResults) {
    if !path.is_file() { return; }
    if !path.extension().is_some_and(|v| v == "rs") { return; }

    results.total_tests_count += 1;

    let display_path = path.strip_prefix(root_dir).expect("cannot strip root dir prefix").with_extension("");

    let name = display_path.to_string_lossy().into_owned();

    if let Some(filters) = &opts.filters {
        if !filters.iter().any(|filter| name.contains(filter)) { return; }
    }

    let unmangled_crate_name = display_path.components()
        .filter_map(|component| {
            match component {
                path::Component::Normal(component) => Some(component.to_str().expect("invalid path component")),
                _ => None,
            }
        })
        .intersperse("_")
        .collect::<String>();

    let crate_hash = {
        let mut hasher = std::hash::DefaultHasher::new();
        unmangled_crate_name.hash(&mut hasher);
        let hash = hasher.finish();
        format!("{hash:016x}")
    };

    let test_crate_name = format!("mutest_test_{crate_hash}_{crate_name}",
        crate_name = &unmangled_crate_name[..unmangled_crate_name.floor_char_boundary(48)],
    );

    let raw_test_directives = parse_directives(&path);

    if raw_test_directives.iter().any(|d| d == "ignore") {
        results.ignored_tests_count += 1;
        log_test(&name, TestResult::Ignored, None);
        return;
    }

    let mut raw_test_directives_iter = raw_test_directives.iter().peekable();
    let test_target = match raw_test_directives_iter.peek().map(|s| s.as_str()) {
        Some("rustc") => {
            // Consume the first `//@ rustc` directive so it is not encountered later.
            let _ = raw_test_directives_iter.next();
            TestTarget::Rustc
        }
        _ => TestTarget::Mutest(MutestTargetDirectives {
            no_harness: false,
            mutest_prints: BTreeSet::new(),
            mutest_outputs: vec!["info"],
        }),
    };
    let mut seen_primary_action_directive = false;
    let mut test_directives = TestDirectives {
        edition: None,
        bin: false,
        expect_build_fail: false,
        exec_build_artifact: false,
        expected_run_exit_code: mutest_exit_code::SUCCESS,
        expect_no_mutations: false,
        expectations: BTreeSet::new(),
        target: test_target,
    };
    for directive in raw_test_directives_iter {
        match directive.as_str() {
            "rustc" => {
                results.ignored_tests_count += 1;
                log_test(&name, TestResult::Ignored, Some("invalid directive: `rustc` target directive must be specified as the first directive"));
                return;
            }

            primary_action_directive if matches!(primary_action_directive, "build" | "build: fail" | "run")
                || primary_action_directive.starts_with("run: exit ") => {
                if seen_primary_action_directive {
                    results.ignored_tests_count += 1;
                    log_test(&name, TestResult::Ignored, Some("invalid directive: multiple primary action directives specified"));
                    return;
                }
                seen_primary_action_directive = true;

                // NOTE: All explicit primary action directives require building a test binary, which has to be explicitly requested from mutest-driver.
                if let TestTarget::Mutest(mutest_target_directives) = &mut test_directives.target {
                    mutest_target_directives.mutest_outputs.push("test-bin");
                }

                match primary_action_directive {
                    "build" => {}
                    "build: fail" => {
                        test_directives.expect_build_fail = true;
                    }
                    "run" => {
                        test_directives.exec_build_artifact = true;
                    }
                    _ if let Some(exit_code) = primary_action_directive.strip_prefix("run: exit ") => {
                        let Ok(exit_code) = exit_code.trim().parse() else {
                            results.ignored_tests_count += 1;
                            log_test(&name, TestResult::Ignored, Some(&format!("invalid directive: `{primary_action_directive}` names no exit code")));
                            return;
                        };
                        test_directives.exec_build_artifact = true;
                        test_directives.expected_run_exit_code = exit_code;
                    }
                    _ => unreachable!(),
                }
            }

            info_request_directive @ ("print-tests" | "print-call-graph" | "print-targets" | "print-unreached" | "print-mutations" | "print-code") => {
                let TestTarget::Mutest(mutest_target_directives) = &mut test_directives.target else {
                    results.ignored_tests_count += 1;
                    log_test(&name, TestResult::Ignored, Some(&format!("invalid directive: `{info_request_directive}` directive cannot be used with `rustc` target directive")));
                    return;
                };

                if seen_primary_action_directive {
                    results.ignored_tests_count += 1;
                    log_test(&name, TestResult::Ignored, Some("invalid directive: info request directives must be specified before an explicit action directive"));
                    return;
                }

                match info_request_directive {
                    "print-tests" => { mutest_target_directives.mutest_prints.insert("tests"); }
                    "print-call-graph" => { mutest_target_directives.mutest_prints.insert("call-graph"); }
                    "print-targets" => { mutest_target_directives.mutest_prints.insert("targets"); }
                    "print-unreached" => { mutest_target_directives.mutest_prints.insert("unreached"); }
                    "print-mutations" => { mutest_target_directives.mutest_prints.insert("mutations"); }
                    "print-code" => { mutest_target_directives.mutest_prints.insert("code"); }
                    _ => unreachable!(),
                }
            }

            _ if let Some(edition_str) = directive.strip_prefix("edition:").map(str::trim) => {
                if let Some(_previous_edition) = test_directives.edition {
                    results.ignored_tests_count += 1;
                    log_test(&name, TestResult::Ignored, Some("invalid directive: multiple editions specified"));
                    return;
                }
                test_directives.edition = Some(edition_str);
            }

            "no-harness" => {
                match &mut test_directives.target {
                    TestTarget::Rustc => {
                        results.ignored_tests_count += 1;
                        log_test(&name, TestResult::Ignored, Some("invalid directive: `no-harness` directive cannot be used with `rustc` target directive"));
                        return;
                    }
                    TestTarget::Mutest(mutest_target_directives) => {
                        mutest_target_directives.no_harness = true;
                    }
                }
            }
            "bin" => test_directives.bin = true,

            "stdout" => { test_directives.expectations.insert(Expectation::StdOut { empty: false }); }
            "stdout: empty" => { test_directives.expectations.insert(Expectation::StdOut { empty: true }); }
            "stderr" => { test_directives.expectations.insert(Expectation::StdErr { empty: false }); }
            "stderr: empty" => { test_directives.expectations.insert(Expectation::StdErr { empty: true }); }
            "mutations: none" => {
                let TestTarget::Mutest(_) = &test_directives.target else {
                    results.ignored_tests_count += 1;
                    log_test(&name, TestResult::Ignored, Some("invalid directive: `mutations` directive cannot be used with `rustc` target directive"));
                    return;
                };
                test_directives.expect_no_mutations = true;
            }
            "eval-stream" => {
                let TestTarget::Mutest(_) = &test_directives.target else {
                    results.ignored_tests_count += 1;
                    log_test(&name, TestResult::Ignored, Some("invalid directive: `eval-stream` directive cannot be used with `rustc` target directive"));
                    return;
                };
                test_directives.expectations.insert(Expectation::EvalStream);
            }

            _ if directive.starts_with("aux-build:") => {}

            _ if directive.starts_with("rustc-flags:") => {}
            _ if directive.starts_with("build-env:") => {
                if !directive.contains('=') {
                    log_test(&name, TestResult::Ignored, Some("invalid directive: each compile-time environment variable must be specified one `build-env: KEY=value` at a time"));
                    return;
                }
            }
            _ if directive.starts_with("run-flags:") => {}
            _ if directive.starts_with("run-env:") => {
                if !directive.contains('=') {
                    log_test(&name, TestResult::Ignored, Some("invalid directive: each runtime environment variable must be specified one `run-env: KEY=value` at a time"));
                    return;
                }
            }

            _ if false
                || directive.starts_with("verify:")
                || directive.starts_with("mutation-operators:")
                || directive.starts_with("mutest-flags:")
            => {
                let TestTarget::Mutest(_) = &test_directives.target else {
                    results.ignored_tests_count += 1;
                    let directive_name = match directive.split_once(':') {
                        Some((name, _)) => name,
                        None => directive,
                    };
                    log_test(&name, TestResult::Ignored, Some(&format!("invalid directive: `{directive_name}` directive cannot be used with `rustc` target directive")));
                    return;
                };
            }

            _ => {
                results.ignored_tests_count += 1;
                log_test(&name, TestResult::Ignored, Some(&format!("unknown directive: `{directive}`")));
                return;
            }
        }
    }

    // Set defaults.
    let edition = test_directives.edition.unwrap_or("2018");

    let mut aux = false;
    for directive in &raw_test_directives {
        let Some(aux_build) = directive.strip_prefix("aux-build:").map(str::trim) else { continue; };
        aux = true;

        let aux_path = aux_dir_path.join(aux_build);
        let aux_crate_name = Path::new(aux_build).file_stem().expect("invalid aux path").to_str().expect("invalid aux path");

        let raw_aux_directives = parse_directives(&aux_path);

        let mut aux_directives = AuxDirectives {
            edition: None,
        };
        for directive in &raw_aux_directives {
            match directive.as_str() {
                _ if let Some(edition_str) = directive.strip_prefix("edition:").map(str::trim) => {
                    if let Some(_previous_edition) = aux_directives.edition {
                        results.ignored_tests_count += 1;
                        log_test(&name, TestResult::Ignored, Some(&format!("invalid directive in `{aux_build}` aux file: multiple editions specified")));
                        return;
                    }
                    aux_directives.edition = Some(edition_str);
                }

                _ if directive.starts_with("build-env:") => {
                    if !directive.contains('=') {
                        log_test(&name, TestResult::Ignored, Some(&format!("invalid directive in `{aux_build}` aux file: each compile-time environment variable must be specified one `build-env: KEY=value` at a time")));
                        return;
                    }
                }

                _ => {
                    results.ignored_tests_count += 1;
                    log_test(&name, TestResult::Ignored, Some(&format!("unknown directive in `{aux_build}` aux file: `{directive}`")));
                    return;
                }
            }
        }

        // Set defaults.
        let edition = aux_directives.edition.unwrap_or("2018");

        // Run rustc directly.
        let mut cmd = Command::new("rustc");

        cmd.arg(&aux_path);
        cmd.args(["--crate-name", aux_crate_name]);
        cmd.args(["--edition", edition]);

        cmd.args(["--out-dir", AUX_OUT_DIR]);

        cmd.args(["-L", AUX_OUT_DIR]);

        let build_env = raw_aux_directives.iter().filter_map(|d| d.strip_prefix("build-env:").map(str::trim))
            .flat_map(|env| {
                let (key, val) = env.split_once("=")?;
                Some((key.trim(), val.trim()))
            });
        for (key, val) in build_env {
            cmd.env(key, val);
        }

        // NOTE: Avoid passing on the `CARGO_*` environment variables from the test runner.
        for (var, _) in env::vars() {
            if var.starts_with("CARGO_") {
                cmd.env_remove(var);
            }
        }

        if opts.verbosity >= 1 {
            eprintln!("running {cmd:?}");
        }

        let output = cmd.output().expect("cannot spawn mutest-driver in rustc mode");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if opts.verbosity >= 1 {
            if let Some(exit_code) = output.status.code() {
                eprintln!("exited with code {exit_code}");
            }
            eprintln!("stdout:\n{}", stdout);
            eprintln!("stderr:\n{}", stderr);
        }

        if output.status.code() != Some(0) {
            results.failed_tests_count += 1;
            log_test(&name, TestResult::Failed, Some(&match output.status.code() {
                Some(exit_code) => format!("process exited with code {exit_code}"),
                None => "process exited without exit code".to_owned(),
            }));
            eprintln!("stdout:\n{}", stdout);
            eprintln!("stderr:\n{}", stderr);
            return;
        }
    }

    let mut cmd = match test_directives.target {
        TestTarget::Rustc => Command::new("rustc"),
        TestTarget::Mutest(_) => Command::new(&opts.driver),
    };
    cmd.arg(&path);
    cmd.args(["--crate-name", &test_crate_name]);
    cmd.args(["--edition", edition]);

    match test_directives.bin {
        false => { cmd.args(["--crate-type", "lib"]); }
        true => { cmd.args(["--crate-type", "bin"]); }
    }

    cmd.args(["--out-dir", BUILD_OUT_DIR]);

    if let TestTarget::Mutest(mutest_target_directives) = &test_directives.target {
        // NOTE: For mutest-driver to not fall back to a rustc invocation, we must have at least `cfg(test)` set.
        match mutest_target_directives.no_harness {
            false => { cmd.arg("--test"); }
            true => { cmd.arg("--cfg=test"); }
        }
    }

    // Explicitly disable color output. This mainly affects diagnostic messages generated for undetected mutations.
    cmd.arg("--color=never");

    let build_env = raw_test_directives.iter().filter_map(|d| d.strip_prefix("build-env:").map(str::trim))
        .flat_map(|env| {
            let (key, val) = env.split_once("=")?;
            Some((key.trim(), val.trim()))
        });
    for (key, val) in build_env {
        cmd.env(key, val);
    }

    let rustc_flags = raw_test_directives.iter().filter_map(|d| d.strip_prefix("rustc-flags:").map(str::trim)).flat_map(parse_args);
    cmd.args(rustc_flags);

    if aux {
        cmd.args(["-L", AUX_OUT_DIR]);
    }

    // NOTE: A test that selects mutation operators but produces no mutations tests none of their code generation.
    let selects_mutation_operators = raw_test_directives.iter().any(|d| d.starts_with("mutation-operators:"));
    let counts_mutations = (selects_mutation_operators || test_directives.expect_no_mutations) && !test_directives.expect_build_fail;
    let mutations_dir = match &test_directives.target {
        TestTarget::Mutest(mutest_target_directives) if counts_mutations && mutest_target_directives.mutest_outputs.contains(&"test-bin") => {
            let dir = path::absolute(Path::new(MUTATIONS_OUT_DIR).join(&test_crate_name)).expect("cannot resolve the mutations directory");
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("cannot create `{}`: {error}", dir.display()));
            Some(dir)
        }
        _ => None,
    };
    if let TestTarget::Mutest(mutest_target_directives) = &test_directives.target {
        let mut mutest_outputs = mutest_target_directives.mutest_outputs.clone();
        if mutations_dir.is_some() {
            mutest_outputs.push("metadata");
        }
        let mut mutest_args = vec![format!("--emit={}", mutest_outputs.join(","))];
        if let Some(dir) = &mutations_dir {
            mutest_args.push(format!("--metadata-out-root-dir={}", dir.display()));
        }
        let verifications = raw_test_directives.iter().filter_map(|d| d.strip_prefix("verify:").map(str::trim))
            .flat_map(|flags| flags.split(",").map(str::trim).filter(|flag| !flag.is_empty()));
        for verification in verifications {
            mutest_args.push("-Z".to_owned());
            mutest_args.push(format!("verify-{}", verification));
        }
        let mut mutation_operators = raw_test_directives.iter().filter_map(|d| d.strip_prefix("mutation-operators:").map(str::trim))
            .flat_map(|flags| flags.split(",").map(str::trim).filter(|flag| !flag.is_empty()))
            .peekable();
        if mutation_operators.peek().is_some() {
            mutest_args.push("--mutation-operators".to_owned());
            mutest_args.push(mutation_operators.intersperse(",").collect::<String>());
        }
        if !mutest_target_directives.mutest_prints.is_empty() {
            mutest_args.push("--print".to_owned());
            mutest_args.push(mutest_target_directives.mutest_prints.iter().map(|s| *s).intersperse(",").collect::<String>());
        }
        raw_test_directives.iter().filter_map(|d| d.strip_prefix("mutest-flags:").map(str::trim))
            .flat_map(|flags| parse_args(flags).into_iter().map(str::to_owned))
            .collect_into(&mut mutest_args);
        cmd.env("MUTEST_ENCODED_ARGS".to_owned(), mutest_args.join("\x1F"));
    }

    // NOTE: Avoid passing on the `CARGO_*` environment variables from the test runner.
    for (var, _) in env::vars() {
        if var.starts_with("CARGO_") {
            cmd.env_remove(var);
        }
    }

    if opts.verbosity >= 1 {
        eprintln!("running {cmd:?}");
    }

    let output = cmd.output().expect("cannot spawn mutest-driver");
    let mut stdout = String::from_utf8(output.stdout).unwrap();
    let mut stderr = String::from_utf8(output.stderr).unwrap();
    if opts.verbosity >= 1 {
        if let Some(exit_code) = output.status.code() {
            eprintln!("exited with code {exit_code}");
        }
        eprintln!("stdout:\n{}", stdout);
        eprintln!("stderr:\n{}", stderr);
    }

    let expected_exit_code = match test_directives.expect_build_fail {
        true => 1,
        false => 0,
    };
    if output.status.code() != Some(expected_exit_code) {
        results.failed_tests_count += 1;
        log_test(&name, TestResult::Failed, Some(&match output.status.code() {
            Some(exit_code) => format!("process exited with code {exit_code}, expected {expected_exit_code}"),
            None => format!("process exited without exit code, expected {expected_exit_code}"),
        }));
        eprintln!("stdout:\n{}", stdout);
        eprintln!("stderr:\n{}", stderr);
        return;
    }

    if let Some(dir) = &mutations_dir {
        let mutations_count = read_mutations_count(dir);
        let _ = fs::remove_dir_all(dir);
        let failure = match (mutations_count, test_directives.expect_no_mutations) {
            (Err(error), _) => Some(error),
            (Ok(0), false) => Some("the selected mutation operators produced no mutations; add `//@ mutations: none` if none are expected".to_owned()),
            (Ok(count @ 1..), true) => Some(format!("the harness contains {count} mutations, expected none")),
            (Ok(_), _) => None,
        };
        if let Some(reason) = failure {
            results.failed_tests_count += 1;
            log_test(&name, TestResult::Failed, Some(&reason));
            return;
        }
    }

    let mut eval_stream = String::new();

    if test_directives.exec_build_artifact {
        let build_artifact_path = Path::new(BUILD_OUT_DIR).join(&test_crate_name);
        let mut cmd = Command::new(&build_artifact_path);
        // NOTE: The generated program logs each exit code it reports to `cargo mutest` here.
        let exit_code_log = path::absolute(Path::new(BUILD_OUT_DIR).join(format!("{test_crate_name}.exit-codes"))).expect("cannot resolve the exit code log path");
        let _ = fs::remove_file(&exit_code_log);
        cmd.env(mutest_exit_code::LOG_VAR, &exit_code_log);
        cmd.env(CARGO_MUTEST_VAR, &opts.cargo_mutest);

        // NOTE: The directory is removed once the stream has been read.
        let eval_stream_dir = test_directives.expectations.contains(&Expectation::EvalStream).then(|| {
            let dir = path::absolute(Path::new(EVAL_STREAM_OUT_DIR).join(&test_crate_name)).expect("cannot resolve the evaluation stream directory");
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("cannot create `{}`: {error}", dir.display()));
            dir
        });
        if let Some(dir) = &eval_stream_dir {
            cmd.arg(format!("--metadata-out-root-dir={}", dir.display()));
            cmd.arg("--Zwrite-json-eval-stream");
        }

        let run_env = raw_test_directives.iter().filter_map(|d| d.strip_prefix("run-env:").map(str::trim))
            .flat_map(|env| {
                let (key, val) = env.split_once("=")?;
                Some((key.trim(), val.trim()))
            });
        for (key, val) in run_env {
            cmd.env(key, val);
        }

        let run_flags = raw_test_directives.iter().filter_map(|d| d.strip_prefix("run-flags:").map(str::trim)).flat_map(parse_args);
        cmd.args(run_flags);

        // NOTE: Avoid passing on the `CARGO_*` environment variables from the test runner.
        for (var, _) in env::vars() {
            if var.starts_with("CARGO_") {
                cmd.env_remove(var);
            }
        }

        if opts.verbosity >= 1 {
            eprintln!("running {cmd:?}");
        }

        let full_stdout = &mut stdout;
        let full_stderr = &mut stderr;

        let orphans_before = orphans::adopted();
        let output = cmd.output().expect(&format!("cannot spawn generated program `{}`", build_artifact_path.display()));
        // Kill leaks before they accumulate across later tests.
        let left_running = orphans::kill_adopted_since(&orphans_before);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let stderr = normalize_retained_journal_paths(&stderr);

        if let Some(dir) = &eval_stream_dir {
            eval_stream = normalize_eval_stream(&fs::read_to_string(dir.join("evaluation.jsonl")).unwrap_or_default());
            let _ = fs::remove_dir_all(dir);
        }
        let recorded_exit_codes = mutest_exit_code::read(&fs::read_to_string(&exit_code_log).unwrap_or_default());
        let _ = fs::remove_file(&exit_code_log);

        if opts.verbosity >= 1 {
            if let Some(exit_code) = output.status.code() {
                eprintln!("exited with code {exit_code}");
            }
            eprintln!("stdout:\n{}", stdout);
            eprintln!("stderr:\n{}", stderr);
        }

        let expected_run_exit_code = test_directives.expected_run_exit_code;
        // NOTE: Only programs generated by mutest-driver record the exit codes they report.
        let records_exit_codes = matches!(test_directives.target, TestTarget::Mutest(_));
        let failure = if output.status.code() != Some(expected_run_exit_code) {
            Some(match output.status.code() {
                Some(exit_code) => format!("process exited with code {exit_code}, expected {expected_run_exit_code}"),
                None => format!("process exited without exit code, expected {expected_run_exit_code}"),
            })
        } else if records_exit_codes && recorded_exit_codes != [expected_run_exit_code] {
            Some(format!("recorded exit codes {recorded_exit_codes:?} for `cargo mutest`, expected [{expected_run_exit_code}]"))
        } else if left_running >= 1 {
            Some(format!("left {left_running} processes running after it exited"))
        } else {
            None
        };
        if let Some(reason) = failure {
            results.failed_tests_count += 1;
            log_test(&name, TestResult::Failed, Some(&reason));
            eprintln!("stdout:\n{}", stdout);
            eprintln!("stderr:\n{}", stderr);
            return;
        }

        // NOTE: We only add the divider in the concatenated stdout and stderr streams
        //       if both the build stream and the run stream are not empty,
        //       mainly to ensure that the expectations on stream emptiness are not broken
        //       by these synthetic dividers.
        if !full_stdout.is_empty() && !stdout.is_empty() {
            full_stdout.push_str("\n---\n\n");
        }
        if !full_stderr.is_empty() && !stderr.is_empty() {
            full_stderr.push_str("\n---\n\n");
        }

        full_stdout.push_str(&stdout);
        full_stderr.push_str(&stderr);
    }

    // DefaultHasher output can change between Rust releases.
    let stdout = stdout.replace(&crate_hash, "$HASH");
    let stderr = stderr.replace(&crate_hash, "$HASH");
    let outputs = Outputs { stdout: &stdout, stderr: &stderr, eval_stream: &eval_stream };

    if opts.bless {
        let bless_verdicts = test_directives.expectations.iter()
            .map(|expectation| expectation.bless(&path, &outputs, opts.dry_run))
            .collect::<Vec<_>>();

        if bless_verdicts.iter().all(|v| matches!(v, BlessVerdict::UpToDate)) {
            results.passed_tests_count += 1;
            log_test(&name, TestResult::Ok, None);
            return;
        }

        results.blessed_tests_count += 1;
        if bless_verdicts.iter().all(|v| matches!(v, BlessVerdict::New)) {
            results.new_tests_count += 1;
        }
        log_test(&name, TestResult::Blessed, None);

        for (expectation, bless_verdict) in iter::zip(&test_directives.expectations, &bless_verdicts) {
            match bless_verdict {
                BlessVerdict::New => {}
                BlessVerdict::Changed(change) => {
                    eprintln!("{}:\n{change}", expectation.display_name());
                }
                BlessVerdict::UpToDate => {}
            }
        }
    } else {
        let expectation_verdicts = test_directives.expectations.iter()
            .map(|expectation| expectation.check(&path, &outputs))
            .collect::<Vec<_>>();

        if expectation_verdicts.iter().all(|v| matches!(v, ExpectationVerdict::Met)) {
            results.passed_tests_count += 1;
            log_test(&name, TestResult::Ok, None);
            return;
        }

        let has_unblessed_expectations = expectation_verdicts.iter().any(|v| matches!(v, ExpectationVerdict::Unblessed));

        if has_unblessed_expectations { results.new_tests_count += 1; }

        let unmet_expectation_verdicts = expectation_verdicts.iter()
            .filter(|v| matches!(v, ExpectationVerdict::Unmet { .. }))
            .collect::<Vec<_>>();

        match (&unmet_expectation_verdicts[..], has_unblessed_expectations) {
            ([], true) => {
                log_test(&name, TestResult::New, None);
            }
            ([], false) => unreachable!(),
            ([ExpectationVerdict::Unmet { reason, error }], _) => {
                results.failed_tests_count += 1;
                log_test(&name, TestResult::Failed, Some(reason));
                if let Some(error) = error {
                    eprintln!("{error}");
                }
            }
            (unmet_expectation_verdicts, _) => {
                results.failed_tests_count += 1;
                log_test(&name, TestResult::Failed, Some(&format!("{} expectations failed", unmet_expectation_verdicts.len())));
                for unmet_expectation_verdict in unmet_expectation_verdicts {
                    let ExpectationVerdict::Unmet { reason, error } = unmet_expectation_verdict else { unreachable!(); };
                    eprintln!("{reason}:");
                    if let Some(error) = error {
                        eprintln!("{error}");
                    }
                }
            }
        }
    }
}

fn main() {
    let matches = clap::command!()
        .bin_name("cargo ui-test")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(clap::arg!(--bless "Update expectation snapshots for new and existing tests."))
        .arg(clap::arg!(--"dry-run" "Do not modify the file system when blessing expectations."))
        .arg(clap::arg!(--filter [FILTER] "Only run tests matching any of one or more filter(s)."))
        .arg(clap::arg!(-v --verbose "Print more verbose information during execution.").action(clap::ArgAction::Count).default_value("0").display_order(100))
        .arg(clap::arg!(-h --help "Print help information; this message.").action(clap::ArgAction::Help).display_order(999).global(true))
        .get_matches();

    let bless = matches.get_flag("bless");
    let dry_run = matches.get_flag("dry-run");
    let verbosity = matches.get_count("verbose");

    let filters = matches.get_one::<String>("filter").map(|s| s.split(",").map(|f| f.trim().to_owned()).collect::<Vec<_>>());

    let target_dir = cargo_metadata::MetadataCommand::new().no_deps().exec()
        .expect("could not retrieve Cargo metadata")
        .target_directory
        .into_std_path_buf();

    let opts = Opts {
        filters,
        bless,
        dry_run,
        verbosity,
        driver: target_dir.join("release").join(format!("mutest-driver{}", env::consts::EXE_SUFFIX)),
        cargo_mutest: target_dir.join("release").join(format!("cargo-mutest{}", env::consts::EXE_SUFFIX)),
    };

    // Ensure we are testing latest mutest-driver and cargo-mutest.
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "-p", "mutest-driver", "-p", "cargo-mutest"]);
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    if !cmd.output().expect("cannot spawn cargo").status.success() {
        eprintln!("`cargo build --release -p mutest-driver -p cargo-mutest` failed");
        process::exit(1);
    }
    eprintln!();

    // NOTE: Only after the build, so that a daemon Cargo starts is not taken for a test's orphan.
    orphans::adopt();

    let mut results = TestRunResults {
        ignored_tests_count: 0,
        passed_tests_count: 0,
        failed_tests_count: 0,
        new_tests_count: 0,
        blessed_tests_count: 0,
        total_tests_count: 0,
    };

    let t_tests_start = Instant::now();

    // Ensure build output directory exists.
    fs::create_dir_all(BUILD_OUT_DIR).expect("cannot create build output directory");

    fn run_tests_in_dir(dir_path: &Path, opts: &Opts, results: &mut TestRunResults) {
        fn run_tests_in_dir_impl(root_dir: &Path, dir_path: &Path, opts: &Opts, results: &mut TestRunResults) {
            let aux_dir_path = dir_path.join("auxiliary");

            for entry in fs::read_dir(dir_path).expect(&format!("cannot read `{}` directory", dir_path.display())) {
                let entry = entry.expect(&format!("cannot read entry in `{}` directory", dir_path.display()));
                let path = entry.path();

                #[cfg(windows)]
                let path = {
                    use std::path::PathBuf;
                    use path_slash::PathBufExt;
                    PathBuf::from(path.to_slash_lossy().to_string())
                };

                if path.is_dir() {
                    if path.file_name().is_some_and(|v| v == "auxiliary") { continue; };
                    run_tests_in_dir_impl(root_dir, &path, opts, results);
                    continue;
                }

                if !path.is_file() { continue; }
                if !path.extension().is_some_and(|v| v == "rs") { continue; }

                run_test(&path, &aux_dir_path, root_dir, opts, results);
            }
        }

        run_tests_in_dir_impl(dir_path, dir_path, opts, results);
    }

    run_tests_in_dir(Path::new("tests/ui"), &opts, &mut results);

    let tests_duration = t_tests_start.elapsed();

    eprintln!();
    if opts.bless {
        eprintln!("test result: {result}. {blessed} blessed ({new} new); {passed} passed; {failed} failed; {ignored} ignored; finished in {duration:.2?}",
            result = match results.blessed_tests_count {
                0 => "\x1b[1;32mok\x1b[0m",
                _ => "\x1b[1;33mCHANGED\x1b[0m",
            },
            blessed = results.blessed_tests_count,
            new = results.new_tests_count,
            passed = results.passed_tests_count,
            failed = results.failed_tests_count,
            ignored = results.ignored_tests_count,
            duration = tests_duration,
        );
    } else {
        eprintln!("test result: {result}. {passed} passed; {failed} failed; {ignored} ignored; finished in {duration:.2?}",
            result = match results.failed_tests_count {
                0 => "\x1b[1;32mok\x1b[0m",
                _ => "\x1b[1;31mFAILED\x1b[0m",
            },
            passed = results.passed_tests_count,
            failed = results.failed_tests_count,
            ignored = results.ignored_tests_count,
            duration = tests_duration,
        );

        if results.new_tests_count >= 1 {
            eprintln!("note: encountered {new} tests with missing expectation snapshots, rerun with `--bless`",
                new = results.new_tests_count,
            );
        }
    }

    if results.failed_tests_count >= 1 {
        process::exit(101);
    }
}
