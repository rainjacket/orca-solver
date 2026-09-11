//! Candidate storage. Mutations keep bits, count, and nonempty-block index together.
use crate::bitset::BitSet;

/// A word domain with an exact index of its nonzero 64-bit blocks.
/// Cloning restores all derived metadata together with the candidate bits.
#[derive(Debug, Clone)]
pub struct CandidateSet {
    bits: BitSet,
    count: u32,
    active: Vec<usize>,
}

impl CandidateSet {
    pub fn new(bits: BitSet) -> Self {
        let count = bits.count_ones();
        let active = bits
            .blocks()
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| (b != 0).then_some(i))
            .collect();
        Self {
            bits,
            count,
            active,
        }
    }
    pub fn bits(&self) -> &BitSet {
        &self.bits
    }
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Always skip empty blocks when enumerating candidates. The density switch
    /// below applies only to filtering, not to this iterator.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.active.iter().flat_map(|&i| {
            let mut bits = self.bits.blocks()[i];
            std::iter::from_fn(move || {
                if bits == 0 {
                    return None;
                }
                let word = i * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                Some(word)
            })
        })
    }
    /// Intersect with a single bitset using the same kernel as letter filtering.
    #[inline]
    pub fn intersect(&mut self, filter: &[u64]) -> u32 {
        debug_assert_eq!(filter.len(), self.bits.blocks().len());
        self.intersect_sources::<1>(&[filter; 6])
    }
    /// Remove a letter's words for symmetry pruning. This infrequent operation
    /// preserves the original dense loop, with index maintenance encapsulated.
    pub fn remove(&mut self, words: &BitSet) {
        debug_assert_eq!(words.blocks().len(), self.bits.blocks().len());
        let blocks = self.bits.blocks_mut();
        let mut removed = 0;
        for (d, &w) in blocks.iter_mut().zip(words.blocks()) {
            removed += (*d & w).count_ones();
            *d &= !w;
        }
        if removed != 0 {
            self.active.retain(|&i| blocks[i] != 0);
        }
        self.count -= removed;
    }
    #[inline]
    pub(crate) fn intersect_union(&mut self, sources: &[&[u64]; 6], count: usize) -> u32 {
        match count {
            0 => {
                let removed = self.count;
                self.bits.blocks_mut().fill(0);
                self.active.clear();
                self.count = 0;
                removed
            }
            1 => self.intersect_sources::<1>(sources),
            2 => self.intersect_sources::<2>(sources),
            3 => self.intersect_sources::<3>(sources),
            4 => self.intersect_sources::<4>(sources),
            5 => self.intersect_sources::<5>(sources),
            6 => self.intersect_sources::<6>(sources),
            _ => unreachable!(),
        }
    }
    /// Keep source count static so dense loops can be unrolled/vectorized.
    /// Use dense traversal at >=25% nonempty blocks, indexed traversal below.
    #[inline]
    fn intersect_sources<const N: usize>(&mut self, sources: &[&[u64]; 6]) -> u32 {
        let blocks = self.bits.blocks_mut();
        let mut removed = 0;
        if self.active.len() >= blocks.len().div_ceil(4) {
            for (i, word) in blocks.iter_mut().enumerate() {
                let filter = union_at::<N>(sources, i);
                removed += (*word & !filter).count_ones();
                *word &= filter;
            }
            if removed != 0 {
                self.active.retain(|&i| blocks[i] != 0);
            }
        } else {
            self.active.retain(|&i| {
                let filter = union_at::<N>(sources, i);
                removed += (blocks[i] & !filter).count_ones();
                blocks[i] &= filter;
                blocks[i] != 0
            });
        }
        self.count -= removed;
        removed
    }
}

#[inline]
fn union_at<const N: usize>(sources: &[&[u64]; 6], i: usize) -> u64 {
    let mut filter = 0;
    for source in sources.iter().take(N) {
        filter |= source[i];
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(d: &CandidateSet) {
        let expected: Vec<_> = d
            .bits
            .blocks()
            .iter()
            .enumerate()
            .filter_map(|(i, &b)| (b != 0).then_some(i))
            .collect();
        assert_eq!(d.active, expected);
        assert_eq!(d.count, d.bits.count_ones());
        assert_eq!(
            d.iter().collect::<Vec<_>>(),
            d.bits.iter_ones().collect::<Vec<_>>()
        );
    }
    #[test]
    fn dense_sparse_and_tail_blocks_match_reference() {
        for len in [0, 1, 63, 64, 65, 255, 256, 257, 4097] {
            // Include the exact 25% boundary and sparse/dense cases on either side.
            for stride in [1, 2, 4, 5, 64, 1000] {
                let mut initial = BitSet::new(len);
                for i in (0..len).step_by(stride) {
                    initial.set(i);
                }
                let mut d = CandidateSet::new(initial.clone());
                let mut reference = initial;
                let root = d.clone();
                let mut random = 123u64;
                for _ in 0..8 {
                    let mut filter = BitSet::new_all_set(len);
                    for b in filter.blocks_mut() {
                        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                        *b &= random;
                    }
                    let before = reference.count_ones();
                    reference.and_with(&filter);
                    assert_eq!(
                        d.intersect(filter.blocks()),
                        before - reference.count_ones()
                    );
                    assert_eq!(d.bits(), &reference);
                    check(&d);
                }
                d = root;
                check(&d);
                let remove = d.bits.clone();
                d.remove(&remove);
                check(&d);
                assert_eq!(d.count(), 0);
            }
        }
    }
    #[test]
    fn grouped_filters_match_independent_letter_unions() {
        let text: String = (0..4097)
            .map(|i| {
                format!(
                    "{}{}{};1\n",
                    (b'A' + (i / 676) as u8) as char,
                    (b'A' + (i / 26 % 26) as u8) as char,
                    (b'A' + (i % 26) as u8) as char
                )
            })
            .collect();
        let dict = crate::dict::Dictionary::parse(&text).unwrap();
        let b = dict.bucket(3).unwrap();
        let mut random = 123u32;
        for stride in [1, 2, 64, 256, 1000] {
            for case in 0..200 {
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                let mask = match case {
                    0 => 0,
                    1 => (1 << 26) - 1,
                    _ => random & ((1 << 26) - 1),
                };
                let mut initial = BitSet::new(b.words.len());
                for i in (0..b.words.len()).step_by(stride) {
                    initial.set(i);
                }
                let mut d = CandidateSet::new(initial.clone());
                let before = d.count();
                let expected: Vec<u64> = initial
                    .blocks()
                    .iter()
                    .enumerate()
                    .map(|(i, word)| {
                        let filter = (0..26)
                            .filter(|l| mask & (1 << l) != 0)
                            .fold(0, |acc, l| acc | b.letter_bits[1][l].blocks()[i]);
                        word & filter
                    })
                    .collect();
                let removed = b.intersect_letter_union(1, mask, &mut d);
                check(&d);
                assert_eq!(d.bits().blocks(), expected);
                assert_eq!(removed, before - d.count());
                assert_eq!(b.intersect_letter_union(1, mask, &mut d), 0);
            }
        }
    }
}
