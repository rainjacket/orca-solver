//! Solver state: per-slot candidate domains with trail-based backtracking.

use orca_core::bitset::BitSet;
use orca_core::dict::Dictionary;
use orca_core::grid::Grid;

use crate::stats::SolverStats;

pub(crate) use orca_core::grid::MAX_SLOT_LEN;

/// Domain for a single slot: the set of candidate word_ids that are still valid.
#[derive(Debug, Clone)]
pub struct SlotDomain {
    values: orca_core::domain::CandidateSet,
    /// Conservative viable-letter bounds, restored with the candidate snapshot.
    /// Inline storage avoids an extra allocation per saved domain. Only crossing
    /// positions are read; 32 matches propagation's maximum supported slot length.
    pub(crate) letter_masks: [u32; MAX_SLOT_LEN],
}

impl SlotDomain {
    pub fn new(candidates: BitSet) -> Self {
        Self {
            values: orca_core::domain::CandidateSet::new(candidates),
            letter_masks: [(1 << 26) - 1; MAX_SLOT_LEN],
        }
    }
    pub fn candidates(&self) -> &BitSet {
        self.values.bits()
    }
    pub fn count(&self) -> u32 {
        self.values.count()
    }
    /// Enumerate only nonempty blocks, independent of the filtering threshold.
    pub fn iter_candidates(&self) -> impl Iterator<Item = usize> + '_ {
        self.values.iter()
    }
    pub fn intersect(&mut self, other: &BitSet) {
        self.values.intersect(other.blocks());
    }
    /// Snapshotting and propagation scheduling remain the caller's responsibility.
    pub(crate) fn restrict_letters(
        &mut self,
        bucket: &orca_core::dict::LengthBucket,
        pos: usize,
        allowed: u32,
    ) -> u32 {
        bucket.intersect_letter_union(pos, allowed, &mut self.values)
    }
    pub(crate) fn remove(&mut self, words: &BitSet) {
        self.values.remove(words);
    }
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }
}

/// Initialize domains for all slots from the dictionary.
pub fn init_domains(grid: &Grid, dict: &Dictionary) -> Vec<SlotDomain> {
    grid.slots
        .iter()
        .map(|slot| {
            if let Some(bucket) = dict.bucket(slot.len) {
                SlotDomain::new(bucket.all.clone())
            } else {
                SlotDomain::new(BitSet::new(0))
            }
        })
        .collect()
}

/// The complete state of the solver during search.
#[derive(Clone)]
pub struct SolverState {
    /// Per-slot candidate domains.
    pub domains: Vec<SlotDomain>,
    /// Flat trail: saved (slot_id, domain) pairs across all levels.
    pub(crate) trail: Vec<(usize, SlotDomain)>,
    /// Start index in `trail` for each decision level.
    pub(crate) trail_levels: Vec<usize>,
    /// Performance counters.
    pub stats: SolverStats,
    // Reusable scratch buffers for propagation (avoid per-node allocation).
    /// Bitset tracking which slots are in the propagation queue.
    pub(crate) prop_queue_bits: Vec<u64>,
    /// Last applied letter mask per directed arc (crossing_idx * 2 + side).
    /// Reset each propagation call: restored neighbors may need filtering again.
    pub(crate) last_applied_letters: Vec<u32>,
    /// Positive witnesses are revalidated against the current domain, not trailed.
    pub(crate) prop_witnesses: Vec<crate::witnesses::Witnesses>,
}

impl SolverState {
    pub fn new(domains: Vec<SlotDomain>) -> Self {
        SolverState {
            domains,
            trail: Vec::new(),
            trail_levels: Vec::new(),
            stats: SolverStats::new(),
            prop_queue_bits: Vec::new(),
            last_applied_letters: Vec::new(),
            prop_witnesses: Vec::new(),
        }
    }

    /// Save the current domain of a slot (for later restoration on backtrack).
    /// Call this BEFORE modifying the domain during propagation.
    pub fn save_domain(&mut self, slot_id: usize) {
        if !self.trail_levels.is_empty() {
            let level_start = *self.trail_levels.last().unwrap();
            if !self.trail[level_start..]
                .iter()
                .any(|(id, _)| *id == slot_id)
            {
                self.trail.push((slot_id, self.domains[slot_id].clone()));
            }
        }
    }

    /// Push a new decision level onto the trail.
    pub fn push_level(&mut self) {
        self.trail_levels.push(self.trail.len());
    }

    /// Undo the most recent decision level: restore saved domains.
    pub fn pop_level(&mut self) {
        if let Some(level_start) = self.trail_levels.pop() {
            while self.trail.len() > level_start {
                let (slot_id, saved) = self.trail.pop().unwrap();
                self.domains[slot_id] = saved;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_dict;
    fn trim(domain: &mut SlotDomain, count: usize) {
        let mut filter = domain.candidates().clone();
        let ids: Vec<_> = filter.iter_ones().skip(count).collect();
        for i in ids {
            filter.blocks_mut()[i / 64] &= !(1 << (i % 64));
        }
        domain.intersect(&filter);
    }
    use orca_core::grid::Grid;

    #[test]
    fn test_push_pop_roundtrip() {
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let dict = test_dict();
        let domains = init_domains(&grid, &dict);
        let mut state = SolverState::new(domains);

        let original_count = state.domains[0].count();
        assert!(original_count > 0);

        // Push level, modify domain, pop level — should restore
        state.push_level();
        state.save_domain(0);
        trim(&mut state.domains[0], 1);
        state.pop_level();

        assert_eq!(state.domains[0].count(), original_count);
    }

    #[test]
    fn test_save_domain_dedup() {
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let dict = test_dict();
        let domains = init_domains(&grid, &dict);
        let mut state = SolverState::new(domains);

        let original_count = state.domains[0].count();

        state.push_level();
        state.save_domain(0);
        // Modify domain
        trim(&mut state.domains[0], 5);
        // Save again — should NOT overwrite the first save
        state.save_domain(0);
        // Modify again
        trim(&mut state.domains[0], 1);

        state.pop_level();
        // Should restore to original, not to 5
        assert_eq!(state.domains[0].count(), original_count);
    }

    #[test]
    fn test_nested_levels() {
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let dict = test_dict();
        let domains = init_domains(&grid, &dict);
        let mut state = SolverState::new(domains);

        let count_0 = state.domains[0].count();
        let count_1 = state.domains[1].count();

        // Level 1: modify slot 0
        state.push_level();
        state.save_domain(0);
        trim(&mut state.domains[0], 5);

        // Level 2: modify slot 1
        state.push_level();
        state.save_domain(1);
        trim(&mut state.domains[1], 3);

        // Pop level 2: slot 1 restored, slot 0 still modified
        state.pop_level();
        assert_eq!(state.domains[0].count(), 5);
        assert_eq!(state.domains[1].count(), count_1);

        // Pop level 1: slot 0 restored
        state.pop_level();
        assert_eq!(state.domains[0].count(), count_0);
    }

    #[test]
    fn test_restrict_vs_intersect() {
        let dict = test_dict();
        let bucket = dict.bucket(3).unwrap();

        // Both should produce the same result
        let mut d1 = SlotDomain::new(bucket.all.clone());
        let mut d2 = SlotDomain::new(bucket.all.clone());
        let filter = &bucket.letter_bits[0][2]; // words with C at position 0

        d1.intersect(filter);
        let removed = d2.restrict_letters(bucket, 0, 1 << 2);
        assert_eq!(removed, bucket.all.count_ones() - filter.count_ones());
        assert_eq!(d2.restrict_letters(bucket, 0, 1 << 2), 0);

        assert_eq!(d1.count(), d2.count());
        assert_eq!(d1.candidates().count_ones(), d2.candidates().count_ones());
    }
    #[test]
    fn nested_sibling_wipeout_restores_candidate_iteration() {
        let root = BitSet::new_all_set(4097);
        let mut state = SolverState::new(vec![SlotDomain::new(root.clone())]);
        for stride in [2, 64, 1000, 4098] {
            state.push_level();
            state.save_domain(0);
            let mut filter = BitSet::new(4097);
            for i in (0..4097).step_by(stride) {
                filter.set(i);
            }
            state.domains[0].intersect(&filter);
            let parent = state.domains[0].candidates().clone();
            state.push_level();
            state.save_domain(0);
            state.domains[0].intersect(&BitSet::new(4097));
            assert_eq!(state.clone().domains[0].iter_candidates().count(), 0);
            state.pop_level();
            assert_eq!(
                state.domains[0].iter_candidates().collect::<Vec<_>>(),
                parent.iter_ones().collect::<Vec<_>>()
            );
            state.save_domain(0);
            state.pop_level();
            assert_eq!(state.domains[0].candidates(), &root);
            assert_eq!(state.domains[0].iter_candidates().count(), 4097);
            assert_eq!(state.domains[0].count(), 4097);
        }
    }
}
