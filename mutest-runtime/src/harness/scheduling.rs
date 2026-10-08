//! The dynamic scheduler runs mutants side by side in one process while the tests of each reach only its own code.
//! An isolated mutant takes no part in this: its tests run in child processes, which see only its own substitutions.

use crate::config::MutationIsolation;
use crate::journal::{self, WorkerJournal};
use crate::metadata::{MutationConflictsMeta, StandaloneMutantMeta, SubstMap};

#[derive(Clone, Copy)]
pub(super) struct ScheduledMutant {
    pub(super) mutant: &'static StandaloneMutantMeta,
    pub(super) isolated: bool,
}

impl ScheduledMutant {
    /// A test that reaches the mutation past call graph limits may reach any other mutant of the process, so it runs isolated.
    pub(super) fn new(mutant: &'static StandaloneMutantMeta, journal: Option<&WorkerJournal>, isolation: MutationIsolation) -> Self {
        let isolated = mutant.mutation.reached_by_truncated_entry_point || journal::isolates(journal, isolation, &[mutant.mutation]);
        Self { mutant, isolated }
    }
}

/// Whether `candidate` may run alongside the scheduled mutants; conflicts matter only between mutants of this process.
pub(super) fn runs_alongside(candidate: ScheduledMutant, scheduled: impl IntoIterator<Item = ScheduledMutant>, conflicts: &MutationConflictsMeta) -> bool {
    candidate.isolated || scheduled.into_iter()
        .filter(|other| !other.isolated)
        .all(|other| !conflicts.conflicting_mutations(candidate.mutant.mutation.id, other.mutant.mutation.id))
}

/// The substitutions of the scheduled mutants whose tests run in this process.
pub(super) fn in_process_substitutions<S: SubstMap>(scheduled: impl IntoIterator<Item = ScheduledMutant>) -> S {
    let mut substitutions = S::empty();
    for scheduled in scheduled.into_iter().filter(|scheduled| !scheduled.isolated) {
        substitutions.overlay(scheduled.mutant.substitutions);
    }
    substitutions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StaticBitMatrix;
    use crate::metadata::{EntryPoints, MutationMeta, MutationSafety, SubstMeta};

    macro fixture($name:ident, $mutant:ident, $id:literal, $safety:ident, $truncated:literal) {
        static $name: MutationMeta = MutationMeta {
            id: $id, safety: MutationSafety::$safety, op_name: "fixture", display_name: stringify!($name), display_location: "fixture",
            reachable_from: EntryPoints::InternalTests(phf::phf_map! { "shared" => 0usize }), reached_by_truncated_entry_point: $truncated,
            undetected_diagnostic: "survived",
        };
        static $mutant: StandaloneMutantMeta = StandaloneMutantMeta { mutation: &$name, substitutions: &[($id - 1, SubstMeta { mutation: &$name })] };
    }
    fixture!(SAFE, SAFE_MUTANT, 1, Safe, false);
    fixture!(UNSAFE, UNSAFE_MUTANT, 2, Unsafe, false);
    fixture!(TRUNCATED, TRUNCATED_MUTANT, 3, Safe, true);
    // Every pair shares the test "shared", so every pair conflicts.
    static CONFLICT_MATRIX: StaticBitMatrix<4> = StaticBitMatrix::from_symmetric_pairs(&[(1, 2), (1, 3), (2, 3)]);
    static CONFLICTS: MutationConflictsMeta = MutationConflictsMeta::from_static(&CONFLICT_MATRIX);

    fn schedule(mutant: &'static StandaloneMutantMeta) -> ScheduledMutant {
        ScheduledMutant::new(mutant, None, MutationIsolation::Unsafe)
    }

    #[test]
    fn isolated_mutant_runs_alongside_conflicting_ones() {
        let (safe, unsafe_) = (schedule(&SAFE_MUTANT), schedule(&UNSAFE_MUTANT));
        assert!(!safe.isolated && unsafe_.isolated);

        assert!(runs_alongside(safe, [unsafe_], &CONFLICTS));
        assert!(runs_alongside(unsafe_, [safe], &CONFLICTS));
        assert!(!runs_alongside(safe, [ScheduledMutant { isolated: false, ..unsafe_ }], &CONFLICTS));
    }

    #[test]
    fn process_activates_substitutions_of_its_own_mutants_only() {
        let substitutions = in_process_substitutions::<[Option<SubstMeta>; 3]>([schedule(&SAFE_MUTANT), schedule(&UNSAFE_MUTANT)]);
        assert!(substitutions.subst_at(0).is_some_and(|subst| subst.mutation.id == 1));
        assert!(substitutions.subst_at(1).is_none());
    }

    #[test]
    fn mutation_reached_past_call_graph_limits_runs_isolated() {
        let (safe, truncated) = (schedule(&SAFE_MUTANT), schedule(&TRUNCATED_MUTANT));
        assert!(truncated.isolated);
        assert!(runs_alongside(safe, [truncated], &CONFLICTS));
    }
}
