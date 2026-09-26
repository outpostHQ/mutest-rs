use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use rustc_interface::Config as CompilerConfig;
use rustc_session::EarlyDiagCtxt;
use rustc_session::config::{ExternEntry, ExternLocation, Externs, OutputType, build_target_config, host_tuple};
use rustc_session::search_paths::{PathKind, SearchPath};
use rustc_session::utils::CanonicalizedPath;

use crate::config::Config;
use crate::passes::external_mutant::specialized_crate::SpecializedMutantCrateCompilationResult;

mod rlib_catalog { include!(env!("RLIB_CATALOG")); }

#[cfg(feature = "embed-runtime")]
fn extract_file(path: &Path, content: &[u8]) {
    let existing_file_up_to_date = match fs::read(path) {
        Ok(file_content) => file_content == content,
        Err(_) => false,
    };

    if existing_file_up_to_date { return; }

    // Every driver of a build extracts into the same directory, while another may already be
    // compiling against the file: write it aside and rename it over, which replaces it whole.
    let mut temp_file_name = path.file_name().expect("extracted file has no name").to_owned();
    temp_file_name.push(format!(".{}.tmp", std::process::id()));
    let temp_path = path.with_file_name(temp_file_name);
    fs::write(&temp_path, content).unwrap_or_else(|error| panic!("cannot write file `{}`: {error}", temp_path.display()));
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        // Windows refuses to replace a file another process has open; that process extracted it.
        if fs::read(path).is_ok_and(|file_content| file_content == content) { return; }
        panic!("cannot write file `{}`: {error}", path.display());
    }
}

const MUTEST_EXTRACTED_DEPS_DIR_NAME: &str = "mutest_deps";

#[cfg(feature = "embed-runtime")]
fn extract_runtime_crate_and_deps(target_dir_root_path: &Path) {
    let mutest_deps_dir_path = target_dir_root_path.join(MUTEST_EXTRACTED_DEPS_DIR_NAME);
    fs::create_dir_all(&mutest_deps_dir_path).unwrap_or_else(|error| panic!("cannot create directory `{}`: {error}", mutest_deps_dir_path.display()));

    extract_file(&mutest_deps_dir_path.join(rlib_catalog::MUTEST_RUNTIME_RLIB_FILENAME), rlib_catalog::MUTEST_RUNTIME_RLIB_DATA);
    for (dep_file_name, dep_data) in rlib_catalog::MUTEST_RUNTIME_EXTERN_DEPS_DATA {
        extract_file(&mutest_deps_dir_path.join(dep_file_name), dep_data);
    }
}

const COMPILETIME_ARTIFACTS_DIR: &str = env!("COMPILETIME_ARTIFACTS_DIR");
const COMPILETIME_DEPS_DIR: &str = env!("COMPILETIME_DEPS_DIR");

/// Every dependency artifact, across `deps/` and each `build/<crate>/<hash>/out` Cargo now writes.
fn dependency_dir_entries(deps_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![deps_dir.to_owned()];
    if let Some(profile_dir) = deps_dir.parent()
        && let Ok(crates) = std::fs::read_dir(profile_dir.join("build"))
    {
        for krate in crates.filter_map(|entry| entry.ok()) {
            let Ok(hashes) = std::fs::read_dir(krate.path()) else { continue };
            dirs.extend(hashes.filter_map(|entry| entry.ok()).map(|hash| hash.path().join("out")));
        }
    }

    dirs.iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()).map(|entry| entry.path()))
        .collect()
}

/// Where `file_name` actually is: `deps/` if Cargo still collects artifacts there, otherwise the
/// `build/<crate>/<hash>/out` holding it. Falls back to the `deps/` path so the error names it.
fn locate_dependency(deps_dir: &Path, file_name: &str) -> PathBuf {
    let in_deps = deps_dir.join(file_name);
    if in_deps.exists() {
        return in_deps;
    }

    let Some(profile_dir) = deps_dir.parent() else { return in_deps };
    let Ok(crates) = std::fs::read_dir(profile_dir.join("build")) else { return in_deps };
    for krate in crates.filter_map(|entry| entry.ok()) {
        let Ok(hashes) = std::fs::read_dir(krate.path()) else { continue };
        for hash in hashes.filter_map(|entry| entry.ok()) {
            let candidate = hash.path().join("out").join(file_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    in_deps
}

/// `lib<NAME>`, of `lib<NAME>-<HASH>.<EXTENSION>` or `lib<NAME>.<EXTENSION>`.
fn crate_file_root(file_name: &str) -> &str {
    file_name.split_once('-').or_else(|| file_name.split_once('.')).map_or(file_name, |(root, _)| root)
}

/// The files of the newest build, among `candidates`, of the crate `dep_file_name` names: its
/// `.rlib` and `.rmeta`. A build directory keeps every build of a crate, each under a hash of its
/// own, after a change of toolchain or profile; rustc refuses an extern offered two of them (E0464),
/// and the newest is the one the embedded runtime stub was last built against.
fn newest_build_of(dep_file_name: &str, candidates: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let root = crate_file_root(dep_file_name);
    let mut builds = candidates.into_iter()
        .filter(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| crate_file_root(name) == root))
        .collect::<Vec<_>>();
    let newest = builds.iter()
        .max_by_key(|path| fs::metadata(path).and_then(|metadata| metadata.modified()).ok())
        .and_then(|path| path.file_stem())
        .map(ToOwned::to_owned);
    builds.retain(|path| path.file_stem() == newest.as_deref());
    builds
}

/// Add `deps_dir` and, beside it, every `build/<crate>/<hash>/out` Cargo now writes instead of
/// collecting artifacts into a single `deps/`.
fn push_dependency_search_paths(search_paths: &mut Vec<SearchPath>, deps_dir: &Path) {
    search_paths.push(SearchPath { kind: PathKind::Dependency, dir: deps_dir.to_owned().into() });

    let Some(profile_dir) = deps_dir.parent() else { return };
    let Ok(crates) = std::fs::read_dir(profile_dir.join("build")) else { return };
    for krate in crates.filter_map(|entry| entry.ok()) {
        let Ok(hashes) = std::fs::read_dir(krate.path()) else { continue };
        for hash in hashes.filter_map(|entry| entry.ok()) {
            let out = hash.path().join("out");
            if out.is_dir() {
                search_paths.push(SearchPath { kind: PathKind::Dependency, dir: out.into() });
            }
        }
    }
}

/// Cargo compiles libraries with `-Zembed-metadata=no`, leaving only a metadata stub in the rlib, so
/// whatever links that rlib needs the `.rmeta` holding the full metadata offered beside it.
fn specialized_mutant_crate_paths(metadata_file_path: &Path, exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut file_paths = vec![];
    if exists(metadata_file_path) { file_paths.push(metadata_file_path.to_owned()); }
    file_paths.push(metadata_file_path.with_extension("rlib"));
    file_paths
}

pub fn inject_runtime_crate_and_deps(config: &Config, compiler_config: &mut CompilerConfig, specialized_external_mutant_crate: Option<&(String, SpecializedMutantCrateCompilationResult)>) {
    // `mutest_runtime`'s `StaticBitMatrix` needs `generic_const_exprs`, which the next-generation
    // trait solver does not support. rustc reverts it for that crate; the generated harness that
    // names those types gets no such treatment, so revert it here too.
    compiler_config.opts.unstable_opts.next_solver.globally = false;

    let early_dcx = EarlyDiagCtxt::new(compiler_config.opts.error_format);

    let host_triple = host_tuple();
    let target_triple = compiler_config.opts.target_triple.tuple();

    // The runtime this binary embeds is built for the host, and is the only one it embeds: a
    // cross-compiled or embedded runtime is looked for in a mutest-rs build directory, as it is by a
    // driver built without embedding.
    let runtime_embedded = cfg!(feature = "embed-runtime") && target_triple == host_triple && !config.opts.unstable_flags.embedded;
    // Unpacked only by a build that links it: every other run of the driver would unpack it for
    // nothing, and into the target directory of a build that has no use for it.
    #[cfg(feature = "embed-runtime")]
    if runtime_embedded { extract_runtime_crate_and_deps(&config.target_dir_root()); }

    let mutest_host_artifacts_dir_path = if runtime_embedded {
        &config.target_dir_root().join(MUTEST_EXTRACTED_DEPS_DIR_NAME)
    } else {
        config.mutest_search_path.as_deref().unwrap_or(Path::new(COMPILETIME_ARTIFACTS_DIR))
    };

    let mutest_host_deps_dir_path = if runtime_embedded {
        mutest_host_artifacts_dir_path
    } else {
        match &config.mutest_search_path {
            Some(path) => &path.join("deps"),
            None => Path::new(COMPILETIME_DEPS_DIR),
        }
    };

    let mutest_target_artifacts_dir_path = match target_triple == host_triple {
        true => mutest_host_artifacts_dir_path,
        false => {
            let profile = mutest_host_artifacts_dir_path.file_name().expect("invalid mutest search path");
            let root_dir_path = mutest_host_artifacts_dir_path.parent().expect("invalid mutest search path");

            &root_dir_path.join(target_triple).join(profile)
        }
    };

    let mutest_target_deps_dir_path = match target_triple == host_triple {
        true => mutest_host_deps_dir_path,
        false => &mutest_target_artifacts_dir_path.join("deps"),
    };

    if target_triple != host_triple {
        push_dependency_search_paths(&mut compiler_config.opts.search_paths, mutest_target_deps_dir_path);
    }
    // NOTE: We need the host dependencies for procedural macro crate dependencies, as these run on the host, during compilation.
    push_dependency_search_paths(&mut compiler_config.opts.search_paths, mutest_host_deps_dir_path);

    // The externs (paths to dependencies) of the `mutest_runtime` crate are baked into it at compile time.
    // These must be propagated to any crate which depends on it.
    let mut externs = BTreeMap::<String, ExternEntry>::new();
    for (key, entry) in compiler_config.opts.externs.iter() {
        externs.insert(key.clone(), entry.clone());
    }

    if !config.opts.unstable_flags.embedded {
        externs.insert("mutest_runtime".to_owned(), ExternEntry {
            location: ExternLocation::ExactPaths(BTreeSet::from([
                CanonicalizedPath::new(mutest_target_artifacts_dir_path.join(rlib_catalog::MUTEST_RUNTIME_RLIB_FILENAME)),
            ])),
            is_private_dep: false,
            add_prelude: false,
            nounused_dep: false,
            force: false,
        });
    } else {
        let rlib_path = mutest_target_artifacts_dir_path.join("libmutest_runtime_embedded_target_stub.rlib");
        match fs::exists(&rlib_path) {
            Ok(true) => {}
            // NOTE: We let the compiler emit its usual error message upon being unable to read an rlib file path.
            Err(_) => {}

            Ok(false) => {
                let profile = mutest_host_artifacts_dir_path.file_name().expect("invalid mutest search path");

                let mut diag = match target_triple == host_triple {
                    true => early_dcx.early_struct_fatal(format!("cannot find mutest-rs embedded runtime for host")),
                    false => early_dcx.early_struct_fatal(format!("cannot find cross-compiled mutest-rs embedded runtime for target `{target_triple}`")),
                };
                diag.note(format!("searching in `{}`", mutest_target_artifacts_dir_path.display()));
                match target_triple == host_triple {
                    true => {
                        // NOTE: We remove the explicit `--target` argument because if specified, Cargo will place the host artifact in a target triple subdirectory.
                        diag.note(format!("consider running `cargo build --profile={} -p mutest-runtime-embedded-target-stub` in the mutest-rs source tree", profile.display()));
                    }
                    false => {
                        diag.note(format!("consider running `cargo build --target={} --profile={} -p mutest-runtime-embedded-target-stub` in the mutest-rs source tree", target_triple, profile.display()));
                    }
                }
                diag.emit();
            }
        }
        externs.insert("mutest_runtime".to_owned(), ExternEntry {
            location: ExternLocation::ExactPaths(BTreeSet::from([
                CanonicalizedPath::new(rlib_path),
            ])),
            is_private_dep: false,
            add_prelude: false,
            nounused_dep: false,
            force: false,
        });
    }

    for &(visible_crate_name, dep_file_name) in rlib_catalog::MUTEST_RUNTIME_PUBLIC_DEPS {
        let mut dep_file_paths = BTreeSet::new();
        // FIXME: Use the actual public dependency list of the injected embedded runtime crate,
        //        rather than piggy-backing off the main mutest-runtime crate.
        if !config.opts.unstable_flags.embedded {
            dep_file_paths.insert(CanonicalizedPath::new(locate_dependency(mutest_target_deps_dir_path, dep_file_name)));
        } else {
            newest_build_of(dep_file_name, dependency_dir_entries(mutest_target_deps_dir_path)).into_iter()
                .map(CanonicalizedPath::new)
                .collect_into(&mut dep_file_paths);
        }
        let existing_extern = externs.insert(visible_crate_name.to_owned(), ExternEntry {
            location: ExternLocation::ExactPaths(dep_file_paths),
            is_private_dep: false,
            add_prelude: false,
            nounused_dep: false,
            force: false,
        });
        if let Some(_existing_extern) = existing_extern {
            let mut diag = early_dcx.early_struct_fatal(format!("mutest-injected crate conflicts with existing extern `{visible_crate_name}`"));
            diag.note("mutest-injected crates use the reserved `__mutest_runtime_public_dep_` prefix: if you see this error for any other crate, please file a bug report");
            diag.emit();
        }
    }

    if let Some((visible_crate_name, specialized_external_mutant_crate_compilation)) = specialized_external_mutant_crate
        && let Some(specialized_external_mutant_crate_outputs) = &specialized_external_mutant_crate_compilation.outputs
    {
        let metadata_file_path = specialized_external_mutant_crate_outputs.path(OutputType::Metadata).as_path().to_owned();

        let Some(extern_entry) = externs.get_mut(visible_crate_name) else {
            early_dcx.early_fatal(format!("cannot find extern `{visible_crate_name}` to replace with specialized external mutant crate"));
        };

        extern_entry.location = ExternLocation::ExactPaths(
            specialized_mutant_crate_paths(&metadata_file_path, |path| path.exists()).into_iter()
                .map(CanonicalizedPath::new)
                .collect::<BTreeSet<_>>()
        );
    }

    compiler_config.opts.externs = Externs::new(externs);
}

pub fn inject_test_crate_shim_if_no_target_std(config: &Config, compiler_config: &mut CompilerConfig) {
    let early_dcx = EarlyDiagCtxt::new(compiler_config.opts.error_format);

    let target = build_target_config(&early_dcx, &compiler_config.opts.target_triple, compiler_config.opts.sysroot.path(), compiler_config.opts.unstable_opts.unstable_options);
    if target.metadata.std == Some(true) { return; }

    let target_triple = compiler_config.opts.target_triple.tuple();

    if !config.opts.unstable_flags.embedded {
        let mut diag = early_dcx.early_struct_fatal(format!("target `{}` does not support std, but the embedded mutation runtime was not specified", target_triple));
        diag.note("the default mutation runtime does not support targets without std support");
        diag.note("consider running with the `-Z embedded` flag to use the experimental embedded mutation runtime");
        diag.emit();
    }

    // The shim is never embedded: it is looked for in a mutest-rs build directory.
    let mutest_host_artifacts_dir_path = config.mutest_search_path.as_deref().unwrap_or(Path::new(COMPILETIME_ARTIFACTS_DIR));

    let mutest_target_artifacts_dir_path = {
        let profile = mutest_host_artifacts_dir_path.file_name().expect("invalid mutest search path");
        let root_dir_path = mutest_host_artifacts_dir_path.parent().expect("invalid mutest search path");

        &root_dir_path.join(target_triple).join(profile)
    };

    let mut externs = BTreeMap::<String, ExternEntry>::new();
    for (key, entry) in compiler_config.opts.externs.iter() {
        externs.insert(key.clone(), entry.clone());
    }

    let rlib_path = mutest_target_artifacts_dir_path.join("libtest_metadata_shim.rlib");
    match fs::exists(&rlib_path) {
        Ok(true) => {}
        // NOTE: We let the compiler emit its usual error message upon being unable to read an rlib file path.
        Err(_) => {}

        Ok(false) => {
            let profile = mutest_host_artifacts_dir_path.file_name().expect("invalid mutest search path");

            let mut diag = early_dcx.early_struct_fatal(format!("cannot find cross-compiled libtest metadata shim for target `{target_triple}`"));
            diag.note(format!("searching in `{}`", mutest_target_artifacts_dir_path.display()));
            diag.note(format!("consider running `cargo build --target={} --profile={} -p test-metadata-shim` in the mutest-rs source tree", target_triple, profile.display()));
            diag.emit();
        }
    }
    externs.insert("test".to_owned(), ExternEntry {
        location: ExternLocation::ExactPaths(BTreeSet::from([
            CanonicalizedPath::new(rlib_path),
        ])),
        is_private_dep: false,
        add_prelude: false,
        nounused_dep: false,
        force: false,
    });

    compiler_config.opts.externs = Externs::new(externs);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{newest_build_of, specialized_mutant_crate_paths};

    #[test]
    fn of_two_builds_of_a_dependency_only_the_newer_is_offered() {
        use std::fs::{self, File};
        use std::time::{Duration, SystemTime};

        let dir_path = std::env::temp_dir().join(format!("mutest-newest-build-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir_path);
        fs::create_dir_all(&dir_path).unwrap();
        let file = |file_name: &str, age_in_hours: u64| {
            let path = dir_path.join(file_name);
            File::create(&path).unwrap().set_modified(SystemTime::now() - Duration::from_secs(age_in_hours * 60 * 60)).unwrap();
            path
        };
        let candidates = vec![
            file("libphf-5ebb443ee2c3b5f2.rlib", 2),
            file("libphf-5ebb443ee2c3b5f2.rmeta", 2),
            file("libphf-1161bb9eb0c8a456.rlib", 1),
            file("libphf-1161bb9eb0c8a456.rmeta", 1),
            file("libphf_shared-0281b48223ea3e2e.rlib", 0),
            file("phf-1161bb9eb0c8a456.d", 0),
        ];

        let mut offered = newest_build_of("libphf-43a9f6a5e2bd7c01.rlib", candidates);
        offered.sort();

        assert_eq!(offered, [dir_path.join("libphf-1161bb9eb0c8a456.rlib"), dir_path.join("libphf-1161bb9eb0c8a456.rmeta")]);
        fs::remove_dir_all(&dir_path).unwrap();
    }

    #[test]
    fn a_dependency_built_without_a_hash_in_its_name_is_found_by_its_name() {
        let candidates = vec![PathBuf::from("deps/libphf.rlib"), PathBuf::from("deps/libphf_shared.rlib")];

        assert_eq!(newest_build_of("libphf-43a9f6a5e2bd7c01.rlib", candidates), [PathBuf::from("deps/libphf.rlib")]);
    }

    #[cfg(feature = "embed-runtime")]
    #[test]
    fn an_extracted_file_replaces_a_stale_one_and_leaves_nothing_beside_it() {
        use std::fs;

        let dir_path = std::env::temp_dir().join(format!("mutest-extract-{}", std::process::id()));
        fs::create_dir_all(&dir_path).unwrap();
        let file_path = dir_path.join("libmutest_runtime.rlib");
        fs::write(&file_path, b"stale").unwrap();

        super::extract_file(&file_path, b"current");
        super::extract_file(&file_path, b"current");

        assert_eq!(fs::read(&file_path).unwrap(), b"current");
        assert_eq!(fs::read_dir(&dir_path).unwrap().count(), 1);
        fs::remove_dir_all(&dir_path).unwrap();
    }

    #[test]
    fn a_crate_linking_the_specialized_mutant_is_offered_its_full_metadata_beside_its_rlib() {
        let metadata_file_path = Path::new("target/mutest/out/libkrate-1a2b-for-integration-3c4d.rmeta");

        let file_paths = specialized_mutant_crate_paths(metadata_file_path, |_| true);

        assert_eq!(file_paths, vec![
            PathBuf::from("target/mutest/out/libkrate-1a2b-for-integration-3c4d.rmeta"),
            PathBuf::from("target/mutest/out/libkrate-1a2b-for-integration-3c4d.rlib"),
        ]);
    }

    #[test]
    fn a_specialized_mutant_that_wrote_no_metadata_file_is_offered_as_its_rlib_alone() {
        let metadata_file_path = Path::new("target/mutest/out/libkrate-1a2b-for-integration-3c4d.rmeta");

        let file_paths = specialized_mutant_crate_paths(metadata_file_path, |_| false);

        assert_eq!(file_paths, vec![PathBuf::from("target/mutest/out/libkrate-1a2b-for-integration-3c4d.rlib")]);
    }
}
