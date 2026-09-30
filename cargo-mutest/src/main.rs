use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use mutest_driver_cli::{UnstableFlag, UnstableOption};
use mutest_exit_code as exit_code;

pub mod build {
    pub const RUST_TOOLCHAIN_VERSION: &str = env!("RUST_TOOLCHAIN_VERSION");
}

fn strip_arg(args: &mut Vec<String>, has_value: bool, short_arg: Option<&str>, long_arg: Option<&str>) {
    let short_arg = short_arg.map(|v| format!("-{v}"));
    let long_arg = long_arg.map(|v| format!("--{v}"));

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let arg_without_prefix = short_arg.as_deref().and_then(|v| arg.strip_prefix(v))
            .or_else(|| long_arg.as_deref().and_then(|v| arg.strip_prefix(v)));

        match arg_without_prefix.map(|v| has_value && !v.trim_start().starts_with("=") && i + 1 < args.len()) {
            Some(true) => { args.splice(i..=(i + 1), []); }
            Some(false) => { args.remove(i); }
            None => i += 1,
        }
    }
}

fn strip_arg_value_occurrences(args: &mut Vec<String>, short_arg: Option<&str>, long_arg: Option<&str>, value: &str) {
    let short_arg = short_arg.map(|v| format!("-{v}"));
    let long_arg = long_arg.map(|v| format!("--{v}"));

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        match () {
            _ if let Some(short_arg_without_prefix) = short_arg.as_deref().and_then(|v| arg.strip_prefix(v)) => {
                let short_arg_inline_value = short_arg_without_prefix.trim_prefix('=');
                match (short_arg_inline_value.is_empty(), args.get(i + 1)) {
                    (false, _) if short_arg_inline_value == value => { args.remove(i); }
                    (true, Some(v)) if v == value => { args.splice(i..=(i + 1), []); }
                    _ => { i += 1; }
                }
            }
            _ if let Some(long_arg_without_prefix) = long_arg.as_deref().and_then(|v| arg.strip_prefix(v)) => {
                let long_arg_inline_value = long_arg_without_prefix.strip_prefix('=');
                match (long_arg_inline_value, args.get(i + 1)) {
                    (Some(v), _) if v == value => { args.remove(i); }
                    (None, Some(v)) if v == value => { args.splice(i..=(i + 1), []); }
                    _ => { i += 1; }
                }
            }
            _ => { i += 1; }
        }
    }
}

#[test]
fn test_strip_arg() {
    let mut args = vec!["--lib".to_owned()];
    strip_arg(&mut args, false, None, Some("lib"));
    assert_eq!(&[] as &[String], &args[..]);

    let mut args = vec!["--lib".to_owned(), "--print".to_owned(), "tests".to_owned()];
    strip_arg(&mut args, false, None, Some("lib"));
    assert_eq!(&["--print".to_owned(), "tests".to_owned()] as &[String], &args[..]);

    let mut args = vec!["--features".to_owned(), "all".to_owned()];
    strip_arg(&mut args, true, None, Some("features"));
    assert_eq!(&[] as &[String], &args[..]);

    let mut args = vec!["--features=all".to_owned()];
    strip_arg(&mut args, true, None, Some("features"));
    assert_eq!(&[] as &[String], &args[..]);

    let mut args = vec!["--features=all".to_owned(), "--metadata-out-root-dir=target/mutest/json".to_owned(), "--print=code".to_owned()];
    strip_arg(&mut args, true, None, Some("features"));
    assert_eq!(&["--metadata-out-root-dir=target/mutest/json".to_owned(), "--print=code".to_owned()] as &[String], &args[..]);
}

#[test]
fn test_strip_arg_value_occurrences() {
    let mut args = vec!["-Z".to_owned(), "write-json-eval-stream".to_owned()];
    strip_arg_value_occurrences(&mut args, Some("Z"), None, "write-json-eval-stream");
    assert_eq!(&[] as &[String], &args[..]);

    let mut args = vec!["-Zwrite-json-eval-stream".to_owned()];
    strip_arg_value_occurrences(&mut args, Some("Z"), None, "write-json-eval-stream");
    assert_eq!(&[] as &[String], &args[..]);

    let mut args = vec!["-Z=write-json-eval-stream".to_owned()];
    strip_arg_value_occurrences(&mut args, Some("Z"), None, "write-json-eval-stream");
    assert_eq!(&[] as &[String], &args[..]);

    let mut args = vec!["-Z".to_owned(), "feature-a".to_owned(), "-Z".to_owned(), "feature-b".to_owned(), "-Z".to_owned(), "feature-c".to_owned()];
    strip_arg_value_occurrences(&mut args, Some("Z"), None, "feature-b");
    assert_eq!(&["-Z".to_owned(), "feature-a".to_owned(), "-Z".to_owned(), "feature-c".to_owned()] as &[String], &args[..]);

    let mut args = vec!["-Z".to_owned(), "feature-b".to_owned(), "-Z".to_owned(), "feature-a".to_owned(), "-Z".to_owned(), "feature-b".to_owned()];
    strip_arg_value_occurrences(&mut args, Some("Z"), None, "feature-b");
    assert_eq!(&["-Z".to_owned(), "feature-a".to_owned()] as &[String], &args[..]);
}

const RUN_UNSTABLE_FLAGS: &[UnstableFlag] = mutest_driver_cli::extend_const_slice!(mutest_driver_cli::UNSTABLE_FLAGS: &[UnstableFlag], &[
    // NOTE: Whenever these change, the `strip_arg_value_occurrences` calls in `run_cargo_with_mutest_driver` have to be updated as well.
    UnstableFlag::new("write-json-eval-stream", Some("During evaluation, write JSONL stream file into JSON output directory specified by `--metadata-out-root-dir`.")),
]);

const RUN_UNSTABLE_OPTIONS: &[UnstableOption] = mutest_driver_cli::extend_const_slice!(mutest_driver_cli::UNSTABLE_OPTIONS: &[UnstableOption], &[
    // No Cargo or evaluation-specific unstable options at the moment.
]);

mod run_isolate {
    mutest_driver_cli::exclusive_opts! { pub(crate) possible_values where
        UNSAFE = "unsafe"; ["Only isolate tests for unsafe mutations."]
        ALL = "all"; ["Isolate tests for all mutations."]
    }
}

mod run_print {
    mutest_driver_cli::opts! { ALL, pub(crate) possible_values where
        DETECTION_MATRIX = "detection-matrix"; ["Print test-mutation detection matrix."]
        SUBSUMPTION_MATRIX = "subsumption-matrix"; ["Print mutation subsumption matrix."]
    }
}

#[cfg(not(windows))]
fn cargo_command_base() -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg(format!("+{}", build::RUST_TOOLCHAIN_VERSION));
    cmd
}

#[cfg(windows)]
fn cargo_command_base() -> Command {
    let mut cmd = Command::new("rustup");
    cmd.arg("run");
    cmd.arg(build::RUST_TOOLCHAIN_VERSION);
    cmd.arg("cargo");
    cmd
}

fn main() {
    let mut args = env::args().collect::<Vec<_>>();
    // NOTE: We determine whether we are
    //       invoked through Cargo as a subcommand ('cargo mutest`) or as a standalone command (`cargo-mutest`)
    //       based on Cargo's behavior of inserting the subcommand name after the binary path for external subcommands,
    //       see https://doc.rust-lang.org/cargo/reference/external-tools.html#custom-subcommands.
    let bin_name = match args.get(1).map(String::as_str) == Some("mutest") {
        true => {
            args.remove(1);
            "cargo mutest"
        }
        false => "cargo-mutest",
    };

    let matches = clap::Command::new("cargo-mutest")
        .bin_name(bin_name)
        .about("Mutation testing tools for Rust")
        .author("Zalán Bálint Lévai")
        .version(mutest_driver_cli::VERSION_STR)
        .styles(mutest_driver_cli::STYLES)
        .propagate_version(true)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .subcommand(mutest_driver_cli::command("run")
            .about("Generate mutations in the specified crate(s) and evaluate the project's tests against them.")
            .next_help_heading("Options")
            .arg(clap::arg!(--"no-emit-metadata" "Do not write JSON metadata files."))
            .arg(clap::arg!(--"no-build" "Do not compile the generated mutation test harness."))
            .arg(clap::arg!(--"no-run" "Do not run the generated mutation test harness."))
            .arg(clap::arg!(--color [WHEN] "Output coloring."))
            .arg(clap::arg!(Z: -Z [FLAG] "Experimental, unstable flags. See `-Z help` for details.").action(clap::ArgAction::Append))
            // NOTE: Whenever these change, the `strip_args` calls in `run_cargo_with_mutest_driver` have to be updated as well.
            .next_help_heading("Evaluation Options")
            .arg(clap::arg!(-i --inspect [INSPECT_OPTS] "Inspect mutations after evaluation. Options may be specified in the form `--inspect=[open][:<PORT>]`.").num_args(0..=1).require_equals(true).conflicts_with_all(["no-run", "no-build", "no-emit-metadata"]))
            // Evaluation-related Arguments
            .arg(clap::arg!(--simulate [MUTATION_ID] "Evaluate tests for a single mutation.").value_parser(clap::value_parser!(u32)).conflicts_with_all(["flakes", "exhaustive", "print"]).conflicts_with_all(["no-run", "no-build"]))
            .arg(clap::arg!(--flakes [ITERATIONS_COUNT] "Perform mutation analysis multiple times to find flaky test-mutation pairs.").value_parser(clap::value_parser!(usize)).conflicts_with_all(["no-run", "no-build"]))
            .arg(clap::arg!(--exhaustive "Evaluate remaining tests, even if the mutation has already been detected by another test.").conflicts_with_all(["no-run", "no-build"]))
            .arg(clap::arg!(--isolate [ISOLATION_MODE] "Isolate tests of mutations into separate processes.").value_parser(run_isolate::possible_values()).default_value(run_isolate::UNSAFE).conflicts_with_all(["no-run", "no-build"]))
            .arg(clap::arg!(--"use-thread-pool" "Evaluate tests in a fixed-size thread pool.").conflicts_with_all(["no-run", "no-build"]))
            .arg(clap::arg!(--"require-progress" "Require protocol-v1 progress before executing tests.").conflicts_with_all(["no-run", "no-build"]))
            // Printing-related Arguments
            .arg(clap::arg!(--"eval-print" [PRINT] "Print additional information during mutation evaluation. Multiple may be specified, separated by commas.").value_delimiter(',').value_parser(run_print::possible_values()).conflicts_with_all(["no-run", "no-build"]))
            // Passed arguments
            .arg(clap::arg!([PASSED_OPTIONS] ...).last(true).conflicts_with_all(["no-run", "no-build"]))
            // Cargo options
            .next_help_heading("Package Selection")
            .arg(clap::arg!(-p --package [PACKAGE] "Test the specified packages.").action(clap::ArgAction::Append))
            .arg(clap::arg!(--workspace "Test all packages in the workspace."))
            .arg(clap::arg!(--exclude [PACKAGE] "Exclude packages from testing.").action(clap::ArgAction::Append))
            .next_help_heading("Target Selection")
            .arg(clap::arg!(--lib "Test only this package's library unit tests."))
            .arg(clap::arg!(--bin [BINARY] "Test only the specified binary. This flag may be specified multiple times.").action(clap::ArgAction::Append))
            .arg(clap::arg!(--bins "Test all binaries."))
            .arg(clap::arg!(--example [EXAMPLE] "Test only the specified example. This flag may be specified multiple times.").action(clap::ArgAction::Append))
            .arg(clap::arg!(--examples "Test all examples."))
            .arg(clap::arg!(--test [TEST] "Test only the specified integration test. This flag may be specified multiple times.").action(clap::ArgAction::Append))
            .arg(clap::arg!(--tests "Test all targets that have the `test = true` manifest flag set."))
            .arg(clap::arg!(--"all-targets" "Test all targets."))
            .next_help_heading("Feature Selection")
            .arg(clap::arg!(-F --features [FEATURES]... "Space or comma separated list of features to activate."))
            .arg(clap::arg!(--"all-features" "Activate all available features."))
            .arg(clap::arg!(--"no-default-features" "Do not activate the `default` feature."))
            .next_help_heading("Compilation Options")
            .arg(clap::arg!(--target [TRIPLE] "Test for the given architecture. The default is the host architecture."))
            .arg(clap::arg!(-r --release "Build artifacts in release mode, with optimizations."))
            .arg(clap::arg!(--profile [PROFILE] "Build artifacts with the specified profile."))
            .arg(clap::arg!(--"target-dir" [TARGET_DIR] "Directory for all generated artifacts.").value_parser(clap::value_parser!(PathBuf)))
            .next_help_heading("Manifest Options")
            .arg(clap::arg!(--"manifest-path" [MANIFEST_PATH] "Path to `Cargo.toml`."))
            .arg(clap::arg!(--locked "Assert that `Cargo.lock` will remain unchanged."))
            .arg(clap::arg!(--offline "Run without accessing the network."))
            .arg(clap::arg!(--frozen "Equivalent to specifying both `--locked` and `--offline`."))
        )
        .next_help_heading("Options")
        // FIXME: Regression; the `help` subcommand can no longer be customized,
        //        so the about text does not match that of the help flags.
        .arg(clap::arg!(-h --help "Print help information; this message or the help of the given subcommand.").action(clap::ArgAction::Help).global(true))
        .arg(clap::arg!(-V --version "Print version information.").action(clap::ArgAction::Version).global(true))
        .after_help(color_print::cstr!("Run `<bright-cyan,bold>cargo mutest run -h</>` to see the options available for generating and evaluating mutations."))
        .after_long_help(color_print::cstr!("Run `<bright-cyan,bold>cargo mutest help run</>` to see the options available for generating and evaluating mutations."))
        .try_get_matches_from(&args)
        .unwrap_or_else(|error| {
            let _ = error.print();
            process::exit(usage_exit_code(&error));
        });

    match matches.subcommand() {
        Some(("run", matches)) => {
            let unstable_flags = matches.get_many::<String>("Z").into_iter().flatten().map(String::as_str).collect::<Vec<_>>();
            if unstable_flags.contains(&"help") {
                mutest_driver_cli::print_unstable_flags_help(RUN_UNSTABLE_FLAGS);
                process::exit(exit_code::SUCCESS);
            }
            mutest_driver_cli::check_unstable_flags(&unstable_flags, RUN_UNSTABLE_FLAGS);
            if !unstable_flags.contains(&"unstable-options") {
                mutest_driver_cli::check_unstable_options(matches, RUN_UNSTABLE_OPTIONS);
            }

            let inspect_opts = match matches.value_source("inspect") {
                Some(clap::parser::ValueSource::CommandLine) if let Some(inspect_opts_str) = matches.get_one::<String>("inspect") => {
                    let mut unparsed_str: &str = inspect_opts_str;

                    let mut open = false;
                    if let Some(rest) = unparsed_str.strip_prefix("open") {
                        open = true;
                        unparsed_str = rest;
                    }
                    if !unparsed_str.is_empty() && !unparsed_str.starts_with(":") {
                        color_print::ceprintln!("<red,bold>error</>: invalid inspect options `{}`: must match `[open][:<<PORT>>]`", inspect_opts_str);
                        process::exit(exit_code::USAGE);
                    }

                    let mut port = None;
                    if let Some(port_str) = unparsed_str.strip_prefix(":") {
                        port = match port_str.parse::<u16>() {
                            Ok(v) => Some(v),
                            Err(_) => {
                                color_print::ceprintln!("<red,bold>error</>: invalid inspect options `{}`: invalid port number `{}`", inspect_opts_str, port_str);
                                process::exit(exit_code::USAGE);
                            }
                        };
                    }

                    Some((open, port))
                }
                Some(clap::parser::ValueSource::CommandLine) => Some((false, None)),
                _ => None,
            };

            // Remove binary path and subcommand from argument list for processing.
            let mut args = &args[2..];
            // Remove passed arguments from argument list for processing.
            if let Some(rest_idx) = args.iter().position(|arg| arg == "--") {
                args = &args[..rest_idx];
            }

            let mut cargo_invocation = process_cargo_args(&args, matches);
            // NOTE: Our target directory lives within the real target directory,
            //       whether specified explicitly through `--target-dir`, or implicitly chosen by Cargo.
            cargo_invocation.target_dir.push("mutest");

            let code = run_cargo_with_mutest_driver(&cargo_invocation, matches, &unstable_flags);
            let code = match inspect_opts {
                Some((open, port)) => inspect_completed_analysis(code, open, port, &cargo_invocation, matches),
                None => code,
            };
            process::exit(code);
        }
        _ => unreachable!(),
    }
}

/// Clap exits 2 on a command line it refuses, which here would say that mutations were missed.
fn usage_exit_code(error: &clap::Error) -> i32 {
    match error.use_stderr() {
        true => exit_code::USAGE,
        false => exit_code::SUCCESS,
    }
}

#[test]
fn a_command_line_that_is_refused_exits_1_and_one_asking_for_help_0() {
    let command = || clap::Command::new("cargo-mutest").arg(clap::arg!(--exhaustive));

    assert_eq!(usage_exit_code(&command().try_get_matches_from(["cargo-mutest", "--exhaustve"]).unwrap_err()), 1);
    assert_eq!(usage_exit_code(&command().try_get_matches_from(["cargo-mutest", "--help"]).unwrap_err()), 0);
}

struct CargoInvocation<'a> {
    cargo_args: Vec<&'a str>,
    non_cargo_args: Vec<String>,
    target_dir: PathBuf,
    explicit_targetings_count: usize,
    selects_library: bool,
}

fn has_library(package: &cargo_metadata::Package) -> bool {
    package.targets.iter().any(|target| target.is_lib() || target.is_rlib() || target.is_dylib() || target.is_cdylib() || target.is_staticlib() || target.is_proc_macro())
}

/// Whether any package the invocation selects has a library, which Cargo requires of `--lib`.
/// A package given as a pattern or a version spec is taken to have one.
fn selects_library(metadata: &cargo_metadata::Metadata, packages: Option<&[&str]>, workspace: bool, excluded: &[&str]) -> bool {
    if let Some(names) = packages {
        return names.iter().any(|name| metadata.packages.iter().find(|package| package.name == *name).is_none_or(has_library));
    }
    let selected = match (workspace, metadata.root_package()) {
        (false, Some(root)) => vec![root],
        (true, _) => metadata.workspace_packages(),
        (false, None) => metadata.workspace_default_packages(),
    };
    selected.into_iter().filter(|package| !excluded.iter().any(|name| package.name == *name)).any(has_library)
}

fn process_cargo_args<'a>(args: &'a [String], matches: &'a clap::ArgMatches) -> CargoInvocation<'a> {
    let mut cargo_args = vec![];
    let mut non_cargo_args = args.to_vec();

    // NOTE: `--color` may be interpreted by the wrapper invoked through Cargo, so we leave it in the non-Cargo args.
    if let Some(color) = matches.get_one::<String>("color") {
        cargo_args.extend(["--color", color]);
    }

    let mut metadata_cmd = cargo_metadata::MetadataCommand::new();

    if let Some(manifest_path) = matches.get_one::<String>("manifest-path") {
        metadata_cmd.manifest_path(manifest_path);
        cargo_args.extend(["--manifest-path", manifest_path]);
        strip_arg(&mut non_cargo_args, true, None, Some("manifest-path"));
    }

    // Package selection.
    if let Some(packages) = matches.get_many::<String>("package") {
        for package in packages { cargo_args.extend(["--package", package]); }
        strip_arg(&mut non_cargo_args, true, Some("p"), Some("package"));
    }
    if matches.get_flag("workspace") {
        cargo_args.push("--workspace");
        strip_arg(&mut non_cargo_args, false, None, Some("workspace"));
    }
    if let Some(packages) = matches.get_many::<String>("exclude") {
        for package in packages { cargo_args.extend(["--exclude", package]); }
        strip_arg(&mut non_cargo_args, true, None, Some("exclude"));
    }

    // Feature selection.
    if let Some(features) = matches.get_many::<String>("features") {
        metadata_cmd.features(cargo_metadata::CargoOpt::SomeFeatures(features.clone().map(ToOwned::to_owned).collect()));
        for feature in features { cargo_args.extend(["--features", feature]); }
        strip_arg(&mut non_cargo_args, true, Some("F"), Some("features"));
    }
    if matches.get_flag("all-features") {
        metadata_cmd.features(cargo_metadata::CargoOpt::AllFeatures);
        cargo_args.push("--all-features");
        strip_arg(&mut non_cargo_args, false, None, Some("all-features"));
    }
    if matches.get_flag("no-default-features") {
        metadata_cmd.features(cargo_metadata::CargoOpt::NoDefaultFeatures);
        cargo_args.push("--no-default-features");
        strip_arg(&mut non_cargo_args, false, None, Some("no-default-features"));
    }

    let cargo_metadata = metadata_cmd.exec().expect("could not retrieve Cargo metadata");

    let target_dir = target_directory(
        matches.get_one::<PathBuf>("target-dir").map(PathBuf::as_path),
        cargo_metadata.target_directory.as_std_path(),
    ).expect("cannot resolve target directory");
    strip_arg(&mut non_cargo_args, true, None, Some("target-dir"));

    if let Some(target) = matches.get_one::<String>("target") {
        cargo_args.extend(["--target", target]);
        strip_arg(&mut non_cargo_args, true, None, Some("target"));
    }

    if matches.get_flag("release") {
        cargo_args.push("--release");
        strip_arg(&mut non_cargo_args, false, Some("r"), Some("release"));
    }
    if let Some(profile) = matches.get_one::<String>("profile") {
        cargo_args.extend(["--profile", profile]);
        strip_arg(&mut non_cargo_args, true, None, Some("profile"));
    }

    // Target selection.
    let mut explicit_targetings_count = 0;
    if matches.get_flag("lib") {
        explicit_targetings_count += 1;
        cargo_args.push("--lib");
        strip_arg(&mut non_cargo_args, false, None, Some("lib"));
    }
    if let Some(bins) = matches.get_many::<String>("bin") {
        for bin in bins {
            explicit_targetings_count += 1;
            cargo_args.extend(["--bin", bin]);
        }
        strip_arg(&mut non_cargo_args, true, None, Some("bin"));
    }
    if matches.get_flag("bins") {
        explicit_targetings_count  += 1;
        cargo_args.push("--bins");
        strip_arg(&mut non_cargo_args, false, None, Some("bins"));
    }
    if let Some(examples) = matches.get_many::<String>("example") {
        for example in examples {
            explicit_targetings_count += 1;
            cargo_args.extend(["--example", example]);
        }
        strip_arg(&mut non_cargo_args, true, None, Some("example"));
    }
    if matches.get_flag("examples") {
        explicit_targetings_count += 1;
        cargo_args.push("--examples");
        strip_arg(&mut non_cargo_args, false, None, Some("examples"));
    }
    if let Some(tests) = matches.get_many::<String>("test") {
        for test in tests {
            explicit_targetings_count += 1;
            cargo_args.extend(["--test", test]);
        }
        strip_arg(&mut non_cargo_args, true, None, Some("test"));
    }
    if matches.get_flag("tests") {
        explicit_targetings_count += 1;
        cargo_args.push("--tests");
        strip_arg(&mut non_cargo_args, false, None, Some("tests"));
    }
    if matches.get_flag("all-targets") {
        explicit_targetings_count += 1;
        cargo_args.push("--all-targets");
        strip_arg(&mut non_cargo_args, false, None, Some("all-targets"));
    }

    if matches.get_flag("locked") {
        cargo_args.push("--locked");
        strip_arg(&mut non_cargo_args, false, None, Some("locked"));
    }
    if matches.get_flag("offline") {
        cargo_args.push("--offline");
        strip_arg(&mut non_cargo_args, false, None, Some("offline"));
    }
    if matches.get_flag("frozen") {
        cargo_args.push("--frozen");
        strip_arg(&mut non_cargo_args, false, None, Some("frozen"));
    }

    let packages = matches.get_many::<String>("package").map(|packages| packages.map(String::as_str).collect::<Vec<_>>());
    let excluded = matches.get_many::<String>("exclude").map(|packages| packages.map(String::as_str).collect::<Vec<_>>()).unwrap_or_default();
    let selects_library = selects_library(&cargo_metadata, packages.as_deref(), matches.get_flag("workspace"), &excluded);

    CargoInvocation { cargo_args, non_cargo_args, target_dir, explicit_targetings_count, selects_library }
}

fn target_directory(selected: Option<&Path>, fallback: &Path) -> std::io::Result<PathBuf> {
    std::path::absolute(selected.unwrap_or(fallback))
}

#[test]
fn a_relative_target_directory_is_made_absolute_as_cargo_runs_drivers_in_package_directories() {
    let root = env::current_dir().unwrap();
    let fallback = root.join("other-target");

    assert_eq!(target_directory(Some(Path::new("target/mutest")), &fallback).unwrap(), root.join("target/mutest"));
    assert_eq!(target_directory(None, &fallback).unwrap(), fallback);
}

/// The files a run's drivers and harnesses report through, named by run so concurrent runs stay apart.
struct RunReports {
    /// Each build that failed, so that sibling compilers Cargo leaves running can stop.
    build_failure_marker: PathBuf,
    /// Each harness's exit code, as Cargo only says whether any of them failed.
    exit_code_log: PathBuf,
}

impl RunReports {
    fn new(target_dir: &Path, run_id: u32) -> Self {
        Self {
            build_failure_marker: target_dir.join(format!("build-failure-{run_id}")),
            exit_code_log: target_dir.join(format!("exit-codes-{run_id}")),
        }
    }

    fn pass_to(&self, cmd: &mut Command) {
        let _ = fs::remove_file(&self.build_failure_marker);
        let _ = fs::remove_file(&self.exit_code_log);
        cmd.env("MUTEST_BUILD_FAILURE_MARKER", &self.build_failure_marker);
        cmd.env(exit_code::LOG_VAR, &self.exit_code_log);
    }

    /// Reads and removes both reports, naming every build that failed.
    fn exit_code(self, cargo_exit_code: Option<i32>) -> i32 {
        let marker_contents = take_file(&self.build_failure_marker);
        let exit_code_log = take_file(&self.exit_code_log);

        let failed_builds = failed_builds_named_in(&marker_contents);
        for failed_build in &failed_builds {
            color_print::ceprintln!("<red,bold>error</>: {} did not build, so none of its mutations were evaluated", failed_build);
        }

        run_exit_code(cargo_exit_code, &failed_builds, &exit_code::read(&exit_code_log))
    }
}

fn take_file(path: &Path) -> String {
    let contents = fs::read_to_string(path).unwrap_or_default();
    let _ = fs::remove_file(path);
    contents
}

#[test]
fn each_run_reports_through_its_own_files_inside_the_target_directory() {
    let reports = RunReports::new(Path::new("target/mutest"), 4321);

    assert_eq!(reports.build_failure_marker, PathBuf::from("target/mutest/build-failure-4321"));
    assert_eq!(reports.exit_code_log, PathBuf::from("target/mutest/exit-codes-4321"));
    assert_ne!(RunReports::new(Path::new("target/mutest"), 8765).build_failure_marker, reports.build_failure_marker);
}

/// The driver binary's size and modification time, which change whenever it is rebuilt.
fn driver_stamp(driver: &Path) -> String {
    let Ok(metadata) = fs::metadata(driver) else { return String::new(); };
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).unwrap_or_default();
    format!("{}-{}", metadata.len(), modified.as_nanos())
}

#[test]
fn a_rebuilt_driver_has_another_stamp() {
    let driver = env::temp_dir().join(format!("mutest-driver-stamp-{}", process::id()));
    fs::write(&driver, "old").unwrap();
    let old = driver_stamp(&driver);
    fs::write(&driver, "rebuilt").unwrap();
    let rebuilt = driver_stamp(&driver);
    fs::remove_file(&driver).unwrap();

    assert_ne!(old, rebuilt);
    assert_eq!(driver_stamp(&driver), "");
}

fn is_harness_internal_var(var: &OsStr) -> bool {
    var.as_encoded_bytes().starts_with(b"__MUTEST_")
}

/// A test that runs `cargo mutest` must not pass its own harness's variables on to this run.
fn remove_harness_internal_vars(cmd: &mut Command) {
    for (var, _) in env::vars_os().filter(|(var, _)| is_harness_internal_var(var)) {
        cmd.env_remove(var);
    }
}

#[test]
fn a_harness_s_own_variables_are_kept_from_cargo() {
    let vars = ["__MUTEST_RUN_AS_LIBTEST", "MUTEST_EXIT_CODE_LOG", "PATH", "__MUTEST_JOURNAL"];

    assert_eq!(vars.map(|var| is_harness_internal_var(var.as_ref())), [true, false, false, true]);
}

/// The directory and nonce that `--require-progress` has the harness write its progress under.
fn progress_environment(directory: Option<&OsStr>, nonce: Option<&OsStr>) -> Result<(), &'static str> {
    let absolute_directory = directory.is_some_and(|directory| Path::new(directory).is_absolute());
    let lowercase_hex_nonce = nonce.and_then(OsStr::to_str)
        .is_some_and(|nonce| nonce.len() == 32 && nonce.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    match absolute_directory && lowercase_hex_nonce {
        true => Ok(()),
        false => Err("--require-progress needs an absolute MUTEST_PROGRESS_DIR and a MUTEST_PROGRESS_NONCE of 32 lowercase hex digits"),
    }
}

#[test]
fn required_progress_needs_an_absolute_directory_and_a_lowercase_hex_nonce() {
    let nonce = OsStr::new("0123456789abcdef0123456789abcdef");
    let absolute = env::temp_dir();
    let absolute = Some(absolute.as_os_str());

    assert!(progress_environment(absolute, Some(nonce)).is_ok());
    for (directory, nonce) in [(None, None), (absolute, None),
        (Some(OsStr::new("relative")), Some(nonce)),
        (absolute, Some(OsStr::new("0123456789ABCDEF0123456789ABCDEF")))] {
        assert!(progress_environment(directory, nonce).is_err());
    }
}

fn run_cargo_with_mutest_driver(cargo_invocation: &CargoInvocation, matches: &clap::ArgMatches, unstable_flags: &[&str]) -> i32 {
    if matches.get_flag("require-progress")
        && let Err(error) = progress_environment(env::var_os("MUTEST_PROGRESS_DIR").as_deref(), env::var_os("MUTEST_PROGRESS_NONCE").as_deref())
    {
        color_print::ceprintln!("<red,bold>error</>: {}", error);
        return exit_code::USAGE;
    }
    let mut mutest_args = cargo_invocation.non_cargo_args.clone();

    let no_run = matches.get_flag("no-run");
    if no_run { strip_arg(&mut mutest_args, false, None, Some("no-run")); }

    let no_build = matches.get_flag("no-build");
    if no_build { strip_arg(&mut mutest_args, false, None, Some("no-build")); }

    let (cargo_subcommand, cargo_args): (_, &[&str]) = match () {
        _ if no_build => ("check", &["--profile", "test"]),
        _ if no_run => ("test", &["--no-run"]),
        _ => ("test", &["--no-fail-fast"]),
    };

    let mut mutest_driver_outputs = vec!["info"];
    if !no_build { mutest_driver_outputs.push("test-bin") }

    let mut cmd = cargo_command_base();
    remove_harness_internal_vars(&mut cmd);

    let embedded = unstable_flags.contains(&"embedded");
    if embedded {
        let target = match matches.get_one::<String>("target") {
            Some(target) => target.clone(),
            None => {
                let mut config_cmd = cargo_command_base();
                config_cmd.arg("-Zunstable-options");
                config_cmd.args(["config", "get"]);
                config_cmd.arg("--format=json-value");
                config_cmd.arg("build.target");

                let output = config_cmd.output().expect("failed to run Cargo");

                if !output.status.success() {
                    color_print::ceprintln!("<red,bold>error</>: target must be specified when using the embedded mutation runtime");
                    color_print::ceprintln!("       consider specifying `build.target` in `.cargo/config.toml` or using the `--target` option");
                    process::exit(exit_code::USAGE);
                }

                let stdout_str = str::from_utf8(&output.stdout).expect("invalid Cargo output");
                let val_str = stdout_str
                    .lines().last().expect("invalid Cargo output")
                    .strip_prefix('\"').and_then(|s| s.strip_suffix('\"')).expect("invalid Cargo output");

                val_str.to_owned()
            }
        };

        let mut runner_path = env::current_exe().expect("current executable path invalid");
        runner_path.set_file_name("mutest-runtime-embedded-host-driver");
        if cfg!(windows) { runner_path.set_extension("exe"); }

        match fs::exists(&runner_path) {
            Ok(true) => {}
            // NOTE: We let Cargo emit its usual error message upon being unable to run the runner executable.
            Err(_) => {}

            Ok(false) => {
                color_print::ceprintln!("<red,bold>error</>: cannot find mutest-rs embedded runtime host driver");
                color_print::ceprintln!("       consider running `cargo install --force --path mutest-runtime-embedded-host-driver` in the mutest-rs source tree");
                process::exit(exit_code::USAGE);
            }
        }

        // Override test bin runner for the target, replacing it with our embedded mutation runtime host driver,
        // which flashes the test binary and drives the mutation evaluation on embedded targets.
        // NOTE: This must be specified for the specific target triple,
        //       as those override any `cfg(true)` specifications.
        cmd.arg("--config");
        cmd.arg(format!("target.{target}.runner=\"{}\"", runner_path.display()));
    }

    cmd.arg(cargo_subcommand);
    cmd.args(cargo_args);

    let cargo_verbosity = env::var("CARGO_VERBOSITY").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or_default();
    cmd.args((0..cargo_verbosity).map(|_| "-v"));

    if let Some(color) = matches.get_one::<String>("color") {
        strip_arg(&mut mutest_args, true, None, Some("color"));
        if color == "never" {
            // HACK: Indicate to mutest-driver that the explicit `--color=never` flag was passed.
            // NOTE: This is needed because Cargo always captures colored output, and
            //       the Cargo --color flag only controls whether the coloring is stripped before final output.
            //       This is also required for users because Cargo's rustc invocations disallow using RUSTFLAGS="--color=never".
            cmd.env("MUTEST_CARGO_EXPLICIT_NO_COLOR", "1");
        }
    }

    cmd.arg("--target-dir");
    cmd.arg(&cargo_invocation.target_dir);
    cmd.env("MUTEST_TARGET_DIR_ROOT", &cargo_invocation.target_dir);

    let run_reports = RunReports::new(&cargo_invocation.target_dir, process::id());
    run_reports.pass_to(&mut cmd);

    cmd.args(&cargo_invocation.cargo_args);

    // Explicitly specify supported targets if none was selected.
    if cargo_invocation.explicit_targetings_count == 0 {
        // NOTE: We specifically do not target the following:
        //       * `--bench`/`--benches`: Benchmarks, for two reasons.
        //         First, the `#[bench]` attribute is currently a nightly-only feature.
        //         Second, the semantics of running benchmarks under mutation testing
        //         are not fully clear.
        //       * `--doc`: Documentation tests, as they require a completely different
        //         compilation and evaluation strategy that we do not currently support.
        cmd.args(cargo_invocation.selects_library.then_some("--lib"));
        cmd.args(["--bins", "--examples", "--tests"]);
    }

    let mut path = env::current_exe().expect("current executable path invalid");
    path.set_file_name("mutest-driver");
    if cfg!(windows) { path.set_extension("exe"); }
    // NOTE: Cargo does not rebuild after the compiler wrapper changes, so each crate depends on the driver's stamp.
    cmd.env("MUTEST_DRIVER_STAMP", driver_stamp(&path));
    cmd.env("RUSTC_WORKSPACE_WRAPPER", path);

    if matches.get_flag("no-emit-metadata") {
        strip_arg(&mut mutest_args, false, None, Some("no-emit-metadata"));
    } else {
        mutest_driver_outputs.push("metadata");
    }
    mutest_args.push(format!("--emit={}", mutest_driver_outputs.join(",")));

    // Prevent evaluation arguments from being passed to mutest-driver.
    strip_arg(&mut mutest_args, false, None, Some("inspect"));
    strip_arg(&mut mutest_args, true, None, Some("simulate"));
    strip_arg(&mut mutest_args, true, None, Some("flakes"));
    strip_arg(&mut mutest_args, false, None, Some("exhaustive"));
    strip_arg(&mut mutest_args, true, None, Some("isolate"));
    strip_arg(&mut mutest_args, false, None, Some("use-thread-pool"));
    strip_arg(&mut mutest_args, false, None, Some("require-progress"));
    strip_arg(&mut mutest_args, true, None, Some("eval-print"));
    strip_arg_value_occurrences(&mut mutest_args, Some("Z"), None, "write-json-eval-stream");

    cmd.env("MUTEST_ENCODED_ARGS", mutest_args.join("\x1F"));

    if !no_build && !no_run {
        cmd.arg("--");

        let parallel_mutants = matches.get_flag("parallel-mutants");

        if let Some(mutation_id) = matches.get_one::<u32>("simulate") { cmd.arg(format!("--simulate={mutation_id}")); }
        if let Some(iterations_count) = matches.get_one::<usize>("flakes") { cmd.arg(format!("--flakes={iterations_count}")); }

        if matches.get_flag("exhaustive") { cmd.arg("--exhaustive"); }
        cmd.args(matches.get_flag("require-progress").then_some("--require-progress"));

        if !embedded {
            if let Some(isolation_mode) = matches.get_one::<String>("isolate") { cmd.arg(format!("--isolate={isolation_mode}")); }
            // NOTE: `--parallel-mutants` requires the test thread pool, so we automatically set it.
            if matches.get_flag("use-thread-pool") || parallel_mutants { cmd.arg("--use-thread-pool"); }
        }

        let mut print_names = matches.get_many::<String>("eval-print").map(|print| print.map(String::as_str).collect::<HashSet<_>>()).unwrap_or_default();
        if print_names.contains("all") { print_names = HashSet::from_iter(run_print::ALL.into_iter().map(|s| *s)); }
        for print_name in print_names { cmd.arg(format!("--print={print_name}")); }

        cmd.args((0..matches.get_count("verbose")).map(|_| "-v"));
        if matches.get_flag("timings") { cmd.arg("--timings"); }
        if !matches.get_flag("no-emit-metadata") {
            let out_dir = matches.get_one::<PathBuf>("metadata-out-root-dir").cloned().unwrap_or_else(|| cargo_invocation.target_dir.join("json"));
            fs::create_dir_all(&out_dir).expect(&format!("cannot create JSON metadata output directory at `{}`", out_dir.display()));
            // NOTE: The out dir path passed to the generated test binary must be canonicalized,
            //       as it will likely be run under a different cwd.
            let out_dir = out_dir.canonicalize().expect("cannot canonicalize JSON metadata output directory path");
            let out_dir = out_dir.as_os_str().to_str().expect("non-UTF-8 path");
            cmd.arg(format!("--metadata-out-root-dir={out_dir}"));
        }

        if unstable_flags.contains(&"write-json-eval-stream") { cmd.arg("--Zwrite-json-eval-stream"); }

        cmd.args(matches.get_many::<String>("PASSED_OPTIONS").unwrap_or_default());

        // NOTE: Disable insta snapshot creation for mutated program tests.
        cmd.env("INSTA_UPDATE", "no");
    }

    let exit_status = cmd
        .spawn().expect("failed to run Cargo")
        .wait().expect("failed to run Cargo");

    run_reports.exit_code(exit_status.code())
}

fn failed_builds_named_in(marker_contents: &str) -> Vec<&str> {
    marker_contents.lines().filter(|line| !line.is_empty()).collect()
}

#[test]
fn every_build_a_driver_recorded_as_failed_is_named() {
    let marker_contents = "the test harness of `krate` in package `krate`\nthe test harness of `cli` in package `krate`\n";

    assert_eq!(failed_builds_named_in(marker_contents), ["the test harness of `krate` in package `krate`", "the test harness of `cli` in package `krate`"]);
    assert_eq!(failed_builds_named_in(""), [] as [&str; 0]);
}

fn run_exit_code(cargo_exit_code: Option<i32>, failed_builds: &[&str], harness_exit_codes: &[i32]) -> i32 {
    if !failed_builds.is_empty() { return exit_code::BASELINE_FAILED; }
    match cargo_exit_code {
        Some(exit_code::SUCCESS) => exit_code::SUCCESS,
        // Cargo exits 101 however a test binary failed; each harness recorded how.
        Some(101) if !harness_exit_codes.is_empty() => match exit_code::worst(harness_exit_codes.iter().copied()) {
            exit_code::SUCCESS => exit_code::PANIC,
            code => code,
        },
        // A build no driver took part in, such as a dependency's, failed before any harness ran.
        Some(101) => exit_code::BASELINE_FAILED,
        Some(code) => code,
        None => exit_code::PANIC,
    }
}

#[test]
fn a_run_whose_harnesses_caught_every_mutation_exits_0() {
    assert_eq!(run_exit_code(Some(0), &[], &[exit_code::SUCCESS, exit_code::SUCCESS]), 0);
    assert_eq!(run_exit_code(Some(0), &[], &[]), 0);
}

#[test]
fn a_run_given_an_argument_it_cannot_use_exits_1() {
    assert_eq!(run_exit_code(Some(101), &[], &[exit_code::SUCCESS, exit_code::USAGE]), 1);
    assert_eq!(run_exit_code(Some(1), &[], &[]), 1);
}

#[test]
fn a_run_whose_tests_missed_a_mutation_exits_2() {
    assert_eq!(run_exit_code(Some(101), &[], &[exit_code::SUCCESS, exit_code::MISSED]), 2);
}

#[test]
fn a_run_in_which_a_mutation_timed_out_exits_3_whatever_else_was_missed() {
    assert_eq!(run_exit_code(Some(101), &[], &[exit_code::TIMED_OUT]), 3);
    assert_eq!(run_exit_code(Some(101), &[], &[exit_code::MISSED, exit_code::TIMED_OUT]), 3);
}

#[test]
fn a_run_whose_harness_did_not_build_or_whose_tests_failed_unmutated_exits_4() {
    let failed_builds = ["the test harness of `krate` in package `krate`"];

    assert_eq!(run_exit_code(Some(101), &failed_builds, &[]), 4);
    assert_eq!(run_exit_code(Some(101), &[], &[]), 4);
    assert_eq!(run_exit_code(Some(101), &[], &[exit_code::MISSED, exit_code::BASELINE_FAILED]), 4);
}

#[test]
fn a_run_in_which_a_harness_panicked_or_ended_abnormally_exits_101() {
    assert_eq!(run_exit_code(Some(101), &[], &[exit_code::MISSED, exit_code::PANIC]), 101);
    assert_eq!(run_exit_code(Some(101), &[], &[128 + 9]), 101);
    assert_eq!(run_exit_code(Some(101), &[], &exit_code::read("started 4321\n")), 101);
    assert_eq!(run_exit_code(Some(101), &[], &[exit_code::SUCCESS]), 101);
    assert_eq!(run_exit_code(None, &[], &[]), 101);
}

/// Opens the inspector once an analysis completed; an inspector failure takes over the exit code.
fn inspect_completed_analysis(code: i32, open: bool, port: Option<u16>, cargo_invocation: &CargoInvocation, matches: &clap::ArgMatches) -> i32 {
    if !exit_code::analysis_completed(code) { return code; }
    match run_mutest_inspector_from_cargo_invocation(open, port, cargo_invocation, matches) {
        exit_code::SUCCESS => code,
        inspector_code => inspector_code,
    }
}

fn run_mutest_inspector_from_cargo_invocation(open: bool, port: Option<u16>, cargo_invocation: &CargoInvocation, matches: &clap::ArgMatches) -> i32 {
    // NOTE: This replicates Cargo's action message styling, including the color and justification.
    color_print::ceprintln!("<green,bold>{:>12}</> inspector", "Running");

    let metadata_root_dir = matches.get_one::<PathBuf>("metadata-out-root-dir").cloned().unwrap_or_else(|| cargo_invocation.target_dir.join("json"));

    let mut path = env::current_exe().expect("current executable path invalid");
    path.set_file_name("mutest-inspector");
    if cfg!(windows) { path.set_extension("exe"); }

    let mut cmd = Command::new(path);

    cmd.arg("--metadata-root-dir");
    cmd.arg(metadata_root_dir);

    if let Some(port) = port { cmd.arg(format!("--port={port}")); }

    if open {
        // NOTE: Only attempt to open a specific target if only one was selected.
        let mut packages_iter = matches.get_many::<String>("package").into_iter().flatten();
        if let Some(package) = packages_iter.next() && let None = packages_iter.next() {
            if cargo_invocation.explicit_targetings_count == 1 {
                match () {
                    _ if matches.get_flag("lib") => { cmd.arg(format!("--open={}/lib", package)); }
                    _ if let Some(target_name) = matches.get_one::<String>("bin") => { cmd.arg(format!("--open={}/bin:{}", package, target_name)); }
                    _ if let Some(target_name) = matches.get_one::<String>("example") => { cmd.arg(format!("--open={}/example:{}", package, target_name)); }
                    _ if let Some(target_name) = matches.get_one::<String>("test") => { cmd.arg(format!("--open={}/test:{}", package, target_name)); }
                    // NOTE: Only open the package if no specific target was selected.
                    //       This includes generic selectors, such as `--bins`, `--examples`, `--tests`, and `--all-targets`.
                    _ => { cmd.arg(format!("--open={}", package)); }
                }
            } else {
                // NOTE: Only open the package if no specific (none or multiple) target was selected.
                cmd.arg(format!("--open={}", package));
            }
        } else {
            cmd.arg("--open");
        }
    }

    let exit_status = cmd
        .spawn().expect("failed to run mutest-inspector")
        .wait().expect("failed to run mutest-inspector");

    exit_status.code().unwrap_or(exit_code::PANIC)
}
