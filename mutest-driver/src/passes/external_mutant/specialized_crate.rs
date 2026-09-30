use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_interface::interface::Result as CompilerResult;
use rustc_session::EarlyDiagCtxt;
use rustc_session::config::{ErrorOutputType, OutFileName, OutputFilenames, OutputType, OutputTypes};

use crate::config::{self, Config};
use crate::passes::analysis::AnalysisPassResult;
use crate::passes::compilation::CompilationPassResult;
use crate::passes::external_mutant::ExternalTargets;
use super::{invocation, replay};

pub struct SpecializedMutantCrateCompilationRequest {
    pub replay_record: invocation::Record,
    pub specialized_extra_filename: String,
    pub external_targets: ExternalTargets,
    /// The crates built on the mutant crate, which have to be compiled against the specialized one.
    pub dependents: Vec<invocation::Record>,
}

pub struct NestedRunResult {
    pub analysis_pass: Option<AnalysisPassResult>,
    pub compilation_pass: Option<CompilationPassResult>,
}

pub struct SpecializedMutantCrateCompilationResult {
    pub nested_run_result: NestedRunResult,
    pub duration: Duration,
    pub outputs: Option<Arc<OutputFilenames>>,
    /// Each original artifact of the mutant crate and its dependents, with its specialized replacement.
    pub dependent_outputs: Vec<(PathBuf, PathBuf)>,
    pub dependencies: Vec<PathBuf>,
}

pub fn compile_specialized_mutant_crate(
    config: &Config,
    _print_opts: config::PrintOptions,
    request: SpecializedMutantCrateCompilationRequest,
) -> CompilerResult<SpecializedMutantCrateCompilationResult> {
    let t_start = Instant::now();
    let early_dcx = EarlyDiagCtxt::new(ErrorOutputType::default());
    let SpecializedMutantCrateCompilationRequest { replay_record, specialized_extra_filename: suffix, external_targets, dependents } = request;

    // NOTE: The child runs in the recorded working directory, so it is given absolute paths.
    let exchange = std::path::absolute(config.target_dir_root().join(format!("mutest-replay-{}", process::id())))
        .unwrap_or_else(|error| early_dcx.early_fatal(format!("cannot resolve the mutest target directory: {error}")));
    let result = fs::create_dir_all(&exchange)
        .map_err(|error| format!("cannot create `{}`: {error}", exchange.display()))
        .and_then(|()| compile_in_child(config, &replay_record, &suffix, external_targets, &exchange));
    let _ = fs::remove_dir_all(&exchange);
    let (nested_run_result, outputs, dependencies) = result.unwrap_or_else(|error| early_dcx.early_fatal(error));

    let mut dependent_outputs = vec![];
    if outputs.is_some() {
        dependent_outputs.extend(replay_record.paired_outputs.iter().map(|artifact| (artifact.clone(), with_suffix(artifact, &suffix))));
        for dependent in &dependents {
            rebuild_dependent(dependent, &suffix, &mut dependent_outputs, &dependencies).unwrap_or_else(|error| early_dcx.early_fatal(error));
        }
    }

    Ok(SpecializedMutantCrateCompilationResult {
        nested_run_result,
        duration: t_start.elapsed(),
        outputs,
        dependent_outputs,
        dependencies,
    })
}

/// The child's run, its output filenames, and the dependencies it was compiled against.
type ChildCompilation = (NestedRunResult, Option<Arc<OutputFilenames>>, Vec<PathBuf>);

/// Compiles the specialized mutant crate in a child driver, which reports back through `exchange`.
fn compile_in_child(config: &Config, record: &invocation::Record, suffix: &str, external_targets: ExternalTargets, exchange: &Path) -> Result<ChildCompilation, String> {
    let request_path = exchange.join("request.json");
    let result_path = exchange.join("result.json");
    replay::write(&request_path, &replay::Request {
        targets: replay::Targets::encode(external_targets),
        suffix: suffix.to_owned(),
        cargo_target_kind: config.opts.cargo_target_kind,
        metadata_directory: config.opts.write_opts.out_dir.clone(),
        result: result_path.clone(),
    })?;

    let mut command = replay_command(record)?;
    command.env("MUTEST_REPLAY_REQUEST", &request_path);
    for name in ["MUTEST_ENCODED_ARGS", "MUTEST_ARGS", "MUTEST_TARGET_DIR_ROOT", "MUTEST_SEARCH_PATH"] {
        if let Some(value) = std::env::var_os(name) { command.env(name, value); }
    }
    command.args(&record.invocation.args[1..]);
    let status = command.status().map_err(|error| format!("cannot start the specialized mutant crate compilation: {error}"))?;
    if !status.success() { return Err(format!("the specialized mutant crate compilation failed with {status}")); }

    let result = replay::read::<replay::ResultRecord>(&result_path)?;
    let compilation_pass = match (result.metadata, result.compilation_duration) {
        (Some(metadata), Some(duration)) => {
            let output_types = OutputTypes::new(&[(OutputType::Metadata, Some(OutFileName::Real(metadata.clone())))]);
            let outputs = Arc::new(OutputFilenames::new(metadata.parent().unwrap().to_owned(), "specialized".to_owned(), "specialized".to_owned(), None, None, None, None, String::new(), output_types));
            Some(CompilationPassResult { duration, outputs, dependencies: result.dependencies.clone() })
        }
        (None, None) if !config.opts.outputs.contains(&config::OutputKind::TestBin) => None,
        _ => return Err("the specialized mutant crate compilation produced no artifacts".to_owned()),
    };
    let outputs = compilation_pass.as_ref().map(|pass| pass.outputs.clone());

    Ok((NestedRunResult { analysis_pass: Some(result.analysis), compilation_pass }, outputs, result.dependencies))
}

/// A driver process running in the working directory and environment `record` was compiled with.
fn replay_command(record: &invocation::Record) -> Result<Command, String> {
    let mut command = Command::new(std::env::current_exe().map_err(|error| format!("cannot find mutest-driver: {error}"))?);
    command.env_clear().current_dir(&record.invocation.working_directory);
    for (name, value) in &record.invocation.env_vars {
        if let Some(value) = value { command.env(name, value); }
    }
    // NOTE: The driver itself needs this process's library path to load the compiler.
    for name in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
        match std::env::var_os(name) {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    Ok(command)
}

/// `artifact` with `suffix` added to its file stem, as `-C extra-filename` adds it.
fn with_suffix(artifact: &Path, suffix: &str) -> PathBuf {
    let stem = artifact.file_stem().unwrap_or_default().to_string_lossy();
    match artifact.extension() {
        Some(extension) => artifact.with_file_name(format!("{stem}{suffix}.{}", extension.to_string_lossy())),
        None => artifact.with_file_name(format!("{stem}{suffix}")),
    }
}

/// Points `--extern` arguments at the replacements of the artifacts they name.
fn rebind_externs(args: &mut [String], working_directory: &Path, replacements: &[(PathBuf, PathBuf)]) {
    for index in 0..args.len() {
        let (prefix, value) = match args[index].strip_prefix("--extern=") {
            Some(value) => ("--extern=", value),
            None if index > 0 && args[index - 1] == "--extern" => ("", args[index].as_str()),
            None => continue,
        };
        let Some((name, path)) = value.split_once('=') else { continue; };
        // NOTE: Replaced artifacts are recorded by their canonical paths.
        let Ok(path) = fs::canonicalize(working_directory.join(path)) else { continue; };
        if let Some((_, replacement)) = replacements.iter().find(|(original, _)| *original == path) {
            args[index] = format!("{prefix}{name}={}", replacement.display());
        }
    }
}

/// Compiles a dependent of the mutant crate again, against the specialized crates built so far.
fn rebuild_dependent(record: &invocation::Record, suffix: &str, replacements: &mut Vec<(PathBuf, PathBuf)>, dependencies: &[PathBuf]) -> Result<(), String> {
    let mut args = record.invocation.args.clone();
    rebind_externs(&mut args, &record.invocation.working_directory, replacements);

    let mut suffixed = false;
    for arg in &mut args {
        if let Some(extra) = arg.strip_prefix("extra-filename=").or_else(|| arg.strip_prefix("-Cextra-filename=")) {
            let prefix = &arg[..arg.len() - extra.len()];
            *arg = format!("{prefix}{extra}{suffix}");
            suffixed = true;
        }
    }
    if !suffixed { args.extend(["-C".to_owned(), format!("extra-filename={suffix}")]); }

    // NOTE: The specialized crate links the mutest runtime, which the dependent has to find too.
    let search_paths = dependencies.iter().filter_map(|dependency| dependency.parent()).collect::<BTreeSet<_>>();
    for path in search_paths {
        args.extend(["-L".to_owned(), format!("dependency={}", path.display())]);
    }

    let status = replay_command(record)?.arg("--rustc").args(&args[1..]).status()
        .map_err(|error| format!("cannot start the compilation of a dependent: {error}"))?;
    if !status.success() { return Err(format!("the compilation of a dependent failed with {status}")); }

    replacements.extend(record.paired_outputs.iter().map(|artifact| (artifact.clone(), with_suffix(artifact, suffix))));
    Ok(())
}
