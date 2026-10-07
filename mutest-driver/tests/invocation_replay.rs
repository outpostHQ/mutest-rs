#![cfg(unix)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A source directory, and an output directory outside it.
struct Fixture {
    source: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let name = format!("invocation-replay-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-scratch").join(&name);
        let output = env::temp_dir().join(&name);
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        Self { source, output }
    }

    /// The driver, run in `directory` with only the environment a compiler needs.
    fn driver(&self, directory: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mutest-driver"));
        command.env_clear().current_dir(directory).env("TMPDIR", &self.output);
        for name in ["PATH", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
            if let Some(value) = env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    fn compile_library(&self, source: &str, emit: &str) -> Output {
        fs::write(self.source.join("lib.rs"), source).unwrap();
        self.driver(&self.source)
            .args(["lib.rs", "--crate-name", "fixture", "--crate-type", "lib", "--edition=2024", "--emit", emit, "--out-dir"])
            .arg(&self.output)
            .env("MUTEST_ENCODED_ARGS", "--crate-kind=mutable-dep-for-external-tests")
            .env("REQUIRED", "required-value")
            .env("UNRELATED", "unrelated-value")
            .output()
            .unwrap()
    }

    fn record(&self, artifact: &str) -> serde_json::Value {
        let path = self.output.join(".mutest-invocations").join(format!("{artifact}.json"));
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.output);
        let _ = fs::remove_dir_all(&self.source);
    }
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn only_the_environment_the_crate_read_is_recorded() {
    let fixture = Fixture::new();
    let source = "pub fn required() -> &'static str { env!(\"REQUIRED\") }\npub fn absent() -> Option<&'static str> { option_env!(\"ABSENT\") }\n";

    assert_success(&fixture.compile_library(source, "metadata,link"));

    let record = fixture.record("libfixture.rmeta");
    let values = record["invocation"]["env_vars"].as_array().unwrap();
    assert!(values.contains(&serde_json::json!(["REQUIRED", "required-value"])));
    assert!(values.contains(&serde_json::json!(["ABSENT", null])));
    assert!(!values.iter().any(|value| value[0] == "UNRELATED"));
    for artifact in ["libfixture.rmeta", "libfixture.rlib"] {
        let bytes = fs::read(fixture.output.join(artifact)).unwrap();
        assert!(!bytes.windows(b"unrelated-value".len()).any(|window| window == b"unrelated-value"));
    }
}

/// A build script's probe inherits its package's Cargo variables, but Cargo names no crate for it.
#[test]
fn a_build_script_probe_compiles_as_plain_rustc() {
    let fixture = Fixture::new();
    let manifest = fixture.source.join("Cargo.toml");
    fs::write(&manifest, "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"lib.rs\"\n\n[workspace]\n").unwrap();
    fs::write(fixture.source.join("lib.rs"), "pub fn value() -> u8 { 42 }").unwrap();
    fs::write(fixture.source.join("probe.rs"), "pub fn probe() {}").unwrap();

    let output = fixture.driver(&fixture.source)
        .args(["probe.rs", "--crate-name", "probe", "--crate-type=lib", "--edition=2024", "--emit=metadata", "--out-dir"])
        .arg(&fixture.output)
        .env("CARGO_MANIFEST_PATH", &manifest)
        .env("CARGO_PKG_NAME", "fixture")
        .output()
        .unwrap();

    assert_success(&output);
    assert!(fixture.output.join("libprobe.rmeta").exists());
}

#[test]
fn a_record_is_written_beside_each_artifact_in_clean_and_warm_output_directories() {
    let fixture = Fixture::new();
    for _ in 0..2 {
        assert_success(&fixture.compile_library("pub fn value() -> u8 { 42 }", "link"));

        let record = fixture.record("libfixture.rlib");
        assert_eq!(record["artifact"], fs::canonicalize(fixture.output.join("libfixture.rlib")).unwrap().to_str().unwrap());
        assert!(!fixture.output.join(".mutest-invocations/libfixture.rmeta.json").exists());
    }
}

#[test]
fn a_recorded_crate_builds_its_own_test_harness() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("lib.rs"), "#![forbid(missing_docs)]\n//! Recorded crate.\n").unwrap();

    let output = fixture.driver(&fixture.source)
        .args(["lib.rs", "--crate-name", "fixture", "--crate-type=lib", "--test", "--edition=2024", "--out-dir"])
        .arg(&fixture.output)
        .env("MUTEST_ENCODED_ARGS", "--crate-kind=mutable-dep-for-external-tests\u{1f}--emit=info,test-bin")
        .output()
        .unwrap();
    assert_success(&output);

    let output = Command::new(fixture.output.join("fixture")).env_clear().arg("--list").output().unwrap();
    assert_success(&output);
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "0 tests, 0 benchmarks");
}

#[test]
fn a_mutant_crate_is_compiled_again_with_its_recorded_environment() {
    let fixture = Fixture::new();
    let source = "pub fn answer() -> u8 { 42 }\npub fn required() -> &'static str { env!(\"REQUIRED\") }\npub fn absent() -> Option<&'static str> { option_env!(\"ABSENT\") }\n";
    assert_success(&fixture.compile_library(source, "metadata,link"));
    fs::write(fixture.source.join("integration.rs"), "#[test] fn direct() { assert_eq!(fixture::answer(), 42); assert_eq!(fixture::required(), \"required-value\"); assert_eq!(fixture::absent(), None); }").unwrap();
    fs::create_dir_all(fixture.output.join("metadata")).unwrap();

    let output = fixture.driver(&fixture.source)
        .args(["integration.rs", "--test", "--crate-name", "integration", "--edition=2024", "--extern"])
        .arg(format!("fixture={}", fixture.output.join("libfixture.rlib").display()))
        .arg("-L").arg(format!("dependency={}", fixture.output.display()))
        .arg("--out-dir").arg(&fixture.output)
        .env("MUTEST_ENCODED_ARGS", format!("--crate-kind=integration-tests\u{1f}--emit=all\u{1f}--metadata-out-root-dir={}", fixture.output.join("metadata").display()))
        .env("MUTEST_TARGET_DIR_ROOT", fixture.output.join("mutest"))
        .env("REQUIRED", "ambient-value")
        .env("ABSENT", "ambient-presence")
        .output()
        .unwrap();
    assert_success(&output);

    let output = Command::new(fixture.output.join("integration")).env_clear().env("TMPDIR", &fixture.output).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(2), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("6 detected (0 timed out; 0 crashed); 1 undetected; 7 total"), "{stdout}");
    assert!(stdout.contains("[fn_return_default] return `0` without evaluating the function body at lib.rs:1:1"), "{stdout}");
    assert!(stdout.contains("[fn_return_default] return `1` without evaluating the function body at lib.rs:1:1"), "{stdout}");
    // NOTE: `option_env!` expands to a constant, so returning `None` changes nothing.
    let undetected = stdout.split("mutation was not detected").skip(1).collect::<Vec<_>>();
    assert!(matches!(&undetected[..], [warning] if warning.contains("return `None` without evaluating the function body")), "{stdout}");
}

#[test]
fn a_test_reaching_the_library_only_through_a_dependent_detects_its_mutations() {
    diamond(Diamond::default());
}

#[test]
fn a_type_shared_through_a_dependent_comes_from_the_specialized_library() {
    diamond(Diamond { shared_type: true, ..Diamond::default() });
}

#[test]
fn a_registry_dependency_of_a_dependent_needs_no_record() {
    diamond(Diamond { registry: true, ..Diamond::default() });
}

#[test]
fn a_dependent_without_a_record_is_refused() {
    diamond(Diamond { registry: true, missing_record: true, ..Diamond::default() });
}

#[test]
fn a_dependent_is_rebuilt_against_split_metadata() {
    diamond(Diamond { registry: true, split_metadata: true, ..Diamond::default() });
}

#[derive(Default)]
struct Diamond {
    shared_type: bool,
    registry: bool,
    missing_record: bool,
    split_metadata: bool,
}

/// Library `a`, its dependent `b`, and a test of `a` that calls `a` through `b`.
fn diamond(case: Diamond) {
    let fixture = Fixture::new();
    let workspace = fixture.source.join("diamond");
    fs::create_dir_all(workspace.join("a/src")).unwrap();
    fs::create_dir_all(workspace.join("a/tests")).unwrap();
    fs::create_dir_all(workspace.join("b/src")).unwrap();
    fs::write(workspace.join("Cargo.toml"), "[workspace]\nmembers=[\"a\",\"b\"]\nresolver=\"3\"\n").unwrap();
    fs::write(workspace.join("a/Cargo.toml"), "[package]\nname=\"diamond-a\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[dev-dependencies]\ndiamond-b={path=\"../b\"}\n").unwrap();
    let registry_dependency = if case.registry { "itoa=\"=1.0.18\"\n" } else { "" };
    fs::write(workspace.join("b/Cargo.toml"), format!("[package]\nname=\"diamond-b\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[dependencies]\ndiamond-a={{path=\"../a\"}}\n{registry_dependency}")).unwrap();
    let (a, b, test) = match (case.shared_type, case.registry) {
        (true, _) => (
            "pub struct Shared(pub u8); pub fn answer()->u8 {42}\n",
            "pub fn indirect()->u8 {diamond_a::answer()} pub fn shared(value:diamond_a::Shared)->diamond_a::Shared {value}\n",
            "#[test] fn indirect(){assert_eq!(diamond_a::answer(),42); assert_eq!(diamond_b::indirect(),42); let value:diamond_a::Shared=diamond_b::shared(diamond_a::Shared(7)); assert_eq!(value.0,7);}\n",
        ),
        (false, true) => (
            "pub fn answer()->u8 {42}\n",
            "pub fn indirect()->u8 { let mut buffer=itoa::Buffer::new(); assert_eq!(buffer.format(42u8),\"42\"); diamond_a::answer() }\n",
            "#[test] fn indirect(){assert_eq!(diamond_b::indirect(),42);}\n",
        ),
        (false, false) => (
            "pub fn answer()->u8 {42}\n",
            "pub fn indirect()->u8 {diamond_a::answer()}\n",
            "#[test] fn indirect(){assert_eq!(diamond_b::indirect(),42);}\n",
        ),
    };
    fs::write(workspace.join("a/src/lib.rs"), a).unwrap();
    fs::write(workspace.join("b/src/lib.rs"), b).unwrap();
    fs::write(workspace.join("a/tests/indirect.rs"), test).unwrap();

    let mut cargo = Command::new("cargo");
    cargo.env_clear().current_dir(&workspace)
        .args(["test", "--offline", "-p", "diamond-a", "--test", "indirect", "--no-run", "--message-format=json"])
        .env("RUSTC_WORKSPACE_WRAPPER", env!("CARGO_BIN_EXE_mutest-driver"))
        .env("MUTEST_ENCODED_ARGS", "--emit=test-bin")
        .env("MUTEST_TARGET_DIR_ROOT", fixture.output.join("runtime"))
        .env("CARGO_TARGET_DIR", fixture.output.join("target"))
        .env("CARGO_BUILD_JOBS", "2")
        .env("TMPDIR", &fixture.output)
        .env("RUSTFLAGS", if case.split_metadata { "-Zembed-metadata=no" } else { "-Zembed-metadata=yes" });
    for name in ["PATH", "HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "CARGO_HOME", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
        if let Some(value) = env::var_os(name) {
            cargo.env(name, value);
        }
    }
    let clean = cargo.output().unwrap();
    assert_success(&clean);
    let messages = String::from_utf8_lossy(&clean.stdout).lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()).collect::<Vec<_>>();
    let filenames = |target: &str| {
        let artifact = messages.iter().find(|message| message["target"]["name"] == target).unwrap();
        artifact["filenames"].as_array().unwrap().iter().map(|filename| PathBuf::from(filename.as_str().unwrap())).collect::<Vec<_>>()
    };

    // NOTE: Another build of `a` in the same directory must not be picked over the recorded one.
    for path in filenames("diamond_a") {
        fs::copy(&path, path.with_file_name(format!("libdiamond_a-stale.{}", path.extension().unwrap().to_str().unwrap()))).unwrap();
    }
    if case.missing_record {
        for path in filenames("diamond_b") {
            fs::remove_file(path.parent().unwrap().join(".mutest-invocations").join(format!("{}.json", path.file_name().unwrap().to_str().unwrap()))).unwrap();
        }
    }

    // Build the test again, against the artifacts of the first build.
    let integration_source = workspace.join("a/tests/indirect.rs");
    fs::write(&integration_source, fs::read(&integration_source).unwrap()).unwrap();
    let warm = cargo.output().unwrap();
    if case.missing_record {
        assert!(!warm.status.success());
        assert!(String::from_utf8_lossy(&warm.stdout).contains("affected dependency diamond_b needs valid replay metadata"), "{}", String::from_utf8_lossy(&warm.stdout));
        return;
    }
    assert_success(&warm);

    let executable = messages.iter().find_map(|message| (message["target"]["name"] == "indirect").then(|| message["executable"].as_str()).flatten()).unwrap();
    let output = Command::new(executable).env_clear().env("TMPDIR", &fixture.output).output().unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 detected (0 timed out; 0 crashed); 0 undetected; 2 total"), "{stdout}");
    assert!(stdout.contains("[fn_return_default] return `0` without evaluating the function body"), "{stdout}");
    assert!(stdout.contains("[fn_return_default] return `1` without evaluating the function body"), "{stdout}");
}

#[test]
fn a_dependent_is_rebuilt_with_split_extern_arguments() {
    rebuild_case(ExternSpelling::Split, false);
}

#[test]
fn a_dependent_is_rebuilt_with_joined_extern_arguments() {
    rebuild_case(ExternSpelling::Joined, false);
}

#[test]
fn a_dependent_is_rebuilt_with_split_relative_extern_paths() {
    rebuild_case(ExternSpelling::SplitRelative, false);
}

#[test]
fn a_dependent_is_rebuilt_with_joined_relative_extern_paths() {
    rebuild_case(ExternSpelling::JoinedRelative, false);
}

#[test]
fn a_dependent_is_rebuilt_when_its_directory_and_crate_already_carry_the_suffix() {
    rebuild_case(ExternSpelling::Split, true);
}

#[derive(Clone, Copy, PartialEq)]
enum ExternSpelling {
    Split,
    Joined,
    SplitRelative,
    JoinedRelative,
}

/// A library, a dependent spelling its `--extern` as given, and a test of both, compiled by hand.
fn rebuild_case(spelling: ExternSpelling, repeated_suffix: bool) {
    let fixture = Fixture::new();
    let output_dir = fixture.output.join(if repeated_suffix { "artifacts-for-integration" } else { "artifacts" });
    fs::create_dir_all(output_dir.join("spelling")).unwrap();
    let original_extra = if repeated_suffix { "-for-integration" } else { "" };
    let original_name = format!("libfixture{original_extra}.rlib");
    let original = output_dir.join(&original_name);
    let driver = || {
        let mut command = fixture.driver(&output_dir);
        command.args(["--edition=2024", "--emit=metadata,link", "--out-dir"]).arg(&output_dir)
            .env("MUTEST_ENCODED_ARGS", "--crate-kind=mutable-dep-for-external-tests\u{1f}--emit=test-bin");
        command
    };

    let library = fixture.source.join("fixture.rs");
    fs::write(&library, "pub struct Shared(pub u8); pub fn answer() -> u8 { 42 }\n").unwrap();
    assert_success(&driver().arg(&library).args(["--crate-name=fixture", "--crate-type=lib"]).arg(format!("-Cextra-filename={original_extra}")).output().unwrap());
    let original_bytes = fs::read(&original).unwrap();

    let dependent = fixture.source.join("dependent.rs");
    fs::write(&dependent, "pub fn indirect() -> u8 { fixture::answer() } pub fn shared(value: fixture::Shared) -> fixture::Shared { value }\n").unwrap();
    let dependency_path = match spelling {
        ExternSpelling::SplitRelative | ExternSpelling::JoinedRelative => PathBuf::from("./spelling/..").join(&original_name),
        ExternSpelling::Split | ExternSpelling::Joined => original.clone(),
    };
    let extern_value = format!("fixture={}", dependency_path.display());
    let extern_args = match spelling {
        ExternSpelling::Joined | ExternSpelling::JoinedRelative => vec![format!("--extern={extern_value}")],
        ExternSpelling::Split | ExternSpelling::SplitRelative => vec!["--extern".to_owned(), extern_value],
    };
    let mut command = driver();
    command.arg(&dependent).args(["--crate-name=dependent", "--crate-type=lib"]).args(&extern_args)
        .arg("-L").arg(format!("dependency={}", output_dir.display()));
    assert_success(&command.output().unwrap());
    let dependent_record: serde_json::Value = serde_json::from_slice(&fs::read(output_dir.join(".mutest-invocations/libdependent.rmeta.json")).unwrap()).unwrap();
    assert!(dependent_record["invocation"]["args"].as_array().unwrap().iter().any(|arg| arg.as_str() == extern_args.last().map(String::as_str)));

    let integration = fixture.source.join("integration.rs");
    fs::write(&integration, "#[test] fn direct_and_indirect() { assert_eq!(fixture::answer(), 42); assert_eq!(dependent::indirect(), 42); let value: fixture::Shared = dependent::shared(fixture::Shared(7)); assert_eq!(value.0, 7); }\n").unwrap();
    let output = driver().arg(&integration).args(["--test", "--crate-name=integration"])
        .arg("--extern").arg(format!("fixture={}", original.display()))
        .arg("--extern").arg(format!("dependent={}", output_dir.join("libdependent.rlib").display()))
        .arg("-L").arg(format!("dependency={}", output_dir.display()))
        .env("MUTEST_ENCODED_ARGS", "--crate-kind=integration-tests\u{1f}--emit=test-bin\u{1f}--mutation-operators=fn_return_default")
        .env("MUTEST_TARGET_DIR_ROOT", fixture.output.join("runtime"))
        .env("CARGO_PKG_NAME", "fixture").env("CARGO_CRATE_NAME", "integration")
        .output().unwrap();
    assert_success(&output);
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert!(output_dir.join(format!("libfixture{original_extra}-for-integration.rlib")).is_file());
    assert!(output_dir.join("libdependent-for-integration.rlib").is_file());

    let output = Command::new(output_dir.join("integration")).env_clear().env("TMPDIR", &fixture.output).output().unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[fn_return_default] return `0` without evaluating the function body"), "{stdout}");
    assert!(stdout.contains("[fn_return_default] return `1` without evaluating the function body"), "{stdout}");
    assert!(!stdout.contains("mutation was not detected"), "{stdout}");
}

#[test]
fn manifest_and_cli_operator_selections_produce_identical_mutation_ids() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("Cargo.toml"), "[package]\nname=\"fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[workspace]\n[lib]\npath=\"lib.rs\"\n[package.metadata.mutest.mutation-operators]\nfn-return-default=true\nlogical-op-and-or-swap=true\nmatch-guard-value=true\n").unwrap();
    fs::write(fixture.source.join("lib.rs"), "pub fn answer(value: bool) -> bool { match value { true if value && value => false, _ => true } }\n#[test] fn check() { assert!(!answer(true)); assert!(answer(false)); }\n").unwrap();
    let run = |selection: &str| {
        let mut command = fixture.driver(&fixture.source);
        command.args(["lib.rs", "--crate-name=fixture", "--crate-type=lib", "--test", "--edition=2024", "--out-dir"]).arg(&fixture.output)
            .env("CARGO_MANIFEST_PATH", fs::canonicalize(fixture.source.join("Cargo.toml")).unwrap())
            .env("CARGO_MANIFEST_DIR", fs::canonicalize(&fixture.source).unwrap())
            .env("CARGO_PKG_NAME", "fixture")
            .env("CARGO_CRATE_NAME", "fixture")
            .env("CARGO_PRIMARY_PACKAGE", "1")
            .env("MUTEST_ENCODED_ARGS", format!("--emit=info\u{1f}--print=mutations{selection}"));
        for name in ["HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "CARGO_HOME"] {
            if let Some(value) = env::var_os(name) {
                command.env(name, value);
            }
        }
        let output = command.output().unwrap();
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };

    let manifest = run("");
    let cli = run("\u{1f}--mutation-operators=fn_return_default,logical_op_and_or_swap,match_guard_value");

    for operator in ["fn_return_default", "logical_op_and_or_swap", "match_guard_value"] {
        assert!(manifest.contains(operator), "missing {operator}: {manifest}");
    }
    assert_eq!(manifest, cli);
}
