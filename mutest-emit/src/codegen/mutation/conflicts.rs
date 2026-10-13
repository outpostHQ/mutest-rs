use rustc_data_structures::fx::FxHashMap;
use rustc_index::bit_set::DenseBitSet;

use crate::codegen::substitution::conflicting_substs;

use super::{Mut, SubstDef, SubstLoc};

pub fn pairs_count(n: usize) -> usize {
    n * n.saturating_sub(1) / 2
}

/// The bits above the diagonal of a symmetric `n` by `n` bit matrix, row by row,
/// in the layout that `mutest_runtime::TriangleBitMatrixRef` reads.
pub struct TriangleBits {
    n: usize,
    bytes: Vec<u8>,
}

impl TriangleBits {
    pub fn new(n: usize) -> Self {
        Self { n, bytes: vec![0; pairs_count(n).div_ceil(8)] }
    }

    fn bit(&self, a: usize, b: usize) -> (usize, u8) {
        let (row, column) = (a.min(b), a.max(b));
        assert!(row < column && column < self.n, "({a}, {b}) is not above the diagonal of a {n} by {n} matrix", n = self.n);
        let bit = row * (2 * self.n - row - 1) / 2 + (column - row - 1);
        (bit / 8, 1 << (bit % 8))
    }

    pub fn insert(&mut self, a: usize, b: usize) {
        let (byte, mask) = self.bit(a, b);
        self.bytes[byte] |= mask;
    }

    pub fn contains(&self, a: usize, b: usize) -> bool {
        a != b && { let (byte, mask) = self.bit(a, b); self.bytes[byte] & mask != 0 }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The pairs of mutations whose tests cannot run in one process, by the position of each mutation in `in_process`.
pub fn in_process_conflicts(in_process: &[&Mut]) -> TriangleBits {
    let mut conflicts = TriangleBits::new(in_process.len());
    insert_entry_point_conflicts(&mut conflicts, in_process);
    insert_subst_conflicts(&mut conflicts, in_process);
    conflicts
}

/// To discern results related to the various concurrent mutations, they have to have distinct entry points.
fn insert_entry_point_conflicts(conflicts: &mut TriangleBits, in_process: &[&Mut]) {
    let mut reached_by_entry_point = FxHashMap::<_, DenseBitSet<usize>>::default();
    for (position, mutation) in in_process.iter().enumerate() {
        for (entry_point, _) in mutation.target.reachable_from.iter() {
            reached_by_entry_point.entry(entry_point).or_insert_with(|| DenseBitSet::new_empty(conflicts.n)).insert(position);
        }
    }

    let mut sharing_an_entry_point = DenseBitSet::new_empty(conflicts.n);
    for (position, mutation) in in_process.iter().enumerate() {
        sharing_an_entry_point.clear();
        mutation.target.reachable_from.iter().for_each(|(entry_point, _)| { sharing_an_entry_point.union(&reached_by_entry_point[&entry_point]); });
        sharing_an_entry_point.iter().filter(|&other| other > position).for_each(|other| conflicts.insert(position, other));
    }
}

/// The substitutions that make up each mutation cannot conflict with each other.
fn insert_subst_conflicts(conflicts: &mut TriangleBits, in_process: &[&Mut]) {
    let mut substs_by_location = FxHashMap::<SubstLoc, Vec<(usize, &SubstDef)>>::default();
    for (position, mutation) in in_process.iter().enumerate() {
        for subst in &mutation.substs {
            substs_by_location.entry(subst.location).or_default().push((position, subst));
        }
    }

    for substs in substs_by_location.values() {
        let pairs = substs.iter().enumerate().flat_map(|(i, a)| substs[i + 1..].iter().map(move |b| (a, b)));
        pairs.filter(|((a, subst_a), (b, subst_b))| a != b && conflicting_substs(subst_a, subst_b)).for_each(|((a, _), (b, _))| conflicts.insert(*a, *b));
    }
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;

    use rustc_data_structures::fx::FxHashSet;

    use super::TriangleBits;
    use crate::codegen::mutation::{MutId, MutationConflictGraph};

    #[test]
    fn a_pair_is_one_bit_of_the_row_of_its_lower_index() {
        let mut bits = TriangleBits::new(4);
        for (a, b) in [(2, 1), (1, 3), (3, 2)] { bits.insert(a, b); }

        // Rows 0, 1 and 2 hold 3, 2 and 1 bits, so (1, 2), (1, 3) and (2, 3) are bits 3, 4 and 5.
        // mutest-runtime's `TriangleBitMatrixRef` test reads these bytes.
        assert_eq!(bits.bytes(), &[0b0011_1000]);
        assert!(bits.contains(3, 1) && bits.contains(1, 3));
        assert!(!bits.contains(0, 1) && !bits.contains(2, 2));
    }

    #[test]
    fn every_pair_has_a_bit_of_its_own() {
        let n = 70;
        let mut bits = TriangleBits::new(n);
        assert_eq!(bits.bytes(), [0; 302]);

        for a in 0..n {
            for b in a + 1..n {
                assert!(!bits.contains(a, b), "({a}, {b}) shares a bit with an earlier pair");
                bits.insert(b, a);
            }
        }
        // 70 * 69 / 2 = 2415 pairs fill 301 bytes and 7 bits of the last byte.
        assert_eq!(bits.bytes()[..301], [0xFF; 301]);
        assert_eq!(bits.bytes()[301..], [0b0111_1111]);
    }

    #[test]
    fn a_mutation_outside_the_process_conflicts_with_all() {
        // Mutations 1, 2 and 3 may share a process, and only 1 and 2 conflict in it. Mutation 4 is unsafe.
        let mut conflicts = TriangleBits::new(3);
        conflicts.insert(0, 1);
        let in_process = vec![MutId(1), MutId(2), MutId(3)];
        let graph = MutationConflictGraph { n_mutations: 4, unsafes: FxHashSet::from_iter([MutId(4)]), in_process, conflicts, phantom: PhantomData };
        let ids = |pairs: &mut dyn Iterator<Item = (MutId, MutId)>| pairs.map(|(a, b)| (a.0, b.0)).collect::<Vec<_>>();

        assert_eq!(ids(&mut graph.iter_conflicts()), [(1, 2), (1, 4), (2, 4), (3, 4)]);
        assert_eq!(ids(&mut graph.iter_conflicts_excluding_unsafe()), [(1, 2)]);
        assert_eq!(ids(&mut graph.iter_compatibilities()), [(1, 3), (2, 3)]);
    }

    #[test]
    #[should_panic = "(1, 4) is not above the diagonal of a 4 by 4 matrix"]
    fn a_pair_outside_the_matrix_is_refused() {
        TriangleBits::new(4).insert(1, 4);
    }
}
