//! Records how each crate was compiled, beside its artifacts, so that a mutated dependency and the
//! crates built on it can be compiled again by a child driver.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rustc_middle::ty::TyCtxt;
use rustc_span::def_id::CrateNum;
use serde::{Deserialize, Serialize};

use super::RustcInvocation;

const VERSION: u32 = 2;

/// Variables a compilation may need besides the ones the crate reads through `env!`.
const CONTEXT: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "OUT_DIR",
    "CARGO_MANIFEST_DIR", "CARGO_MANIFEST_PATH", "CARGO_PKG_NAME", "CARGO_CRATE_NAME",
    "CARGO_PKG_VERSION", "CARGO_PKG_VERSION_MAJOR", "CARGO_PKG_VERSION_MINOR", "CARGO_PKG_VERSION_PATCH", "CARGO_PKG_VERSION_PRE",
    "CARGO_PRIMARY_PACKAGE", "RUSTUP_TOOLCHAIN", "RUSTUP_HOME", "CARGO_HOME",
    "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH", "SYSTEMROOT", "LIB", "INCLUDE",
];

/// The size and modification time of the driver binary, which tell a record written by another build of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DriverStamp {
    len: u64,
    modified: SystemTime,
}

impl DriverStamp {
    fn current() -> Result<Self, String> {
        let metadata = env::current_exe().and_then(fs::metadata).map_err(|error| format!("cannot inspect the mutest-driver binary: {error}"))?;
        let modified = metadata.modified().map_err(|error| format!("cannot inspect the mutest-driver binary: {error}"))?;
        Ok(Self { len: metadata.len(), modified })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    version: u32,
    compiler: String,
    driver: DriverStamp,
    /// The artifact this record sits beside.
    pub artifact: PathBuf,
    /// Every artifact of the same compilation, including `artifact`.
    pub paired_outputs: Vec<PathBuf>,
    pub invocation: RustcInvocation,
}

fn compiler() -> &'static str {
    rustc_interface::util::rustc_version_str().unwrap_or("unknown rustc")
}

fn env_value(name: &str) -> Result<Option<String>, String> {
    env::var_os(name)
        .map(|value| value.into_string().map_err(|_| format!("environment variable `{name}` is not UTF-8")))
        .transpose()
}

fn environment(used: impl IntoIterator<Item = (String, Option<String>)>, get: impl Fn(&str) -> Result<Option<String>, String>) -> Result<Vec<(String, Option<String>)>, String> {
    let mut values = BTreeMap::new();
    for &name in CONTEXT {
        values.insert(name.to_owned(), get(name)?);
    }
    // NOTE: These only make Cargo track mutest's arguments and driver; a replay has its own.
    values.extend(used.into_iter().filter(|(name, _)| name != "MUTEST_FINGERPRINT" && name != "MUTEST_DRIVER_STAMP"));
    Ok(values.into_iter().collect())
}

/// The invocation that is compiling the crate in `tcx`, with the environment it reads.
pub fn capture(tcx: TyCtxt<'_>, args: &[String]) -> Result<Record, String> {
    let used = tcx.sess.env_depinfo.lock().iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.map(|value| value.as_str().to_owned())))
        .collect::<Vec<_>>();

    Ok(Record {
        version: VERSION,
        compiler: compiler().to_owned(),
        driver: DriverStamp::current()?,
        artifact: PathBuf::new(),
        paired_outputs: vec![],
        invocation: RustcInvocation {
            args: args.to_vec(),
            env_vars: environment(used, env_value)?,
            working_directory: env::current_dir().map_err(|error| format!("cannot read the working directory: {error}"))?,
        },
    })
}

/// `path` with symlinks resolved, as rustc's crate locator names the crates it loads.
fn canonical(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("cannot resolve `{}`: {error}", path.display()))
}

fn sidecar(artifact: &Path) -> PathBuf {
    let mut name = artifact.file_name().unwrap_or_default().to_owned();
    name.push(".json");
    artifact.parent().unwrap_or(Path::new(".")).join(".mutest-invocations").join(name)
}

/// Writes `record` beside each of `artifacts`, the outputs of one compilation.
pub fn publish(mut record: Record, artifacts: &[PathBuf]) -> Result<(), String> {
    record.paired_outputs = artifacts.iter().map(|artifact| canonical(artifact)).collect::<Result<_, _>>()?;
    for artifact in record.paired_outputs.clone() {
        let path = sidecar(&artifact);
        record.artifact = artifact;
        let written = fs::create_dir_all(path.parent().unwrap())
            .and_then(|()| fs::write(&path, serde_json::to_vec(&record)?));
        written.map_err(|error| format!("cannot write `{}`: {error}", path.display()))?;
    }
    Ok(())
}

/// The record beside `artifact`, provided this driver and compiler wrote it.
pub fn load(artifact: &Path) -> Result<Record, String> {
    let artifact = canonical(artifact)?;
    let path = sidecar(&artifact);
    let bytes = fs::read(&path).map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let record = serde_json::from_slice::<Record>(&bytes).ok()
        .filter(|record| record.version == VERSION && record.compiler == compiler() && record.artifact == artifact);
    match record {
        Some(record) if record.driver == DriverStamp::current()? => Ok(record),
        _ => Err(format!("`{}` was written by another build of mutest-driver or rustc; rebuild `{}`", path.display(), artifact.display())),
    }
}

/// The dependencies named in a crate's metadata, by crate hash.
fn metadata_dependencies(text: &str) -> Result<Vec<String>, String> {
    let Some((_, dependencies)) = text.split_once("=External Dependencies=\n") else {
        return Err("compiler metadata has no dependency table".to_owned());
    };
    dependencies.lines()
        .take_while(|line| !line.is_empty())
        .map(|line| {
            // NOTE: Rows read `<index> <name> hash <hash> ...`.
            match line.split_whitespace().collect::<Vec<_>>()[..] {
                [index, _, "hash", hash, ..] if index.parse::<usize>().is_ok() => Ok(hash.to_owned()),
                _ => Err(format!("unrecognized compiler dependency metadata row `{line}`")),
            }
        })
        .collect()
}

/// The records of every crate that depends on `mutant`, in dependency order.
pub fn dependents(tcx: TyCtxt<'_>, mutant: CrateNum) -> Result<Vec<Record>, String> {
    let mut dependencies = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    for &cnum in tcx.crates(()) {
        let source = tcx.used_crate_source(cnum);
        let Some(path) = source.rmeta.as_ref().or(source.rlib.as_ref()).or(source.dylib.as_ref()) else {
            return Err(format!("crate `{}` has no artifact", tcx.crate_name(cnum)));
        };
        let mut metadata = Vec::new();
        rustc_metadata::locator::list_file_metadata(
            &tcx.sess.target,
            path,
            &rustc_codegen_ssa::back::metadata::DefaultMetadataLoader,
            &mut metadata,
            &["root".to_owned()],
            compiler(),
        ).map_err(|error| format!("cannot read the metadata of `{}`: {error}", path.display()))?;
        let text = String::from_utf8(metadata).map_err(|_| format!("the metadata of `{}` is not UTF-8", path.display()))?;

        let hash = tcx.crate_hash(cnum).to_string();
        dependencies.insert(hash.clone(), metadata_dependencies(&text)?);
        artifacts.insert(hash, path);
    }

    let mut affected = BTreeSet::from([tcx.crate_hash(mutant).to_string()]);
    loop {
        let next = dependencies.iter()
            .filter(|(hash, dependencies)| !affected.contains(*hash) && dependencies.iter().any(|dependency| affected.contains(dependency)))
            .map(|(hash, _)| hash.clone())
            .collect::<Vec<_>>();
        if next.is_empty() { break; }
        affected.extend(next);
    }

    tcx.postorder_cnums(()).iter()
        .filter(|&&cnum| cnum != mutant && affected.contains(&tcx.crate_hash(cnum).to_string()))
        .map(|&cnum| {
            let path = artifacts[&tcx.crate_hash(cnum).to_string()];
            load(path).map_err(|error| format!("affected dependency {} needs valid replay metadata: {error}", tcx.crate_name(cnum)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-scratch").join(format!("invocation-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(fs::canonicalize(&dir).unwrap())
        }

        fn artifact(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, name).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record(dir: &Path) -> Record {
        Record {
            version: VERSION,
            compiler: compiler().to_owned(),
            driver: DriverStamp::current().unwrap(),
            artifact: PathBuf::new(),
            paired_outputs: vec![],
            invocation: RustcInvocation { args: vec!["rustc".to_owned(), "lib.rs".to_owned()], env_vars: vec![], working_directory: dir.to_owned() },
        }
    }

    #[test]
    fn only_the_context_and_the_variables_the_crate_read_are_recorded() {
        let ambient = BTreeMap::from([("PATH", "tool-path"), ("REQUIRED", "ambient"), ("UNRELATED", "unrelated")]);
        let used = [
            ("REQUIRED".to_owned(), Some("read".to_owned())),
            ("ABSENT".to_owned(), None),
            ("MUTEST_FINGERPRINT".to_owned(), Some("fingerprint".to_owned())),
            ("MUTEST_DRIVER_STAMP".to_owned(), Some("stamp".to_owned())),
        ];

        let recorded = environment(used, |name| Ok(ambient.get(name).map(|value| (*value).to_owned()))).unwrap()
            .into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(recorded.get("PATH"), Some(&Some("tool-path".to_owned())));
        assert_eq!(recorded.get("REQUIRED"), Some(&Some("read".to_owned())));
        assert_eq!(recorded.get("ABSENT"), Some(&None));
        assert_eq!(recorded.get("UNRELATED"), None);
        assert_eq!(recorded.get("MUTEST_FINGERPRINT"), None);
        assert_eq!(recorded.get("MUTEST_DRIVER_STAMP"), None);
    }

    #[test]
    fn a_record_is_read_back_beside_each_artifact_of_its_compilation() {
        let scratch = Scratch::new("round-trip");
        let rmeta = scratch.artifact("libfixture.rmeta");
        let rlib = scratch.artifact("libfixture.rlib");

        publish(record(&scratch.0), &[rmeta.clone(), rlib.clone()]).unwrap();

        for artifact in [&rmeta, &rlib] {
            let loaded = load(artifact).unwrap();
            assert_eq!(&loaded.artifact, artifact);
            assert_eq!(loaded.paired_outputs, [rmeta.clone(), rlib.clone()]);
            assert_eq!(loaded.invocation.args, ["rustc", "lib.rs"]);
            assert_eq!(loaded.invocation.working_directory, scratch.0);
        }
    }

    #[test]
    fn a_record_from_another_compiler_or_driver_or_for_another_artifact_is_refused() {
        let scratch = Scratch::new("refused");
        let artifact = scratch.artifact("libfixture.rmeta");
        assert!(load(&artifact).is_err());

        let mut stale = record(&scratch.0);
        stale.compiler = "rustc 0.0.0".to_owned();
        publish(stale, std::slice::from_ref(&artifact)).unwrap();
        assert!(load(&artifact).unwrap_err().contains("rebuild"));

        let mut stale = record(&scratch.0);
        stale.driver.len += 1;
        publish(stale, std::slice::from_ref(&artifact)).unwrap();
        assert!(load(&artifact).unwrap_err().contains("rebuild"));

        let moved = scratch.artifact("libmoved.rmeta");
        publish(record(&scratch.0), std::slice::from_ref(&artifact)).unwrap();
        fs::create_dir_all(sidecar(&moved).parent().unwrap()).unwrap();
        fs::copy(sidecar(&artifact), sidecar(&moved)).unwrap();
        assert!(load(&moved).unwrap_err().contains("rebuild"));
        assert!(load(&artifact).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_record_is_found_through_a_symlinked_directory() {
        let scratch = Scratch::new("symlink");
        fs::create_dir(scratch.0.join("real")).unwrap();
        let artifact = scratch.artifact("real/libfixture.rmeta");
        std::os::unix::fs::symlink(scratch.0.join("real"), scratch.0.join("link")).unwrap();

        publish(record(&scratch.0), &[scratch.0.join("link/./libfixture.rmeta")]).unwrap();

        assert_eq!(load(&artifact).unwrap().artifact, artifact);
    }
}
