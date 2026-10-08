#![feature(rustc_private)]

extern crate rustc_session;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use cargo_metadata::{Message, MetadataCommand};
use mutest_driver::config::{
    Config, CrateKind, Options, PrintOptions, UnstableFlags, WriteOptions,
};
use mutest_driver::inject::inject_runtime_crate_and_deps;
use mutest_driver::passes::{copy_compiler_settings, parse_compiler_args};
use mutest_emit::codegen::mutation::UnsafeTargeting;
use rustc_session::config::ExternLocation;

struct Fixture {
    source: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let name = format!("embedded-linkage-{}", std::process::id());
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-scratch")
            .join(&name);
        let output = env::temp_dir().join(&name);
        assert!(
            !source.exists() && !output.exists(),
            "stale embedded linkage fixture; remove it before retrying"
        );
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(output.join("debug/deps")).unwrap();
        Self { source, output }
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear().current_dir(&self.source);
        for name in [
            "PATH",
            "HOME",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
            "CARGO_HOME",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "DYLD_FALLBACK_LIBRARY_PATH",
            "TMPDIR",
            "TMP",
            "TEMP",
            "SystemRoot",
            "SystemDrive",
            "LIB",
        ] {
            if let Some(value) = env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    fn rustc(&self) -> Command {
        let mut command = self.command(env!("CARGO_BIN_EXE_mutest-driver"));
        command.args([
            "--rustc",
            "--edition=2024",
            "-Cmetadata=mutest-runtime-private-v1",
        ]);
        command
    }

    fn build_phf(&self, warm: bool) -> PathBuf {
        let flags = if warm {
            "-Zembed-metadata=no -Cmetadata=mutest-runtime-private-v1 -Cmetadata=embedded-linkage-b"
        } else {
            "-Zembed-metadata=no -Cmetadata=mutest-runtime-private-v1"
        };
        let output = self
            .command(env!("CARGO"))
            .args([
                "build",
                "--offline",
                "--message-format=json",
                "--jobs=1",
                "--manifest-path",
            ])
            .arg(self.source.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(self.output.join("cargo"))
            .env("RUSTFLAGS", flags)
            .output()
            .unwrap();
        assert_success(&output);
        let mut phf = None;
        for message in Message::parse_stream(output.stdout.as_slice()) {
            let Message::CompilerArtifact(artifact) = message.unwrap() else {
                continue;
            };
            for filename in &artifact.filenames {
                let path = filename.as_std_path();
                if !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("rlib" | "rmeta" | "so" | "dylib" | "dll")
                ) {
                    continue;
                }
                let destination = self.deps().join(path.file_name().unwrap());
                fs::copy(path, &destination).unwrap();
                if artifact.target.name == "phf"
                    && path.extension().is_some_and(|ext| ext == "rlib")
                {
                    assert!(
                        phf.replace(destination).is_none(),
                        "ambiguous phf build artifacts"
                    );
                }
                if path.extension().is_some_and(|ext| ext == "rlib")
                    && path.with_extension("rmeta").is_file()
                {
                    fs::copy(
                        path.with_extension("rmeta"),
                        self.deps()
                            .join(path.file_name().unwrap())
                            .with_extension("rmeta"),
                    )
                    .unwrap();
                }
            }
        }
        phf.expect("Cargo did not report the phf rlib")
    }

    fn deps(&self) -> PathBuf {
        self.output.join("debug/deps")
    }

    fn stub(&self) -> PathBuf {
        self.output
            .join("debug/libmutest_runtime_embedded_target_stub.rlib")
    }

    fn build_edition_2021(&self) -> PathBuf {
        let rlib = self.deps().join("libmutest_edition_2021.rlib");
        let output = self
            .command(env!("CARGO_BIN_EXE_mutest-driver"))
            .args(["--rustc", "--edition=2021"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../mutest-edition-2021/src/lib.rs"))
            .args(["--crate-name", "mutest_edition_2021", "--crate-type=rlib", "-o"])
            .arg(&rlib)
            .output()
            .unwrap();
        assert_success(&output);
        rlib
    }

    fn injected_externs(&self) -> Vec<(String, PathBuf)> {
        let args = vec![
            "rustc".to_owned(),
            self.source.join("consumer.rs").display().to_string(),
            "--edition=2024".to_owned(),
            "--crate-type=lib".to_owned(),
        ];
        let (compiler_config, _) = parse_compiler_args(&args);
        let config = Config {
            compiler_config: compiler_config.unwrap(),
            invocation_fingerprint: None,
            mutest_target_dir_root: Some(self.output.clone()),
            mutest_search_path: Some(self.output.join("debug")),
            opts: Options {
                crate_kind: CrateKind::MutantWithInternalTests,
                cargo_target_kind: None,
                outputs: Default::default(),
                verbosity: 0,
                report_timings: false,
                print_opts: PrintOptions {
                    print_headers: false,
                    tests: None,
                    call_graph: None,
                    mutation_targets: None,
                    unreached_fns: None,
                    mutations: None,
                    conflict_graph: None,
                    code: None,
                },
                write_opts: WriteOptions {
                    out_dir: self.output.clone(),
                },
                unsafe_targeting: UnsafeTargeting::None,
                operators: &[],
                call_graph_depth_limit: None,
                call_graph_trace_length_limit: None,
                mutation_depth: 0,
                mutation_filters: vec![],
                mutation_parallelism: None,
                unstable_flags: UnstableFlags {
                    embedded: true,
                    ..Default::default()
                },
            },
        };
        let mut compiler_config = copy_compiler_settings(&config.compiler_config);
        inject_runtime_crate_and_deps(&config, &mut compiler_config, None);
        let externs = compiler_config
            .opts
            .externs
            .iter()
            .flat_map(|(name, entry)| {
                let ExternLocation::ExactPaths(paths) = &entry.location else {
                    panic!("unexpected injected extern {name}");
                };
                paths
                    .iter()
                    .map(|path| (name.clone(), path.canonicalized().to_owned()))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            externs,
            [(
                "mutest_runtime".to_owned(),
                fs::canonicalize(self.stub()).unwrap()
            )]
        );
        externs
    }

    fn consumer(&self, extra_extern: Option<&Path>) -> Output {
        let mut command = self.rustc();
        command
            .arg(self.source.join("consumer.rs"))
            .args(["--crate-name", "embedded_linkage_consumer", "--extern"])
            .arg(format!("mutest_runtime={}", self.stub().display()))
            .arg("-L")
            .arg(format!("dependency={}", self.deps().display()))
            .arg("--out-dir")
            .arg(self.output.join("consumer"));
        if let Some(path) = extra_extern {
            command.arg("--extern").arg(format!(
                "__mutest_runtime_public_dep_phf={}",
                path.display()
            ));
        }
        command.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.source);
        let _ = fs::remove_dir_all(&self.output);
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "compiler failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_phf_refused(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "compiler accepted a missing or mismatched phf artifact"
    );
    assert!(
        stderr.contains("phf"),
        "refusal did not identify the dependency: {stderr}"
    );
    assert!(
        stderr.contains("crate") || stderr.contains("metadata"),
        "unexpected refusal: {stderr}"
    );
}

#[test]
fn embedded_macro_uses_the_stub_linked_phf_in_clean_and_warm_artifact_directories() {
    let fixture = Fixture::new();
    let mut metadata = MetadataCommand::new();
    metadata.manifest_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml"));
    metadata.other_options(vec!["--offline".to_owned(), "--locked".to_owned()]);
    let metadata = metadata.exec().unwrap();
    let stub_package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_ref() == "mutest-runtime-embedded-target-stub")
        .unwrap();
    let stub_node = metadata
        .resolve
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|node| node.id == stub_package.id)
        .unwrap();
    let phf_id = &stub_node
        .deps
        .iter()
        .find(|dep| dep.name == "__mutest_runtime_public_dep_phf")
        .unwrap()
        .pkg;
    let phf_package = metadata
        .packages
        .iter()
        .find(|package| &package.id == phf_id)
        .unwrap();
    let phf_source = phf_package.manifest_path.parent().unwrap();
    fs::write(fixture.source.join("Cargo.toml"), format!(
        "[package]\nname = \"embedded-linkage-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n[lib]\npath = \"lib.rs\"\n[dependencies]\nphf = {{ path = {}, default-features = false, features = [\"macros\"] }}\n",
        serde_json::to_string(phf_source.as_str()).unwrap(),
    )).unwrap();
    fs::write(
        fixture.source.join("lib.rs"),
        "#![no_std]\npub use phf::Map;\n",
    )
    .unwrap();
    fs::write(
        fixture.source.join("consumer.rs"),
        r#"
extern crate mutest_runtime as runtime;
mod phf { pub struct Map; }
const REACHABLE: runtime::EntryPoints = runtime::EntryPoints::InternalTests(runtime::static_map! {
    "case" => 7usize,
    #[cfg(any())]
    "absent" => 99usize,
});
fn main() {
    let runtime::EntryPoints::InternalTests(map) = REACHABLE else { unreachable!() };
    assert_eq!(map.get("case"), Some(&7));
    assert_eq!(map.get("absent"), None);
}
"#,
    )
    .unwrap();
    fs::create_dir_all(fixture.output.join("consumer")).unwrap();

    let edition_2021 = fixture.build_edition_2021();
    let phf_a = fixture.build_phf(false);
    let rmeta_a = phf_a.with_extension("rmeta");
    assert!(
        rmeta_a.is_file(),
        "fixture needs full rmeta beside metadata-stub rlib"
    );
    let output = fixture
        .rustc()
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../mutest-runtime-embedded-target-stub/src/lib.rs"),
        )
        .args([
            "--crate-name",
            "mutest_runtime_embedded_target_stub",
            "--crate-type=rlib",
            "-Zembed-metadata=yes",
            "--extern",
        ])
        .arg(format!(
            "__mutest_runtime_public_dep_phf={}",
            rmeta_a.display()
        ))
        .arg("--extern")
        .arg(format!("mutest_edition_2021={}", edition_2021.display()))
        .arg("-L")
        .arg(format!("dependency={}", fixture.deps().display()))
        .arg("-o")
        .arg(fixture.stub())
        .output()
        .unwrap();
    assert_success(&output);
    fixture.injected_externs();
    assert_success(&fixture.consumer(None));

    let phf_b = fixture.build_phf(true);
    assert_ne!(phf_a, phf_b, "fixture must build distinct phf identities");
    let rmeta_b = phf_b.with_extension("rmeta");
    let older = SystemTime::now() - Duration::from_secs(7200);
    let newer = SystemTime::now() - Duration::from_secs(3600);
    for path in [&phf_a, &rmeta_a] {
        fs::File::options().write(true).open(path).unwrap().set_modified(older).unwrap();
    }
    for path in [&phf_b, &rmeta_b] {
        fs::File::options().write(true).open(path).unwrap().set_modified(newer).unwrap();
    }
    assert!(
        fs::metadata(&phf_b).unwrap().modified().unwrap()
            > fs::metadata(&phf_a).unwrap().modified().unwrap()
    );
    assert_success(&fixture.consumer(None));
    assert_success(&fixture.consumer(Some(&rmeta_b)));

    let saved_rlib = fs::read(&phf_a).unwrap();
    let saved_rmeta = fs::read(&rmeta_a).unwrap();
    fs::remove_file(&phf_a).unwrap();
    fs::remove_file(&rmeta_a).unwrap();
    assert_phf_refused(&fixture.consumer(None));

    fs::write(&rmeta_a, &saved_rmeta).unwrap();
    assert_phf_refused(&fixture.consumer(None));
    fs::copy(&phf_b, &phf_a).unwrap();
    assert_phf_refused(&fixture.consumer(None));

    fs::write(&phf_a, &saved_rlib).unwrap();
    fs::copy(&rmeta_b, &rmeta_a).unwrap();
    assert_phf_refused(&fixture.consumer(None));
    fs::write(&rmeta_a, &saved_rmeta).unwrap();
    assert_success(&fixture.consumer(None));

    fs::remove_file(fixture.stub()).unwrap();
    let missing_stub = fixture.consumer(None);
    assert!(!missing_stub.status.success());
    assert!(String::from_utf8_lossy(&missing_stub.stderr).contains("mutest_runtime"));
}
