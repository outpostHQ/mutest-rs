use std::collections::HashSet;

use crate::data_structures::TriangleBitMatrixRef;
use crate::harness::ActiveMutantHandle;

pub macro static_map($($input:tt)*) {
    {
        // NOTE: The `phf` crate name must exist in this generated scope
        //       for the `phf_map` macro to expand correctly.
        extern crate __mutest_runtime_public_dep_phf as phf;
        phf::phf_map!($($input)*)
    }
}

pub type TestPath = &'static str;

#[derive(Debug)]
pub struct ExternalTestsExtra {
    pub test_crate_name: &'static str,
}

#[derive(Debug)]
pub enum TestSuite<'a> {
    Tests(&'a [&'static ::test::TestDescAndFn], Option<&'static ExternalTestsExtra>),
}

pub fn reachable_tests(mutation: &MutationMeta, external_tests_extra: Option<&ExternalTestsExtra>) -> HashSet<TestPath> {
    match (&mutation.reachable_from, external_tests_extra) {
        (EntryPoints::InternalTests(reachable_from), None) => reachable_from.keys().copied().collect(),
        (EntryPoints::InternalTests(_), _) => panic!("encountered meta-mutant compiled for internal tests being run against external tests"),

        (EntryPoints::ExternalTests(reachable_from), Some(_external_tests_extra)) => reachable_from.keys().copied().collect(),
        (EntryPoints::ExternalTests(_), _) => panic!("encountered meta-mutant compiled for external tests being run without external test metadata"),
    }
}

pub fn reachable_tests_count(mutation: &MutationMeta, external_tests_extra: Option<&ExternalTestsExtra>) -> usize {
    match (&mutation.reachable_from, external_tests_extra) {
        (EntryPoints::InternalTests(reachable_from), None) => reachable_from.len(),
        (EntryPoints::InternalTests(_), _) => panic!("encountered meta-mutant compiled for internal tests being run against external tests"),

        (EntryPoints::ExternalTests(reachable_from), Some(_external_tests_extra)) => reachable_from.len(),
        (EntryPoints::ExternalTests(_), _) => panic!("encountered meta-mutant compiled for external tests being run without external test metadata"),
    }
}

pub fn test_reachability(mutation: &MutationMeta, test_path: &str, external_tests_extra: Option<&ExternalTestsExtra>) -> Option<usize> {
    match (&mutation.reachable_from, external_tests_extra) {
        (EntryPoints::InternalTests(reachable_from), None) => reachable_from.get(test_path).copied(),
        (EntryPoints::InternalTests(_), _) => panic!("encountered meta-mutant compiled for internal tests being run against external tests"),

        (EntryPoints::ExternalTests(reachable_from), Some(_external_tests_extra)) => reachable_from.get(test_path).copied(),
        (EntryPoints::ExternalTests(_), _) => panic!("encountered meta-mutant compiled for external tests being run without external test metadata"),
    }
}

pub type SubstLocIdx = usize;

/// The substitutions one mutant makes, by location; every other location keeps its original code.
/// Sparse, as a dense map per mutant would grow with the product of mutants and locations.
pub type Substs = &'static [(SubstLocIdx, SubstMeta)];

pub trait SubstMap: Sized + Clone {
    /// The map of no mutant, which a substitution point reads with one load while no mutant is active.
    const EMPTY: Self;

    fn empty() -> Self {
        Self::EMPTY
    }

    fn overlay(&mut self, substs: Substs);

    fn with(substs: Substs) -> Self {
        let mut subst_map = Self::empty();
        subst_map.overlay(substs);
        subst_map
    }

    fn subst_at(&self, subst_loc_idx: SubstLocIdx) -> Option<SubstMeta>;

    /// # Safety
    ///
    /// The substitution location index must be valid for the substitution map.
    unsafe fn subst_at_unchecked(&self, subst_loc_idx: SubstLocIdx) -> Option<SubstMeta>;
}

impl<const N: usize> SubstMap for [Option<SubstMeta>; N] {
    const EMPTY: Self = [None; N];

    fn overlay(&mut self, substs: Substs) {
        for &(subst_loc_idx, subst) in substs {
            self[subst_loc_idx] = Some(subst);
        }
    }

    #[inline(always)]
    fn subst_at(&self, subst_loc_idx: SubstLocIdx) -> Option<SubstMeta> {
        self[subst_loc_idx]
    }

    #[inline]
    unsafe fn subst_at_unchecked(&self, subst_loc_idx: SubstLocIdx) -> Option<SubstMeta> {
        // SAFETY: The caller must ensure that the substitution location index is
        //         valid for the active substitution map.
        unsafe { *self.get_unchecked(subst_loc_idx) }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SubstMeta {
    pub mutation: &'static MutationMeta,
}

#[derive(Debug)]
pub enum MutationSafety {
    Safe,
    Tainted,
    Unsafe,
}

#[derive(Debug)]
pub enum EntryPoints {
    InternalTests(phf::Map<TestPath, usize>),
    ExternalTests(phf::Map<TestPath, usize>),
}

#[derive(Debug)]
pub struct MutationMeta {
    pub id: u32,
    pub safety: MutationSafety,
    pub op_name: &'static str,
    pub display_name: &'static str,
    pub display_location: &'static str,
    pub reachable_from: EntryPoints,
    /// Whether a test that reaches the mutation may also reach any other mutation past call graph limits.
    pub reached_by_truncated_entry_point: bool,
    pub undetected_diagnostic: &'static str,
}

impl MutationMeta {
    pub fn is_unsafe(&self) -> bool {
        !matches!(self.safety, MutationSafety::Safe)
    }
}

/// Stands for no mutation in the slot of a guard, which is set for a function body to run the code that holds
/// its mutations. The ids of mutations start at 1.
pub static NO_MUTATION: MutationMeta = MutationMeta {
    id: 0,
    safety: MutationSafety::Safe,
    op_name: "",
    display_name: "",
    display_location: "",
    reachable_from: EntryPoints::InternalTests(phf::Map::new()),
    reached_by_truncated_entry_point: false,
    undetected_diagnostic: "",
};

#[derive(Debug)]
pub struct MutationConflictsMeta {
    /// The ids of the mutations whose tests may run in one process, in ascending order.
    in_process: &'static [u32],
    /// The conflicts between the mutations of `in_process`, by their position in it.
    conflicts: TriangleBitMatrixRef<'static>,
}

impl MutationConflictsMeta {
    pub const fn from_upper_triangle(in_process: &'static [u32], bytes: &'static [u8]) -> Self {
        Self { in_process, conflicts: TriangleBitMatrixRef::from_bytes(in_process.len() as u32, bytes) }
    }

    /// Whether the two mutations conflict in one process; the scheduler isolates mutations outside `in_process` itself.
    pub fn conflicting_mutations(&self, a: u32, b: u32) -> bool {
        let position = |id| self.in_process.binary_search(&id).ok();
        position(a).zip(position(b)).is_some_and(|(a, b)| self.conflicts.contains(a as u32, b as u32))
    }
}

#[derive(Debug)]
pub struct StandaloneMutantMeta {
    pub mutation: &'static MutationMeta,
    pub substitutions: Substs,
}

#[derive(Debug)]
pub struct BatchedMutantMeta {
    pub batch_id: u32,
    pub mutations: &'static [&'static MutationMeta],
    pub substitutions: Substs,
}

#[derive(Copy, Clone)]
pub enum Mutant {
    Mutation(&'static StandaloneMutantMeta),
    Batch(&'static BatchedMutantMeta),
}

impl Mutant {
    pub fn substitutions(&self) -> Substs {
        match self {
            Self::Mutation(mutant) => mutant.substitutions,
            Self::Batch(mutant) => mutant.substitutions,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum MutationParallelism {
    None(&'static [StandaloneMutantMeta]),
    Batched(&'static [BatchedMutantMeta]),
    DynamicallyScheduled(&'static [StandaloneMutantMeta], &'static MutationConflictsMeta),
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CargoTargetKind {
    Lib,
    MainBin,
    Bin,
    Example,
    Test,
}

#[derive(Debug)]
pub struct MetaMutant<S: SubstMap + 'static> {
    pub cargo_package_name: Option<&'static str>,
    pub cargo_target_kind: Option<CargoTargetKind>,
    pub crate_name: &'static str,

    pub active_mutant_handle: &'static ActiveMutantHandle<S>,
    pub mutations: &'static [&'static MutationMeta],
    pub mutation_parallelism: MutationParallelism,
}

impl<S: SubstMap + 'static> MetaMutant<S> {
    pub fn mutants(&self) -> Box<dyn Iterator<Item = Mutant>> {
        match self.mutation_parallelism {
            MutationParallelism::None(mutants) => Box::new(mutants.iter().map(|mutant| Mutant::Mutation(mutant))),
            MutationParallelism::Batched(mutants) => Box::new(mutants.iter().map(|mutant| Mutant::Batch(mutant))),
            MutationParallelism::DynamicallyScheduled(mutants, _) => Box::new(mutants.iter().map(|mutant| Mutant::Mutation(mutant))),
        }
    }

    pub fn find_mutant_with_mutation(&self, mutation_id: u32) -> Option<Mutant> {
        self.mutants().find(|mutant| {
            match mutant {
                Mutant::Mutation(mutant) => mutant.mutation.id == mutation_id,
                Mutant::Batch(mutant) => mutant.mutations.iter().any(|mutation| mutation.id == mutation_id),
            }
        })
    }
}
