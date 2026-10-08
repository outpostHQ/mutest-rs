use crate::analysis::hir;
use crate::codegen::ast;
use crate::codegen::ast::visit::Visitor;
use crate::codegen::mutation::UnsafeTargeting;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnsafeSource {
    EnclosingUnsafe,
    Unsafe,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unsafety {
    None,
    /// Safe code called from an unsafe context.
    Tainted(UnsafeSource),
    Unsafe(UnsafeSource),
}

impl Unsafety {
    pub fn any(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn is_unsafe(&self, unsafe_targeting: UnsafeTargeting) -> bool {
        matches!((unsafe_targeting, self),
            | (_, Unsafety::Unsafe(UnsafeSource::Unsafe) | Unsafety::Tainted(UnsafeSource::Unsafe))
            | (UnsafeTargeting::None, Unsafety::Unsafe(_) | Unsafety::Tainted(_))
            | (UnsafeTargeting::OnlyEnclosing(hir::Safety::Unsafe), Unsafety::Unsafe(UnsafeSource::EnclosingUnsafe) | Unsafety::Tainted(UnsafeSource::EnclosingUnsafe))
        )
    }
}

struct BodyUnsafetyChecker {
    unsafety: Option<Unsafety>,
}

impl<'ast> ast::visit::Visitor<'ast> for BodyUnsafetyChecker {
    fn visit_block(&mut self, block: &'ast ast::Block) {
        if let ast::BlockCheckMode::Unsafe(ast::UnsafeSource::UserProvided) = block.rules {
            self.unsafety = Some(Unsafety::Unsafe(UnsafeSource::EnclosingUnsafe));
            return;
        }

        ast::visit::walk_block(self, block);
    }
}

pub fn check_item_unsafety<'ast>(item: ast::DefItem<'ast>) -> Unsafety {
    let ast::DefItemKind::Fn(target_fn) = item.kind() else { return Unsafety::None };

    let (ast::Safety::Default | ast::Safety::Safe(_)) = target_fn.sig.header.safety else { return Unsafety::Unsafe(UnsafeSource::Unsafe) };

    let Some(target_body) = &target_fn.body else { return Unsafety::None };
    let mut checker = BodyUnsafetyChecker { unsafety: None };
    checker.visit_block(target_body);
    checker.unsafety.unwrap_or(Unsafety::None)
}

pub fn collect_unsafe_blocks<'tcx>(body_hir: &'tcx hir::Body<'tcx>, root_scope_safety: hir::Safety) -> Vec<&'tcx hir::Block<'tcx>> {
    struct BodyUnsafeBlockCollector<'tcx> {
        current_scope_safety: hir::Safety,
        unsafe_blocks: Vec<&'tcx hir::Block<'tcx>>,
    }

    impl<'tcx> hir::intravisit::Visitor<'tcx> for BodyUnsafeBlockCollector<'tcx> {
        fn visit_block(&mut self, block: &'tcx hir::Block<'tcx>) {
            let previous_scope_unsafety = self.current_scope_safety;
            self.current_scope_safety = match block.rules {
                | hir::BlockCheckMode::DefaultBlock
                // NOTE: We explicitly ignore compiler-generated unsafe blocks.
                | hir::BlockCheckMode::UnsafeBlock(hir::UnsafeSource::CompilerGenerated)
                => self.current_scope_safety,

                hir::BlockCheckMode::UnsafeBlock(hir::UnsafeSource::UserProvided) => hir::Safety::Unsafe,
            };

            if self.current_scope_safety == hir::Safety::Unsafe {
                self.unsafe_blocks.push(block);
            }

            hir::intravisit::walk_block(self, block);

            self.current_scope_safety = previous_scope_unsafety;
        }
    }

    let mut collector = BodyUnsafeBlockCollector {
        current_scope_safety: root_scope_safety,
        unsafe_blocks: vec![],
    };
    hir::intravisit::Visitor::visit_body(&mut collector, body_hir);
    collector.unsafe_blocks
}
