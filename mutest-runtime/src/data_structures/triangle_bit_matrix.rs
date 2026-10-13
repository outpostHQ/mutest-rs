/// A symmetric `n` by `n` bit matrix with an empty diagonal, stored as the bits above the diagonal, row by row.
/// Bit `i` of the triangle is bit `i % 8` of byte `i / 8`.
#[derive(Clone, Copy, Debug)]
pub struct TriangleBitMatrixRef<'a> {
    n: u32,
    bytes: &'a [u8],
}

impl<'a> TriangleBitMatrixRef<'a> {
    pub const fn from_bytes(n: u32, bytes: &'a [u8]) -> Self {
        let n_bits = n as u64 * (n as u64).saturating_sub(1) / 2;
        assert!(bytes.len() as u64 == n_bits.div_ceil(8), "the byte count does not match the size of the matrix");
        Self { n, bytes }
    }

    pub const fn contains(&self, a: u32, b: u32) -> bool {
        assert!(a < self.n && b < self.n, "the index is outside the matrix");
        if a == b { return false; }

        let (row, column) = if a < b { (a as u64, b as u64) } else { (b as u64, a as u64) };
        let bit = row * (2 * self.n as u64 - row - 1) / 2 + (column - row - 1);
        self.bytes[(bit / 8) as usize] & (1 << (bit % 8)) != 0
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{self, UnwindSafe};

    use super::TriangleBitMatrixRef;

    // mutest-emit's `TriangleBits` test writes these bytes for the pairs (1, 2), (1, 3) and (2, 3).
    const MATRIX: TriangleBitMatrixRef<'static> = TriangleBitMatrixRef::from_bytes(4, &[0b0011_1000]);

    #[test]
    fn each_pair_reads_its_own_bit_in_either_order() {
        for (a, b) in [(1, 2), (1, 3), (2, 3)] {
            assert!(MATRIX.contains(a, b) && MATRIX.contains(b, a), "({a}, {b}) is not set");
        }
        for (a, b) in [(0, 1), (0, 2), (0, 3), (2, 2)] {
            assert!(!MATRIX.contains(a, b), "({a}, {b}) is set");
        }
    }

    #[test]
    fn an_empty_matrix_has_no_bytes() {
        assert!(!TriangleBitMatrixRef::from_bytes(1, &[]).contains(0, 0));
    }

    fn panic_message(f: impl FnOnce() + UnwindSafe) -> Option<&'static str> {
        panic::catch_unwind(f).err()?.downcast_ref::<&'static str>().copied()
    }

    #[test]
    fn bytes_of_another_size_are_refused() {
        let message = panic_message(|| { TriangleBitMatrixRef::from_bytes(5, &[0]); });
        assert_eq!(message, Some("the byte count does not match the size of the matrix"));
    }

    #[test]
    fn an_index_outside_the_matrix_is_refused() {
        let message = panic_message(|| { MATRIX.contains(1, 4); });
        assert_eq!(message, Some("the index is outside the matrix"));
    }
}
