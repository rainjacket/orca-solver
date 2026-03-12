//! Fixed-size bitset backed by `Vec<u64>` for high-performance set operations
//! on crossword candidate sets (~100K bits). The hot path (`and_with`) compiles
//! to SIMD instructions (NEON / AVX).

use std::fmt;

/// The core data structure for candidate filtering. See module docs.
#[derive(Clone)]
pub struct BitSet {
    blocks: Vec<u64>,
    num_bits: usize,
}

impl BitSet {
    /// Create a new bitset with all bits cleared.
    pub fn new(num_bits: usize) -> Self {
        let num_blocks = num_bits.div_ceil(64);
        BitSet {
            blocks: vec![0u64; num_blocks],
            num_bits,
        }
    }

    /// Create a new bitset with all bits set.
    pub fn new_all_set(num_bits: usize) -> Self {
        let num_blocks = num_bits.div_ceil(64);
        let mut blocks = vec![!0u64; num_blocks];
        // Clear excess bits in the last block
        let excess = num_blocks * 64 - num_bits;
        if excess > 0 {
            let last = blocks.len() - 1;
            blocks[last] >>= excess;
        }
        BitSet { blocks, num_bits }
    }

    /// Number of bits in the bitset.
    pub fn len(&self) -> usize {
        self.num_bits
    }

    /// Set bit at index.
    pub fn set(&mut self, idx: usize) {
        debug_assert!(idx < self.num_bits);
        self.blocks[idx / 64] |= 1u64 << (idx % 64);
    }

    /// Test whether bit at index is set.
    pub fn test(&self, idx: usize) -> bool {
        debug_assert!(idx < self.num_bits);
        (self.blocks[idx / 64] & (1u64 << (idx % 64))) != 0
    }

    /// Returns true if no bits are set.
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|&b| b == 0)
    }

    /// Count of set bits.
    pub fn count_ones(&self) -> u32 {
        self.blocks.iter().map(|b| b.count_ones()).sum()
    }

    /// In-place OR with another bitset.
    pub fn or_with(&mut self, other: &BitSet) {
        debug_assert_eq!(self.blocks.len(), other.blocks.len());
        for (a, &b) in self.blocks.iter_mut().zip(other.blocks.iter()) {
            *a |= b;
        }
    }

    #[inline]
    pub fn and_with(&mut self, other: &BitSet) -> bool {
        debug_assert_eq!(self.blocks.len(), other.blocks.len());
        let mut any_set = 0u64;
        for (a, &b) in self.blocks.iter_mut().zip(other.blocks.iter()) {
            *a &= b;
            any_set |= *a;
        }
        any_set != 0
    }

    /// Count of set bits in the intersection with another bitset.
    #[inline]
    pub fn count_intersection(&self, other: &BitSet) -> u32 {
        debug_assert_eq!(self.blocks.len(), other.blocks.len());
        self.blocks
            .iter()
            .zip(other.blocks.iter())
            .map(|(&a, &b)| (a & b).count_ones())
            .sum()
    }

    /// Check if intersection is non-empty without mutating either bitset.
    /// Short-circuits on first non-zero block.
    #[inline]
    pub fn has_intersection(&self, other: &BitSet) -> bool {
        debug_assert_eq!(self.blocks.len(), other.blocks.len());
        for (&a, &b) in self.blocks.iter().zip(other.blocks.iter()) {
            if a & b != 0 {
                return true;
            }
        }
        false
    }

    /// Iterate over indices of set bits.
    pub fn iter_ones(&self) -> IterOnes<'_> {
        IterOnes {
            blocks: &self.blocks,
            block_idx: 0,
            current: if self.blocks.is_empty() {
                0
            } else {
                self.blocks[0]
            },
        }
    }

    /// Return the index of the first set bit, or None.
    pub fn first_one(&self) -> Option<usize> {
        for (i, &block) in self.blocks.iter().enumerate() {
            if block != 0 {
                return Some(i * 64 + block.trailing_zeros() as usize);
            }
        }
        None
    }

    /// Raw blocks access for low-level operations.
    pub fn blocks(&self) -> &[u64] {
        &self.blocks
    }

    /// Mutable raw blocks access.
    pub fn blocks_mut(&mut self) -> &mut [u64] {
        &mut self.blocks
    }
}

impl fmt::Debug for BitSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BitSet({} bits, {} set)",
            self.num_bits,
            self.count_ones()
        )
    }
}

impl PartialEq for BitSet {
    fn eq(&self, other: &Self) -> bool {
        self.num_bits == other.num_bits && self.blocks == other.blocks
    }
}

impl Eq for BitSet {}

/// Iterator over set bit indices, using trailing_zeros for efficiency.
pub struct IterOnes<'a> {
    blocks: &'a [u64],
    block_idx: usize,
    current: u64,
}

impl<'a> Iterator for IterOnes<'a> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        loop {
            if self.current != 0 {
                let tz = self.current.trailing_zeros() as usize;
                // Clear the lowest set bit
                self.current &= self.current - 1;
                return Some(self.block_idx * 64 + tz);
            }
            self.block_idx += 1;
            if self.block_idx >= self.blocks.len() {
                return None;
            }
            self.current = self.blocks[self.block_idx];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_empty() {
        let bs = BitSet::new(100);
        assert!(bs.is_empty());
        assert_eq!(bs.count_ones(), 0);
        assert_eq!(bs.len(), 100);
    }

    #[test]
    fn test_new_all_set() {
        let bs = BitSet::new_all_set(100);
        assert!(!bs.is_empty());
        assert_eq!(bs.count_ones(), 100);
    }

    #[test]
    fn test_new_all_set_exact_multiple() {
        let bs = BitSet::new_all_set(128);
        assert_eq!(bs.count_ones(), 128);
    }

    #[test]
    fn test_set_clear_test() {
        let mut bs = BitSet::new(200);
        bs.set(0);
        bs.set(63);
        bs.set(64);
        bs.set(127);
        bs.set(199);

        assert!(bs.test(0));
        assert!(bs.test(63));
        assert!(bs.test(64));
        assert!(bs.test(127));
        assert!(bs.test(199));
        assert!(!bs.test(1));
        assert!(!bs.test(128));
        assert_eq!(bs.count_ones(), 5);
    }

    #[test]
    fn test_and_with() {
        let mut a = BitSet::new(128);
        let mut b = BitSet::new(128);
        a.set(10);
        a.set(50);
        a.set(100);
        b.set(50);
        b.set(100);
        b.set(120);

        let non_empty = a.and_with(&b);
        assert!(non_empty);
        assert!(!a.test(10));
        assert!(a.test(50));
        assert!(a.test(100));
        assert!(!a.test(120));
        assert_eq!(a.count_ones(), 2);
    }

    #[test]
    fn test_and_with_empty_result() {
        let mut a = BitSet::new(128);
        let mut b = BitSet::new(128);
        a.set(10);
        b.set(20);

        let non_empty = a.and_with(&b);
        assert!(!non_empty);
        assert!(a.is_empty());
    }

    #[test]
    fn test_has_intersection() {
        let mut a = BitSet::new(128);
        let mut b = BitSet::new(128);
        a.set(50);
        b.set(50);

        assert!(a.has_intersection(&b));

        let c = BitSet::new(128);
        assert!(!a.has_intersection(&c));
    }

    #[test]
    fn test_iter_ones() {
        let mut bs = BitSet::new(200);
        bs.set(0);
        bs.set(63);
        bs.set(64);
        bs.set(127);
        bs.set(199);

        let ones: Vec<usize> = bs.iter_ones().collect();
        assert_eq!(ones, vec![0, 63, 64, 127, 199]);
    }

    #[test]
    fn test_iter_ones_empty() {
        let bs = BitSet::new(100);
        let ones: Vec<usize> = bs.iter_ones().collect();
        assert!(ones.is_empty());
    }

    #[test]
    fn test_first_one() {
        let mut bs = BitSet::new(200);
        assert_eq!(bs.first_one(), None);
        bs.set(150);
        assert_eq!(bs.first_one(), Some(150));
        bs.set(50);
        assert_eq!(bs.first_one(), Some(50));
    }

    #[test]
    fn test_all_set_boundary() {
        // Test that all_set doesn't set bits beyond len
        for len in [1, 2, 63, 64, 65, 127, 128, 129] {
            let bs = BitSet::new_all_set(len);
            assert_eq!(bs.count_ones(), len as u32, "failed for len={}", len);
        }
    }
}
