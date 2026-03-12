//! Solver state: per-slot candidate domains with trail-based backtracking.

use orca_core::bitset::BitSet;
use orca_core::dict::Dictionary;
use orca_core::grid::Grid;

use crate::stats::SolverStats;

/// Domain for a single slot: the set of candidate word_ids that are still valid.
#[derive(Debug, Clone)]
pub struct SlotDomain {
    /// Bitset of candidate word_ids for this slot's length bucket.
    pub candidates: BitSet,
    /// Cached count of candidates (avoids recomputing popcount).
    pub count: u32,
}

impl SlotDomain {
    pub fn new(candidates: BitSet) -> Self {
        let count = candidates.count_ones();
        SlotDomain { candidates, count }
    }

    /// Intersect the domain with a candidate set (recomputes count from scratch).
    pub fn intersect(&mut self, other: &BitSet) {
        self.candidates.and_with(other);
        self.count = self.candidates.count_ones();
    }

    /// Intersect domain with a filter bitset, tracking removed bits for an
    /// incremental count update. Faster than `intersect` (which recomputes
    /// popcount from scratch) when the domain is large.
    pub fn intersect_incremental(&mut self, filter: &BitSet) {
        let domain_blocks = self.candidates.blocks_mut();
        let filter_blocks = filter.blocks();
        let mut count_removed: u32 = 0;
        for (d, &f) in domain_blocks.iter_mut().zip(filter_blocks.iter()) {
            count_removed += (*d & !f).count_ones();
            *d &= f;
        }
        self.count -= count_removed;
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
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
    /// Cache of possible_letters per directed arc (crossing_idx * 2 + side).
    pub(crate) prop_letters_cache: Vec<u32>,
    /// Scratch buffer for filter construction.
    pub(crate) prop_filter: Vec<u64>,
}

impl SolverState {
    pub fn new(domains: Vec<SlotDomain>) -> Self {
        SolverState {
            domains,
            trail: Vec::new(),
            trail_levels: Vec::new(),
            stats: SolverStats::new(),
            prop_queue_bits: Vec::new(),
            prop_letters_cache: Vec::new(),
            prop_filter: Vec::new(),
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
    use orca_core::grid::Grid;

    #[test]
    fn test_push_pop_roundtrip() {
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let dict = test_dict();
        let domains = init_domains(&grid, &dict);
        let mut state = SolverState::new(domains);

        let original_count = state.domains[0].count;
        assert!(original_count > 0);

        // Push level, modify domain, pop level — should restore
        state.push_level();
        state.save_domain(0);
        state.domains[0].count = 1;
        state.pop_level();

        assert_eq!(state.domains[0].count, original_count);
    }

    #[test]
    fn test_save_domain_dedup() {
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let dict = test_dict();
        let domains = init_domains(&grid, &dict);
        let mut state = SolverState::new(domains);

        let original_count = state.domains[0].count;

        state.push_level();
        state.save_domain(0);
        // Modify domain
        state.domains[0].count = 5;
        // Save again — should NOT overwrite the first save
        state.save_domain(0);
        // Modify again
        state.domains[0].count = 1;

        state.pop_level();
        // Should restore to original, not to 5
        assert_eq!(state.domains[0].count, original_count);
    }

    #[test]
    fn test_nested_levels() {
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let dict = test_dict();
        let domains = init_domains(&grid, &dict);
        let mut state = SolverState::new(domains);

        let count_0 = state.domains[0].count;
        let count_1 = state.domains[1].count;

        // Level 1: modify slot 0
        state.push_level();
        state.save_domain(0);
        state.domains[0].count = 5;

        // Level 2: modify slot 1
        state.push_level();
        state.save_domain(1);
        state.domains[1].count = 3;

        // Pop level 2: slot 1 restored, slot 0 still modified
        state.pop_level();
        assert_eq!(state.domains[0].count, 5);
        assert_eq!(state.domains[1].count, count_1);

        // Pop level 1: slot 0 restored
        state.pop_level();
        assert_eq!(state.domains[0].count, count_0);
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
        d2.intersect_incremental(filter);

        assert_eq!(d1.count, d2.count);
        assert_eq!(d1.candidates.count_ones(), d2.candidates.count_ones());
    }
}
