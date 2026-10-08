use crate::harness::ActiveMutantHandle;

#[doc(hidden)]
pub extern crate __mutest_runtime_public_dep_phf as __mutest_runtime_phf;

#[macro_export]
macro_rules! static_map {
    ($($input:tt)*) => {{
        // phf's generated paths must refer to the same crate as EntryPoints::InternalTests.
        use $crate::__mutest_runtime_phf as phf;
        phf::phf_map!($($input)*)
    }};
}

pub type TestPath = &'static str;

#[derive(Debug)]
pub struct EmbeddedTestDescAndFn {
    pub name: TestPath,
    pub source_file: &'static str,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub ignore: bool,
    pub should_panic: bool,
    pub test_fn: fn() -> !,
}

#[derive(Debug)]
pub struct ExternalTestsExtra {
    pub test_crate_name: &'static str,
}

#[derive(Debug)]
pub enum TestSuite<'a> {
    Tests(&'a [&'static EmbeddedTestDescAndFn], Option<&'static ExternalTestsExtra>),
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
pub type Substs = &'static [(SubstLocIdx, SubstMeta)];

pub trait SubstMap: Sized + Clone {
    fn subst_at(&self, subst_loc_idx: SubstLocIdx) -> Option<SubstMeta>;

    /// # Safety
    ///
    /// The substitution location index must be valid for the substitution map.
    unsafe fn subst_at_unchecked(&self, subst_loc_idx: SubstLocIdx) -> Option<SubstMeta>;
}

impl<const N: usize> SubstMap for [Option<SubstMeta>; N] {
    #[inline]
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
    pub fn mutants(&self) -> impl Iterator<Item = Mutant> {
        match self.mutation_parallelism {
            MutationParallelism::None(mutants) => mutants.iter().map(|mutant| Mutant::Mutation(mutant)),
            MutationParallelism::Batched(_mutants) => panic!("embedded targets do not support mutation batching"),
        }
    }

    pub fn find_mutant_with_mutation(&self, mutation_id: u32) -> Option<Mutant> {
        self.mutants().find(|mutant| {
            match mutant {
                Mutant::Mutation(mutant) => mutant.mutation.id == mutation_id,
                Mutant::Batch(_mutant) => panic!("embedded targets do not support mutation batching"),
            }
        })
    }
}
