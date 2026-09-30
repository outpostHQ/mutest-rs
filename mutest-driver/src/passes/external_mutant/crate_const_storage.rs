use mutest_emit::analysis::hir;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::harness::mk_crate_kind_const;
use mutest_emit::codegen::symbols::{DUMMY_SP, Ident, sym};
use rustc_data_structures::thin_vec::thin_vec;
use rustc_middle::ty::TyCtxt;

use crate::passes::external_mutant::invocation;

/// Marks the crate as one that its integration tests may mutate.
pub fn embed_candidate_marker(krate: &mut ast::Crate) {
    // pub const CRATE_KIND: &str = "recompilable_dep_crate";
    let crate_kind_const = mk_crate_kind_const(DUMMY_SP, "recompilable_dep_crate");

    // pub mod mutest_generated { ... }
    let mutest_generated_mod = ast::mk::item_mod(DUMMY_SP,
        ast::mk::vis_pub(DUMMY_SP),
        Ident::new(sym::mutest_generated, DUMMY_SP),
        thin_vec![crate_kind_const],
    );

    krate.items.push(mutest_generated_mod);
}

/// The recorded invocation of an extern crate, if it has an artifact to read it from.
pub fn extract_replay_record(tcx: TyCtxt<'_>, cnum: hir::CrateNum) -> Option<invocation::Record> {
    let source = tcx.used_crate_source(cnum);
    let artifact = source.rmeta.as_ref().or(source.rlib.as_ref())?;
    Some(invocation::load(artifact).unwrap_or_else(|error| tcx.dcx().fatal(error)))
}
