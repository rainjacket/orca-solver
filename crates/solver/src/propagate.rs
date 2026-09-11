//! AC-3 constraint propagation with priority queue (smallest domain first).
//! The hot loop filters neighbor domains via precomputed `letter_bits` bitsets,
//! using scratch buffers to avoid per-node allocation.

use crate::constraint::ConstraintGraph;
use orca_core::dict::Dictionary;
use orca_core::grid::Grid;

use crate::search::CellSymInfo;
use crate::state::{SolverState, MAX_SLOT_LEN};

/// Bitmask with bits 0..25 set — represents "all 26 letters possible".
const ALL_LETTERS_MASK: u32 = (1 << 26) - 1;

/// Cutoff for standalone discovery and exact heuristic counts (without witnesses).
const SMALL_DOMAIN_THRESHOLD: u32 = 2000;
/// Propagation discovery uses cached witnesses above this count. Standalone
/// discovery and exact heuristic counts keep their independent 2,000 cutoff.
const PROPAGATION_ITERATION_THRESHOLD: u32 = 512;

/// Propagate arc consistency (AC-3) from one changed slot.
///
/// When a slot's domain shrinks, we re-check all its crossing neighbors.
/// For each crossing, we compute which letters are possible at the crossing
/// position given the slot's current domain, then filter the neighbor's
/// domain to only words compatible with those letters.
///
/// Returns true if all domains are consistent, false if a domain was wiped out.
pub fn propagate(
    state: &mut SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
    changed_slot: usize,
) -> bool {
    propagate_from_slots(state, graph, dict, grid, &[changed_slot])
}

/// Propagate arc consistency (AC-3) from multiple changed slots.
///
/// Seeds the propagation queue with all given slots, then runs to fixpoint.
/// Processes smallest-domain slots first (priority queue) to detect wipeouts early.
pub fn propagate_from_slots(
    state: &mut SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
    changed_slots: &[usize],
) -> bool {
    let num_slots = grid.slots.len();
    let num_queue_blocks = num_slots.div_ceil(64);
    let num_crossings = graph.crossings.len();
    let cache_size = num_crossings * 2;
    state
        .prop_witnesses
        .resize_with(cache_size, Default::default);

    // Take scratch buffers out of state to avoid borrow conflicts,
    // then put them back at the end. The buffers grow once and are reused.
    let mut in_queue = std::mem::take(&mut state.prop_queue_bits);
    let mut letters_cache = std::mem::take(&mut state.prop_letters_cache);

    in_queue.resize(num_queue_blocks, 0);
    in_queue[..num_queue_blocks].fill(0);
    for &slot in changed_slots {
        in_queue[slot / 64] |= 1u64 << (slot % 64);
    }
    letters_cache.resize(cache_size, ALL_LETTERS_MASK);
    letters_cache[..cache_size].fill(ALL_LETTERS_MASK);

    let result = propagate_inner(state, graph, dict, grid, &mut in_queue, &mut letters_cache);

    state.prop_queue_bits = in_queue;
    state.prop_letters_cache = letters_cache;

    result
}

/// Inner propagation loop, separated so scratch buffers can be returned
/// to SolverState regardless of whether propagation succeeds or fails.
fn propagate_inner(
    state: &mut SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
    in_queue: &mut [u64],
    letters_cache: &mut [u32],
) -> bool {
    loop {
        // Pop the slot with the smallest domain from the queue.
        // Fused emptiness check + min-domain scan in one pass.
        let mut best_slot = usize::MAX;
        let mut best_count = u32::MAX;
        for (block_idx, mask) in in_queue.iter().enumerate() {
            let mut m = *mask;
            while m != 0 {
                let bit = block_idx * 64 + m.trailing_zeros() as usize;
                let count = state.domains[bit].count;
                if count < best_count {
                    best_count = count;
                    best_slot = bit;
                }
                m &= m - 1; // clear lowest bit
            }
        }
        if best_slot == usize::MAX {
            break;
        }
        let slot_id = best_slot;
        in_queue[slot_id / 64] &= !(1u64 << (slot_id % 64));

        let slot = &grid.slots[slot_id];
        let bucket = match dict.bucket(slot.len) {
            Some(b) => b,
            None => return false,
        };

        // Batch-compute possible_letters for all crossing positions of this slot.
        // For small domains, this iterates the domain once instead of once per crossing.
        let mut possible_at_pos = state.domains[slot_id].letter_masks;
        let mut positions_needed: u32 = 0;
        for &(crossing_idx, is_slot_a) in &graph.neighbors[slot_id] {
            let (_, pos_in_this, _) = graph.crossing_info(crossing_idx, is_slot_a);
            positions_needed |= 1u32 << pos_in_this;
            // Within this call, domains only shrink: a previously unsupported
            // letter cannot return. The arc masks reset to all letters per call.
            possible_at_pos[pos_in_this] &= letters_cache[crossing_idx * 2 + is_slot_a as usize];
        }

        let use_witnesses = state.domains[slot_id].count > PROPAGATION_ITERATION_THRESHOLD;
        if use_witnesses {
            for &(crossing_idx, is_slot_a) in &graph.neighbors[slot_id] {
                let (_, pos, _) = graph.crossing_info(crossing_idx, is_slot_a);
                possible_at_pos[pos] = state.prop_witnesses[crossing_idx * 2 + is_slot_a as usize]
                    .letters(
                        &state.domains[slot_id].candidates,
                        bucket,
                        pos,
                        possible_at_pos[pos],
                    );
            }
        } else if positions_needed != 0 {
            batch_compute_possible_letters(
                &state.domains[slot_id].candidates,
                bucket,
                positions_needed,
                &mut possible_at_pos,
            );
        }

        // Refinement alone does not need a new snapshot: if candidates have
        // changed at this decision level, their pre-change domain (including
        // masks) is already saved. Otherwise this refinement is also valid
        // for the unchanged parent domain. Restoring an older, broader mask
        // is safe. Candidate mutations MUST still save the domain first.
        state.domains[slot_id].letter_masks = possible_at_pos;

        // For each crossing neighbor of this slot
        for &(crossing_idx, is_slot_a) in &graph.neighbors[slot_id] {
            let (neighbor_id, pos_in_this, pos_in_neighbor) =
                graph.crossing_info(crossing_idx, is_slot_a);

            let neighbor_slot = &grid.slots[neighbor_id];
            let neighbor_bucket = match dict.bucket(neighbor_slot.len) {
                Some(b) => b,
                None => return false,
            };

            let possible_letters = possible_at_pos[pos_in_this];

            // Fast path: all 26 letters possible → filter is all-ones → intersection is no-op
            if possible_letters == ALL_LETTERS_MASK {
                continue;
            }

            // Cache check: if possible_letters is unchanged since last visit to this arc,
            // the filter is identical and the neighbor's domain (which only shrinks) is
            // still a subset — skip the expensive filter build + intersection.
            let cache_idx = crossing_idx * 2 + (is_slot_a as usize);
            if possible_letters == letters_cache[cache_idx] {
                continue;
            }
            letters_cache[cache_idx] = possible_letters;

            // Snapshot before the fused union/intersection writes the domain.
            state.save_domain(neighbor_id);
            let domain = &mut state.domains[neighbor_id];
            let count_removed = neighbor_bucket.intersect_letter_union(
                pos_in_neighbor,
                possible_letters,
                domain.candidates.blocks_mut(),
            );
            domain.count -= count_removed;
            if count_removed == 0 {
                continue;
            }
            state.stats.propagations += 1;
            let new_count = state.domains[neighbor_id].count;

            if new_count == 0 {
                state.stats.wipeouts += 1;
                return false;
            }

            // Enqueue neighbor for further propagation
            in_queue[neighbor_id / 64] |= 1u64 << (neighbor_id % 64);
        }
    }

    true
}

/// Discover letters at all requested crossing positions in one pass over words.
/// Used for propagation domains of at most PROPAGATION_ITERATION_THRESHOLD.
/// Requested positions are overwritten; other positions retain their bounds.
#[inline]
fn batch_compute_possible_letters(
    domain: &orca_core::bitset::BitSet,
    bucket: &orca_core::dict::LengthBucket,
    positions_needed: u32,
    out: &mut [u32; MAX_SLOT_LEN],
) {
    // Initialize needed positions to 0 for OR accumulation
    let mut mask = positions_needed;
    while mask != 0 {
        let pos = mask.trailing_zeros() as usize;
        out[pos] = 0;
        mask &= mask - 1;
    }

    // Single pass: iterate domain once, update all positions per word
    let word_len = bucket.word_len();
    let word_bytes = &bucket.word_bytes;
    let mut unsaturated = positions_needed;
    for word_id in domain.iter_ones() {
        let base = word_id * word_len;
        let mut mask = unsaturated;
        while mask != 0 {
            let pos = mask.trailing_zeros() as usize;
            out[pos] |= 1u32 << word_bytes[base + pos];
            if out[pos] == ALL_LETTERS_MASK {
                unsaturated &= !(1u32 << pos);
            }
            mask &= mask - 1;
        }
        if unsaturated == 0 {
            break;
        }
    }
}

/// Compute per-letter counts at a single position in a slot's domain.
///
/// Returns a `[u32; 26]` array where entry `i` is the number of candidate words
/// that have letter `i` (A=0) at position `pos`. For small domains
/// (≤SMALL_DOMAIN_THRESHOLD), iterates candidates directly. For large domains,
/// uses bitset intersection (`count_intersection`) which also returns exact counts.
#[inline]
pub fn compute_letter_counts_at(
    domain: &orca_core::bitset::BitSet,
    count: u32,
    bucket: &orca_core::dict::LengthBucket,
    pos: usize,
) -> [u32; 26] {
    let mut counts = [0u32; 26];
    if count <= SMALL_DOMAIN_THRESHOLD {
        let word_len = bucket.word_len();
        let word_bytes = &bucket.word_bytes;
        for word_id in domain.iter_ones() {
            let letter = word_bytes[word_id * word_len + pos] as usize;
            counts[letter] += 1;
        }
    } else {
        for letter in 0..26usize {
            counts[letter] = domain.count_intersection(&bucket.letter_bits[pos][letter]);
        }
    }
    counts
}

/// Compute possible letters at a single position in a slot's domain.
///
/// Returns a u32 bitmask where bit `i` is set if letter `i` (A=0) appears at
/// `pos` in at least one candidate word in the domain.
///
/// Standalone discovery has no persistent cache: domains up to 2,000 iterate
/// candidates directly; larger domains use per-letter bitset intersection.
#[inline]
pub fn compute_possible_letters_at(
    domain: &orca_core::bitset::BitSet,
    count: u32,
    bucket: &orca_core::dict::LengthBucket,
    pos: usize,
) -> u32 {
    if count <= SMALL_DOMAIN_THRESHOLD {
        let word_len = bucket.word_len();
        let word_bytes = &bucket.word_bytes;
        let mut letters = 0u32;
        for word_id in domain.iter_ones() {
            letters |= 1u32 << word_bytes[word_id * word_len + pos];
            if letters == ALL_LETTERS_MASK {
                break;
            }
        }
        letters
    } else {
        let mut letters = 0u32;
        for letter in 0..26u8 {
            if domain.has_intersection(&bucket.letter_bits[pos][letter as usize]) {
                letters |= 1u32 << letter;
            }
        }
        letters
    }
}

/// Initial propagation: apply pre-filled letter constraints to all slots.
/// Returns false if any slot has no valid candidates after filtering.
pub fn initial_propagate(
    state: &mut SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
) -> bool {
    // Filter each slot's domain based on its pre-filled pattern
    let mut changed_slots = Vec::new();
    for (i, slot) in grid.slots.iter().enumerate() {
        if !slot.constrained {
            continue;
        }
        let bucket = match dict.bucket(slot.len) {
            Some(b) => b,
            None => return false,
        };

        let candidates = match bucket.candidates(&slot.pattern) {
            Some(c) => c,
            None => return false,
        };

        state.domains[i].intersect(&candidates);
        if state.domains[i].is_empty() {
            return false;
        }

        // Seed propagation from every constrained slot, not just pattern-
        // constrained ones: a domain that is narrow from initialization alone
        // (e.g. a length bucket holding a single word) constrains its crossings
        // just as much, and nothing later will ever enqueue its arcs.
        changed_slots.push(i);
    }

    // Establish arc consistency across all constrained slots
    propagate_from_slots(state, graph, dict, grid, &changed_slots)
}

/// Remove candidates with specific letters at a position from a slot's domain.
/// Saves the domain before modifying. Returns false if domain becomes empty.
fn remove_letters_from_domain(
    state: &mut SolverState,
    slot_id: usize,
    pos: usize,
    bucket: &orca_core::dict::LengthBucket,
    removed: u32,
) -> bool {
    state.save_domain(slot_id);
    let mut letters = removed;
    while letters != 0 {
        let letter = letters.trailing_zeros() as usize;
        letters &= letters - 1;
        let letter_bits = &bucket.letter_bits[pos][letter];
        let domain_blocks = state.domains[slot_id].candidates.blocks_mut();
        let letter_blocks = letter_bits.blocks();
        let mut count_removed: u32 = 0;
        for (d, &l) in domain_blocks.iter_mut().zip(letter_blocks.iter()) {
            count_removed += (*d & l).count_ones();
            *d &= !l;
        }
        state.domains[slot_id].count -= count_removed;
    }
    state.domains[slot_id].count > 0
}

/// Enforce cell-level symmetry: letter(cell_a) ≤ letter(cell_b).
///
/// Fixpoint loop: prune letters from A that exceed max(possible_B),
/// prune letters from B below min(possible_A), then propagate and repeat.
pub fn enforce_cell_symmetry(
    state: &mut SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
    cell_a: &CellSymInfo,
    cell_b: &CellSymInfo,
) -> bool {
    loop {
        let bucket_a = match dict.bucket(grid.slots[cell_a.slot_id].len) {
            Some(b) => b,
            None => return false,
        };
        let bucket_b = match dict.bucket(grid.slots[cell_b.slot_id].len) {
            Some(b) => b,
            None => return false,
        };

        let possible_a = compute_possible_letters_at(
            &state.domains[cell_a.slot_id].candidates,
            state.domains[cell_a.slot_id].count,
            bucket_a,
            cell_a.pos_in_slot,
        );
        let possible_b = compute_possible_letters_at(
            &state.domains[cell_b.slot_id].candidates,
            state.domains[cell_b.slot_id].count,
            bucket_b,
            cell_b.pos_in_slot,
        );

        if possible_a == 0 || possible_b == 0 {
            return false;
        }

        // max_b = index of highest viable letter in possible_b (0=A, 25=Z)
        let max_b = 31 - possible_b.leading_zeros();
        // min_a = index of lowest viable letter in possible_a (0=A, 25=Z)
        let min_a = possible_a.trailing_zeros();

        // allowed_a: letters ≤ max_b
        let allowed_a = possible_a & ((1u32 << (max_b + 1)) - 1);
        // allowed_b: letters ≥ min_a
        let allowed_b = if min_a == 0 {
            possible_b
        } else {
            possible_b & !((1u32 << min_a) - 1)
        };

        let removed_a = possible_a & !allowed_a;
        let removed_b = possible_b & !allowed_b;

        if removed_a == 0 && removed_b == 0 {
            return true;
        }

        let mut changed_slots = Vec::new();

        if removed_a != 0 {
            if !remove_letters_from_domain(
                state,
                cell_a.slot_id,
                cell_a.pos_in_slot,
                bucket_a,
                removed_a,
            ) {
                return false;
            }
            changed_slots.push(cell_a.slot_id);
        }
        if removed_b != 0 {
            if !remove_letters_from_domain(
                state,
                cell_b.slot_id,
                cell_b.pos_in_slot,
                bucket_b,
                removed_b,
            ) {
                return false;
            }
            if !changed_slots.contains(&cell_b.slot_id) {
                changed_slots.push(cell_b.slot_id);
            }
        }

        // Propagate from changed slots
        let prop_result = propagate_from_slots(state, graph, dict, grid, &changed_slots);
        if !prop_result {
            return false;
        }
        // Loop back to re-check: propagation may have further restricted domains
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraint::ConstraintGraph;
    use orca_core::grid::Grid;

    use crate::test_utils::{init_state, test_dict};

    #[test]
    fn test_initial_propagate_no_prefilled() {
        let grid = Grid::parse("1 3\n...\n").unwrap();
        let dict = test_dict();
        let graph = ConstraintGraph::from_grid(&grid);
        let mut state = init_state(&grid, &dict);

        let result = initial_propagate(&mut state, &graph, &dict, &grid);
        assert!(result);
    }

    #[test]
    fn test_initial_propagate_with_prefilled() {
        let grid = Grid::parse("1 3\nC..\n").unwrap();
        let dict = test_dict();
        let graph = ConstraintGraph::from_grid(&grid);
        let mut state = init_state(&grid, &dict);

        let result = initial_propagate(&mut state, &graph, &dict, &grid);
        assert!(result);
        // Only words starting with C should remain: CAR, CAT, COT
        assert_eq!(state.domains[0].count, 3);
    }

    #[test]
    fn test_propagate_assignment() {
        let grid = Grid::parse("1 3\n...\n").unwrap();
        let dict = test_dict();
        let graph = ConstraintGraph::from_grid(&grid);
        let mut state = init_state(&grid, &dict);

        let slot0_bucket = dict.bucket(3).unwrap();
        let cat_id = slot0_bucket.words.iter().position(|w| w == "CAT").unwrap();

        state.push_level();
        state.save_domain(0);
        // Restrict domain to just the one word
        let mut single = orca_core::bitset::BitSet::new(slot0_bucket.words.len());
        single.set(cat_id);
        state.domains[0].candidates.and_with(&single);
        state.domains[0].count = 1;
        let result = propagate(&mut state, &graph, &dict, &grid, 0);
        assert!(result);
        assert_eq!(state.domains[0].count, 1);
    }

    #[test]
    fn test_propagate_wipeout() {
        // Prefilling Z at position 0 should wipe out — no words start with Z in test_dict
        let grid = Grid::parse("1 3\nZ..\n").unwrap();
        let dict = test_dict();
        let graph = ConstraintGraph::from_grid(&grid);
        let mut state = init_state(&grid, &dict);

        let result = initial_propagate(&mut state, &graph, &dict, &grid);
        assert!(!result, "Should detect wipeout: no words start with Z");
    }

    #[test]
    fn test_propagate_from_multiple_slots() {
        // Test propagate_from_slots with multiple seed slots.
        // Use a 1x3 grid (single slot), restrict domain, then propagate from it.
        let grid = Grid::parse("1 3\n...\n").unwrap();
        let dict = test_dict();
        let graph = ConstraintGraph::from_grid(&grid);
        let mut state = init_state(&grid, &dict);

        let bucket = dict.bucket(3).unwrap();
        // Restrict to just CAT
        let cat_id = bucket.words.iter().position(|w| w == "CAT").unwrap();
        let mut single = orca_core::bitset::BitSet::new(bucket.words.len());
        single.set(cat_id);
        state.domains[0].intersect(&single);

        // propagate_from_slots with slot 0 seeded
        let result = propagate_from_slots(&mut state, &graph, &dict, &grid, &[0]);
        assert!(result);
        assert_eq!(state.domains[0].count, 1);
    }

    #[test]
    fn test_compute_possible_letters() {
        let dict = test_dict();
        let bucket = dict.bucket(3).unwrap();
        // Full domain: all 16 words
        let domain = bucket.all.clone();
        let count = domain.count_ones();

        // Position 0: first letters of all words (A, C, D, O, R, T)
        let letters = compute_possible_letters_at(&domain, count, bucket, 0);
        assert!(letters & (1 << 0) != 0, "A should be possible at pos 0"); // A (ACE, AGE, ARC, ATE)
        assert!(letters & (1 << 2) != 0, "C should be possible at pos 0"); // C (CAT, CAR, COT)
        assert!(letters & (1 << 3) != 0, "D should be possible at pos 0"); // D (DOG, DOT)
        assert!(
            letters & (1 << 25) == 0,
            "Z should not be possible at pos 0"
        );
    }

    #[test]
    fn test_compute_letter_counts() {
        let dict = test_dict();
        let bucket = dict.bucket(3).unwrap();
        let domain = bucket.all.clone();
        let count = domain.count_ones();

        let counts = compute_letter_counts_at(&domain, count, bucket, 0);
        // Sum of all counts at position 0 should equal total words
        let total: u32 = counts.iter().sum();
        assert_eq!(total, 16, "Letter counts at pos 0 should sum to word count");
        // C starts 3 words: CAT, CAR, COT
        assert_eq!(counts[2], 3, "C should start 3 words");
    }

    #[test]
    fn test_enforce_cell_symmetry() {
        // 3x3 grid: cell (0,0) <= cell (0,2) should prune some combinations
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let dict = test_dict();
        let graph = ConstraintGraph::from_grid(&grid);
        let mut state = init_state(&grid, &dict);
        initial_propagate(&mut state, &graph, &dict, &grid);

        let count_before: u32 = state.domains.iter().map(|d| d.count).sum();

        let cell_a = CellSymInfo {
            slot_id: 0,
            pos_in_slot: 0,
        };
        let cell_b = CellSymInfo {
            slot_id: 0,
            pos_in_slot: 2,
        };

        state.push_level();
        let result = enforce_cell_symmetry(&mut state, &graph, &dict, &grid, &cell_a, &cell_b);
        assert!(result, "Symmetry enforcement should succeed");

        let count_after: u32 = state.domains.iter().map(|d| d.count).sum();
        assert!(
            count_after <= count_before,
            "Symmetry should not add candidates"
        );
    }
}

#[cfg(test)]
mod cached_mask_tests {
    use super::*;
    use orca_core::dict::Dictionary;

    #[test]
    fn prior_masks_match_full_letter_checks_after_shrink_and_reset() {
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
        let dict = Dictionary::parse(&text).unwrap();
        let bucket = dict.bucket(3).unwrap();
        let mut domain = bucket.all.clone();
        let mut letters = [ALL_LETTERS_MASK; MAX_SLOT_LEN];
        let mut witnesses: [_; 3] = std::array::from_fn(|_| crate::witnesses::Witnesses::default());
        for phase in 0..3 {
            if phase == 1 {
                for word in 0..676 {
                    domain.blocks_mut()[word / 64] &= !(1 << (word % 64));
                }
            } else if phase == 2 {
                domain = bucket.all.clone();
                letters.fill(ALL_LETTERS_MASK); // New propagation call after backtracking.
            }
            assert!(domain.count_ones() > SMALL_DOMAIN_THRESHOLD);
            for pos in 0..3 {
                letters[pos] = witnesses[pos].letters(&domain, bucket, pos, letters[pos]);
            }
            for (pos, &mask) in letters.iter().take(3).enumerate() {
                assert_eq!(
                    mask,
                    compute_possible_letters_at(&domain, domain.count_ones(), bucket, pos)
                );
            }
        }
    }
}

#[cfg(test)]
mod persistent_mask_tests {
    use super::*;
    use crate::state::SlotDomain;

    fn dictionary() -> Dictionary {
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
        Dictionary::parse(&text).unwrap()
    }
    fn refresh(state: &mut SolverState, bucket: &orca_core::dict::LengthBucket) {
        let d = &mut state.domains[0];
        let mut masks = d.letter_masks;
        for (pos, mask) in masks.iter().take(3).enumerate() {
            let exact = compute_possible_letters_at(&d.candidates, d.count, bucket, pos);
            assert_eq!(exact & !mask, 0, "cached mask must never exclude support");
        }
        batch_compute_possible_letters(&d.candidates, bucket, 7, &mut masks);
        for (pos, mask) in masks.iter().take(3).enumerate() {
            assert_eq!(
                *mask,
                compute_possible_letters_at(&d.candidates, d.count, bucket, pos)
            );
        }
        d.letter_masks = masks;
    }
    #[test]
    fn nested_backtracking_restores_masks_and_candidates_together() {
        let dict = dictionary();
        let b = dict.bucket(3).unwrap();
        let mut state = SolverState::new(vec![SlotDomain::new(b.all.clone())]);
        refresh(&mut state, b);
        let root_masks = state.domains[0].letter_masks;
        state.push_level();
        state.save_domain(0);
        state.domains[0].intersect(&b.letter_bits[0][0]);
        refresh(&mut state, b);
        let parent_masks = state.domains[0].letter_masks;
        assert_eq!(parent_masks[0], 1);
        state.push_level();
        state.save_domain(0);
        state.domains[0].intersect(&b.letter_bits[1][1]);
        refresh(&mut state, b);
        assert_eq!(state.domains[0].letter_masks[1], 2);
        let clone = state.clone();
        state.pop_level();
        assert_eq!(state.domains[0].letter_masks, parent_masks);
        refresh(&mut state, b);
        state.pop_level();
        assert_eq!(state.domains[0].letter_masks, root_masks);
        assert_eq!(state.domains[0].candidates, b.all);
        refresh(&mut state, b);
        assert_eq!(clone.domains[0].letter_masks[1], 2);
    }
    #[test]
    fn refinement_without_candidate_changes_needs_no_new_snapshot() {
        let dict = dictionary();
        let b = dict.bucket(3).unwrap();
        let mut state = SolverState::new(vec![SlotDomain::new(b.all.clone())]);
        state.push_level();
        refresh(&mut state, b);
        assert!(state.trail.is_empty());
        state.pop_level();
        refresh(&mut state, b);
        // A later candidate change must capture the already-refined parent mask.
        let masks = state.domains[0].letter_masks;
        state.push_level();
        state.save_domain(0);
        state.domains[0].intersect(&b.letter_bits[0][1]);
        refresh(&mut state, b);
        state.pop_level();
        assert_eq!(state.domains[0].letter_masks, masks);
        refresh(&mut state, b);
    }
}

#[cfg(test)]
mod propagation_cache_tests {
    use super::*;
    use crate::state::init_domains;

    fn assert_matches_words(
        state: &SolverState,
        graph: &ConstraintGraph,
        dict: &Dictionary,
        grid: &Grid,
    ) {
        for &(crossing_idx, side) in graph.neighbors.iter().flatten() {
            let crossing = &graph.crossings[crossing_idx];
            let source = if side {
                crossing.slot_a
            } else {
                crossing.slot_b
            };
            let (neighbor, pos, neighbor_pos) = graph.crossing_info(crossing_idx, side);
            let bucket = dict.bucket(grid.slots[source].len).unwrap();
            let domain = &state.domains[source];
            let letters = domain.candidates.iter_ones().fold(0u32, |mask, word| {
                mask | 1 << bucket.word_bytes[word * bucket.word_len() + pos]
            });
            assert_eq!(letters & !domain.letter_masks[pos], 0);
            let other = dict.bucket(grid.slots[neighbor].len).unwrap();
            for word in state.domains[neighbor].candidates.iter_ones() {
                assert_ne!(
                    letters & (1 << other.word_bytes[word * other.word_len() + neighbor_pos]),
                    0
                );
            }
        }
    }

    #[test]
    fn propagation_crosses_cutoff_and_restores_sibling_supports() {
        let text: String = (0..729)
            .map(|i| {
                format!(
                    "{}{}{};1\n",
                    (b'A' + (i / 81) as u8) as char,
                    (b'A' + (i / 9 % 9) as u8) as char,
                    (b'A' + (i % 9) as u8) as char
                )
            })
            .collect();
        let dict = Dictionary::parse(&text).unwrap();
        let grid = Grid::parse("3 3\n...\n...\n...\n").unwrap();
        let graph = ConstraintGraph::from_grid(&grid);
        let mut state = SolverState::new(init_domains(&grid, &dict));
        assert!(initial_propagate(&mut state, &graph, &dict, &grid));
        let root = state.domains.clone();
        let bucket = dict.bucket(3).unwrap();
        for count in [513, 512, 1, 512, 513] {
            state.push_level();
            state.save_domain(0);
            let mut subset = bucket.all.clone();
            for word in count..729 {
                subset.blocks_mut()[word / 64] &= !(1 << (word % 64));
            }
            state.domains[0].intersect(&subset);
            assert!(propagate(&mut state, &graph, &dict, &grid, 0));
            assert_matches_words(&state, &graph, &dict, &grid);
            // Clear all discovery caches in a clone and ensure the exact same fixpoint.
            let mut fresh = state.clone();
            fresh.prop_witnesses.clear();
            for domain in &mut fresh.domains {
                domain.letter_masks.fill(ALL_LETTERS_MASK);
            }
            let slots: Vec<_> = (0..grid.slots.len()).collect();
            assert!(propagate_from_slots(
                &mut fresh, &graph, &dict, &grid, &slots
            ));
            for (a, b) in state.domains.iter().zip(&fresh.domains) {
                assert_eq!(a.candidates, b.candidates);
            }
            // Failure must not leave negative knowledge behind after backtracking.
            state.push_level();
            state.save_domain(0);
            state.domains[0].intersect(&orca_core::bitset::BitSet::new(729));
            assert!(!propagate(&mut state, &graph, &dict, &grid, 0));
            state.pop_level();
            assert_matches_words(&state, &graph, &dict, &grid);
            state.pop_level();
            for (a, b) in state.domains.iter().zip(&root) {
                assert_eq!(a.candidates, b.candidates);
                assert_eq!(a.letter_masks, b.letter_masks);
            }
            assert!(propagate_from_slots(
                &mut state, &graph, &dict, &grid, &slots
            ));
            assert_matches_words(&state, &graph, &dict, &grid);
        }
    }
}
