use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_interface::{Linker, create_and_enter_global_ctxt, passes, run_compiler};
use rustc_interface::Config as CompilerConfig;
use rustc_interface::interface::Result as CompilerResult;
use rustc_lint_defs::Level as LintLevel;
use rustc_middle::ty::TyCtxt;
use rustc_session::config::{OptLevel, OutputFilenames, OutputType};
use rustc_session::output::filename_for_input;
use rustc_span::def_id::LOCAL_CRATE;

use crate::passes::base_compiler_config_from_parts;
use crate::passes::external_mutant::{crate_const_storage, invocation};

pub struct RecompilableDepCrateCompilationResult {
    pub duration: Duration,
    pub outputs: Arc<OutputFilenames>,
}

/// The artifacts other crates are compiled against: the crate's metadata and linkable files.
fn artifact_paths(tcx: TyCtxt<'_>, outputs: &OutputFilenames) -> Vec<PathBuf> {
    let mut artifacts = vec![];
    if tcx.sess.opts.output_types.contains_key(&OutputType::Metadata) {
        artifacts.push(outputs.path(OutputType::Metadata).as_path().to_owned());
    }
    if tcx.sess.opts.output_types.contains_key(&OutputType::Exe) {
        let crate_name = tcx.crate_name(LOCAL_CRATE);
        artifacts.extend(tcx.crate_types().iter().map(|&crate_type| filename_for_input(tcx.sess, crate_type, crate_name, outputs).as_path().to_owned()));
    }
    artifacts
}

/// Compiles a crate and records its invocation. A crate Cargo was asked for, or one built without Cargo,
/// is also marked as one that integration tests may mutate.
pub fn compile_recompilable_dep_crate(compiler_config: &CompilerConfig, args: &[String]) -> CompilerResult<RecompilableDepCrateCompilationResult> {
    let candidate = !rustc_session::utils::was_invoked_from_cargo() || std::env::var_os("CARGO_PRIMARY_PACKAGE").is_some();
    let mut compiler_config = base_compiler_config_from_parts(compiler_config, None);

    // NOTE: Disable all MIR optimizations in all cases to ensure identical MIRs
    //       during analysis regardless of final optimization level.
    compiler_config.opts.optimize = OptLevel::No;
    // NOTE: We must disable MIR optimizations to disable inlining of function calls,
    //       which is necessary to building a complete call graph in all circumstances.
    //       The MIR generated in this pass is not used for the final compilation anyway.
    compiler_config.opts.unstable_opts.mir_opt_level = Some(0);
    // NOTE: Ensure that the MIR of all items is encoded, regardless of whether they are
    //       needed for linking or binary codegen.
    //       This is needed to create the full call graph from an external crate.
    compiler_config.opts.unstable_opts.always_encode_mir = true;

    // Disable lints on generated crate code.
    compiler_config.opts.lint_cap = Some(LintLevel::Allow);

    let compilation_pass = run_compiler(compiler_config, |compiler| -> CompilerResult<RecompilableDepCrateCompilationResult> {
        let t_start = Instant::now();

        let sess = &compiler.sess;
        let codegen_backend = &*compiler.codegen_backend;

        let mut krate = passes::parse(sess);

        // NOTE: We must register our custom tool attribute namespace before the
        //       relevant attribute validation is performed during macro expansion.
        mutest_emit::codegen::tool_attr::register(sess, &mut krate);

        if candidate { crate_const_storage::embed_candidate_marker(&mut krate); }

        let ((linker, outputs, record, artifacts), incr_comp_session) = create_and_enter_global_ctxt(compiler, krate, |tcx| {
            let _ = tcx.resolver_for_lowering();

            passes::write_dep_info(tcx);

            passes::write_interface(tcx);

            tcx.ensure_ok().analysis(());

            let outputs = tcx.output_filenames(()).clone();
            let record = invocation::capture(tcx, args).unwrap_or_else(|error| tcx.dcx().fatal(error));
            let artifacts = artifact_paths(tcx, &outputs);
            let linker = Linker::codegen_and_build_linker(tcx, &*compiler.codegen_backend);

            (linker, outputs, record, artifacts)
        });

        linker.link(sess, incr_comp_session, codegen_backend);
        invocation::publish(record, &artifacts).unwrap_or_else(|error| sess.dcx().fatal(error));

        Ok(RecompilableDepCrateCompilationResult {
            duration: t_start.elapsed(),
            outputs,
        })
    })?;

    Ok(compilation_pass)
}
