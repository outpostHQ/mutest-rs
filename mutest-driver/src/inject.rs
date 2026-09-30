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

#[path = "../../build_support/dependency_dirs.rs"]
mod dependency_dirs;
use dependency_dirs::dependency_dirs;

#[cfg(feature = "embed-runtime")]
fn extract_file(path: &Path, content: &[u8]) {
    let existing_file_up_to_date = match fs::read(path) {
        Ok(file_content) => file_content == content,
        Err(_) => false,
    };

    if existing_file_up_to_date { return; }

    // NOTE: The file is written aside and renamed into place, so that concurrent drivers never link a partial one.
    let mut temp_file_name = path.file_name().expect("extracted file has no name").to_owned();
    temp_file_name.push(format!(".{}.tmp", std::process::id()));
    let temp_path = path.with_file_name(temp_file_name);
    fs::write(&temp_path, content).unwrap_or_else(|error| panic!("cannot write file `{}`: {error}", temp_path.display()));
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        // NOTE: Windows refuses to replace a file another process has open, which is fine if it is already up to date.
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

/// The path of a runtime dependency, which is reported as missing from `deps_dir` if no directory holds it.
fn locate_dependency(deps_dir: &Path, file_name: &str) -> PathBuf {
    dependency_dirs(deps_dir).into_iter()
        .map(|dir| dir.join(file_name))
        .find(|path| path.exists())
        .unwrap_or_else(|| deps_dir.join(file_name))
}

fn push_dependency_search_paths(search_paths: &mut Vec<SearchPath>, deps_dir: &Path) {
    search_paths.extend(dependency_dirs(deps_dir).into_iter().map(|dir| SearchPath { kind: PathKind::Dependency, dir: dir.into() }));
}

/// The specialized mutant crate's rlib, and its `.rmeta` if one was written, as `-Zembed-metadata=no` needs.
fn specialized_mutant_crate_paths(metadata_file_path: &Path, exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut file_paths = vec![];
    if exists(metadata_file_path) { file_paths.push(metadata_file_path.to_owned()); }
    file_paths.push(metadata_file_path.with_extension("rlib"));
    file_paths
}

pub fn inject_runtime_crate_and_deps(config: &Config, compiler_config: &mut CompilerConfig, specialized_external_mutant_crate: Option<&(String, SpecializedMutantCrateCompilationResult)>) {
    // Generated harnesses need the old solver for the runtime's `generic_const_exprs` types.
    compiler_config.opts.unstable_opts.next_solver.globally = false;

    let early_dcx = EarlyDiagCtxt::new(compiler_config.opts.error_format);

    let host_triple = host_tuple();
    let target_triple = compiler_config.opts.target_triple.tuple();

    // Only the host runtime is embedded; other targets use the search path.
    let runtime_embedded = cfg!(feature = "embed-runtime") && target_triple == host_triple && !config.opts.unstable_flags.embedded;
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

    // The embedded stub re-exports its dependencies, preserving their identities in its metadata.
    if !config.opts.unstable_flags.embedded {
        for &(visible_crate_name, dep_file_name) in rlib_catalog::MUTEST_RUNTIME_PUBLIC_DEPS {
            let dep_file_paths = BTreeSet::from([
                CanonicalizedPath::new(locate_dependency(mutest_target_deps_dir_path, dep_file_name)),
            ]);
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

    if let Some((_, specialized)) = specialized_external_mutant_crate {
        // NOTE: The specialized crate may link dependencies from outside this crate's search paths, such as the mutest runtime.
        let dependency_dirs = specialized.dependencies.iter().filter_map(|dependency| dependency.parent()).collect::<BTreeSet<_>>();
        compiler_config.opts.search_paths.extend(dependency_dirs.into_iter().map(|dir| SearchPath { kind: PathKind::Dependency, dir: dir.to_owned().into() }));

        // Link against the specialized builds of the crates that depend on the mutant crate.
        for entry in externs.values_mut() {
            let ExternLocation::ExactPaths(paths) = &mut entry.location else { continue; };
            *paths = paths.iter()
                .map(|path| {
                    let replacement = specialized.dependent_outputs.iter().find(|(original, _)| path.canonicalized() == CanonicalizedPath::new(original.clone()).canonicalized());
                    replacement.map_or_else(|| path.clone(), |(_, output)| CanonicalizedPath::new(output.clone()))
                })
                .collect();
        }
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

    use super::specialized_mutant_crate_paths;

    #[cfg(feature = "embed-runtime")]
    #[test]
    fn an_extracted_file_replaces_a_stale_one_and_leaves_nothing_beside_it() {
        use std::fs;

        let dir_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-scratch").join(format!("extract-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir_path);
        fs::create_dir_all(&dir_path).unwrap();
        let file_path = dir_path.join("libmutest_runtime.rlib");
        fs::write(&file_path, b"stale").unwrap();

        super::extract_file(&file_path, b"current");
        super::extract_file(&file_path, b"current");

        let files = fs::read_dir(&dir_path).unwrap().map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
        let content = fs::read(&file_path).unwrap();
        fs::remove_dir_all(&dir_path).unwrap();
        assert_eq!(files, [file_path]);
        assert_eq!(content, b"current");
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
