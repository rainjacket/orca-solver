//! Positive-only witnesses; snapshot-backed masks carry negative knowledge.
use orca_core::{bitset::BitSet, dict::LengthBucket};

const UNKNOWN: u32 = u32::MAX;

/// Supporting word IDs for one fixed directed crossing in one dictionary.
/// Not trailed: every hit is revalidated against the current candidate domain.
#[derive(Clone)]
pub(crate) struct Witnesses([u32; 26]);

impl Default for Witnesses {
    fn default() -> Self {
        Self([UNKNOWN; 26])
    }
}

impl Witnesses {
    /// Return exact support among `allowed`, a conservative viable-letter bound.
    /// Missing support is recorded in the caller's mask, never persisted here.
    #[inline]
    pub(crate) fn letters(
        &mut self,
        domain: &BitSet,
        bucket: &LengthBucket,
        pos: usize,
        mut allowed: u32,
    ) -> u32 {
        let mut result = 0;
        while allowed != 0 {
            let letter = allowed.trailing_zeros() as usize;
            allowed &= allowed - 1;
            let cached = &mut self.0[letter];
            if *cached != UNKNOWN && domain.test(*cached as usize) {
                result |= 1 << letter;
                continue;
            }
            *cached = UNKNOWN;
            for (block, (&a, &b)) in domain
                .blocks()
                .iter()
                .zip(bucket.letter_bits[pos][letter].blocks())
                .enumerate()
            {
                let overlap = a & b;
                if overlap != 0 {
                    let word = block * 64 + overlap.trailing_zeros() as usize;
                    assert!(word < UNKNOWN as usize);
                    *cached = word as u32;
                    result |= 1 << letter;
                    break;
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orca_core::dict::Dictionary;
    #[test]
    fn restored_masks_rediscover_letters_with_stale_witnesses() {
        let dict = Dictionary::parse("AAA;1\nAAB;1\nBAA;1\n").unwrap();
        let b = dict.bucket(3).unwrap();
        let mut w = Witnesses::default();
        let mut d = b.all.clone();
        assert_eq!(w.letters(&d, b, 0, 3), 3);
        d.blocks_mut()[0] &= !1;
        assert_eq!(w.letters(&d, b, 0, 3), 3);
        d.blocks_mut()[0] &= !2;
        assert_eq!(w.letters(&d, b, 0, 3), 2);
        // Restored masks allow A again; its previous failed search isn't reused.
        assert_eq!(w.letters(&b.all, b, 0, 3), 3);
        // A positive witness eliminated in a sibling branch must be replaced.
        let mut sibling = b.all.clone();
        sibling.blocks_mut()[0] &= !1;
        assert_eq!(w.letters(&sibling, b, 0, 3), 3);
    }
}
