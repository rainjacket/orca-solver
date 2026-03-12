//! Branching search with explicit stack and mid-search splitting.

use crate::constraint::ConstraintGraph;
use orca_core::dict::Dictionary;
use orca_core::grid::{set_letter_in_grid_text, Grid};

use crate::propagate::{
    compute_letter_counts_at, compute_possible_letters_at, enforce_cell_symmetry,
    initial_propagate, propagate,
};
use crate::state::{init_domains, SolverState};
use crate::stats::SolverStats;

/// Resolved cell info for symmetry breaking.
#[derive(Debug, Clone)]
pub struct CellSymInfo {
    pub slot_id: usize,
    pub pos_in_slot: usize,
}

/// Search configuration for the solver.
#[derive(Clone, Debug)]
pub struct SearchConfig {
    /// Maximum number of solutions to find (0 = unlimited).
    pub max_solutions: u64,
    /// Report progress every N nodes (0 = no progress reporting).
    pub progress_interval: u64,
    /// Symmetry breaking: enforce letter(cell_a) ≤ letter(cell_b) during propagation.
    pub symmetry_break_cells: Option<(CellSymInfo, CellSymInfo)>,
    /// Split timeout in seconds (0 = disabled). When exceeded, remaining work
    /// at ALL stack frames is split into sub-partitions.
    pub split_timeout_secs: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            max_solutions: 0,
            progress_interval: 10_000,
            symmetry_break_cells: None,
            split_timeout_secs: 0,
        }
    }
}

/// Resolve a (row, col) cell to its first constrained, non-check-only slot.
pub fn resolve_cell(grid: &Grid, row: usize, col: usize) -> Option<CellSymInfo> {
    for (slot_idx, slot) in grid.slots.iter().enumerate() {
        if !slot.constrained || slot.check_only {
            continue;
        }
        for (pos, &(r, c)) in slot.cells.iter().enumerate() {
            if r == row && c == col {
                return Some(CellSymInfo {
                    slot_id: slot_idx,
                    pos_in_slot: pos,
                });
            }
        }
    }
    None
}

/// Validate a fill: build assignments from singleton domains, check for
/// duplicate words and shared substrings. Returns `Some(assignments)` if
/// valid, `None` if a duplicate word or shared substring violation is found.
fn validate_fill<'a>(
    state: &SolverState,
    dict: &'a Dictionary,
    grid: &Grid,
    disallow_shared_substring: usize,
) -> Option<Vec<Option<&'a str>>> {
    let mut assignments: Vec<Option<&str>> = vec![None; grid.slots.len()];
    let mut word_keys = std::collections::HashSet::new();
    let mut all_substrings: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for (slot_idx, slot) in grid.slots.iter().enumerate() {
        if !slot.constrained {
            continue;
        }
        let domain = &state.domains[slot_idx];
        if domain.count != 1 {
            continue;
        }

        let word_id = domain.candidates.first_one().unwrap();
        let bucket = dict.bucket(slot.len)?;
        let word_text = &bucket.words[word_id];
        assignments[slot_idx] = Some(word_text.as_str());

        // Duplicate check: encode (word_len, word_id) into a single u64 key
        let word_key = (slot.len as u64) << 32 | word_id as u64;
        if !word_keys.insert(word_key) {
            return None;
        }

        // Shared substring check
        if disallow_shared_substring > 0 {
            for len in disallow_shared_substring..=word_text.len() {
                for start in 0..=word_text.len() - len {
                    let sub = &word_text[start..start + len];
                    if !all_substrings.insert(sub) {
                        return None;
                    }
                }
            }
        }
    }

    Some(assignments)
}

/// Format a solution as (grid_text, word_list) from an assignment vector.
fn format_solution(grid: &Grid, assignments: &[Option<&str>]) -> (String, Vec<String>) {
    let grid_text = grid.format_filled(assignments);
    let words = assignments
        .iter()
        .filter_map(|a| a.map(|s| s.to_string()))
        .collect();
    (grid_text, words)
}

/// Check whether all constrained non-check-only slots have a single candidate.
fn all_slots_assigned(state: &SolverState, grid: &Grid) -> bool {
    grid.slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.constrained && !s.check_only)
        .all(|(i, _)| state.domains[i].count <= 1)
}

/// Search state after initial propagation, ready for partitioning or solving.
///
/// Created by `prepare_search`. Workers reconstruct this deterministically
/// from their seeded grid+dict, so it doesn't need to be serialized.
pub struct PreparedSearch {
    pub(crate) state: SolverState,
    pub(crate) graph: ConstraintGraph,
}

/// Outcome of `prepare_search`.
pub enum PrepareOutcome {
    /// Normal case: search tree has a real choice point to split on.
    Ready(PreparedSearch),
    /// The grid was solved trivially (all slots had exactly 1 candidate after propagation).
    TrivialSolution(Vec<(String, Vec<String>)>),
    /// No solutions exist (contradiction during initial propagation).
    NoSolutions,
}

/// A sub-partition created when a running partition exceeds its split timeout.
pub struct SubPartition {
    /// Complete grid text with all ancestor letters applied plus the split letter.
    pub grid_contents: String,
    /// Human-readable description of the sub-partition path.
    pub seed_desc: String,
}

/// Result of executing a single partition.
pub struct PartitionResult {
    pub solutions: Vec<(String, Vec<String>)>,
    pub stats: SolverStats,
    pub exhausted: bool,
    /// Sub-partitions created by mid-search splitting (empty if no split occurred).
    pub sub_partitions: Vec<SubPartition>,
}

impl PartitionResult {
    fn empty() -> Self {
        PartitionResult {
            solutions: Vec::new(),
            stats: SolverStats::new(),
            exhausted: true,
            sub_partitions: Vec::new(),
        }
    }
}

/// Build constraint graph, initialize domains, and run initial propagation.
/// Returns `None` on contradiction (no solutions exist).
fn setup(grid: &Grid, dict: &Dictionary) -> Option<(SolverState, ConstraintGraph)> {
    let graph = ConstraintGraph::from_grid(grid);
    let domains = init_domains(grid, dict);
    let mut state = SolverState::new(domains);

    if initial_propagate(&mut state, &graph, dict, grid) {
        Some((state, graph))
    } else {
        None
    }
}

/// From raw inputs, prepare the search state up to the first real choice point.
///
/// The result is deterministic given the same inputs, so distributed workers
/// can reconstruct it independently.
pub fn prepare_search(
    grid: &Grid,
    dict: &Dictionary,
    disallow_shared_substring: usize,
) -> PrepareOutcome {
    let (state, graph) = match setup(grid, dict) {
        Some(sg) => sg,
        None => return PrepareOutcome::NoSolutions,
    };

    if all_slots_assigned(&state, grid) {
        match validate_fill(&state, dict, grid, disallow_shared_substring) {
            Some(assignments) => {
                let (grid_text, words) = format_solution(grid, &assignments);
                PrepareOutcome::TrivialSolution(vec![(grid_text, words)])
            }
            None => PrepareOutcome::NoSolutions,
        }
    } else {
        PrepareOutcome::Ready(PreparedSearch { state, graph })
    }
}

/// Solve a grid end-to-end: build constraint graph, propagate, and search.
///
/// Self-contained entry point used by `solve_parallel` and the distributed worker.
/// Pass `grid_text` to enable mid-search splitting when `config.split_timeout_secs > 0`.
pub fn solve_grid(
    grid: &Grid,
    dict: &Dictionary,
    config: &SearchConfig,
    disallow_shared_substring: usize,
    grid_text: Option<&str>,
) -> PartitionResult {
    let (mut state, graph) = match setup(grid, dict) {
        Some(sg) => sg,
        None => return PartitionResult::empty(),
    };

    // Apply symmetry constraint after initial propagation
    if let Some((ref ca, ref cb)) = config.symmetry_break_cells {
        if !enforce_cell_symmetry(&mut state, &graph, dict, grid, ca, cb) {
            return PartitionResult::empty();
        }
    }

    let mut solutions = Vec::new();
    state.stats.start();
    let (result, sub_partitions) = search(
        &mut state,
        &graph,
        dict,
        grid,
        config,
        disallow_shared_substring,
        &mut |grid, assignments| {
            solutions.push(format_solution(grid, assignments));
        },
        grid_text,
    );

    PartitionResult {
        solutions,
        stats: state.stats,
        exhausted: result,
        sub_partitions,
    }
}

/// Maximum crossings to evaluate before committing to the best found so far.
/// Used by both per-node branching (`select_branch`) and partition
/// splitting (via `find_best_crossing`). Crossings are pre-sorted by static
/// length-sum (descending sum of slot lengths) in `ConstraintGraph::from_grid()`,
/// so the bounded scan evaluates longer-slot crossings first.
const CROSSING_SCAN_LIMIT: usize = 15;

/// Shared crossing evaluation used by both partition splitting and per-node
/// branching. Scans up to `CROSSING_SCAN_LIMIT` valid crossings (sorted by
/// static length-sum order) and picks the one minimizing Σ_l count_a[l] * count_b[l],
/// tie-broken by viable_count.
fn find_best_crossing(
    state: &SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
) -> Option<BranchPoint> {
    let mut best_score = (u64::MAX, u64::MAX);
    let mut best_choice: Option<BranchPoint> = None;
    let mut evaluated = 0usize;

    for crossing in &graph.crossings {
        let count_a = state.domains[crossing.slot_a].count;
        let count_b = state.domains[crossing.slot_b].count;

        if count_a <= 1 || count_b <= 1 {
            continue;
        }

        let slot_a = &grid.slots[crossing.slot_a];
        let slot_b = &grid.slots[crossing.slot_b];
        if !slot_a.constrained || !slot_b.constrained {
            continue;
        }
        if slot_a.check_only || slot_b.check_only {
            continue;
        }

        let bucket_a = match dict.bucket(slot_a.len) {
            Some(b) => b,
            None => continue,
        };
        let bucket_b = match dict.bucket(slot_b.len) {
            Some(b) => b,
            None => continue,
        };

        let counts_a = compute_letter_counts_at(
            &state.domains[crossing.slot_a].candidates,
            count_a,
            bucket_a,
            crossing.pos_in_a,
        );
        let counts_b = compute_letter_counts_at(
            &state.domains[crossing.slot_b].candidates,
            count_b,
            bucket_b,
            crossing.pos_in_b,
        );

        // Build viable mask and compute sum of per-letter products.
        // For each viable letter l, count_a[l] * count_b[l] estimates the
        // sub-tree size when branching on l. The sum estimates total work.
        let mut viable_mask = 0u32;
        let mut work_sum = 0u64;
        for letter in 0..26usize {
            if counts_a[letter] > 0 && counts_b[letter] > 0 {
                viable_mask |= 1u32 << letter;
                work_sum += counts_a[letter] as u64 * counts_b[letter] as u64;
            }
        }
        let viable_count = viable_mask.count_ones() as u64;

        if viable_count <= 1 {
            continue;
        }

        evaluated += 1;

        // Minimize sum of per-letter domain products: Σ_l count_a[l] * count_b[l].
        // This estimates total work across all branches at this crossing.
        // Ties broken by viable_count (fewer branches = better).
        let score = (work_sum, viable_count);

        if score < best_score {
            let (row, col) = slot_a.cells[crossing.pos_in_a];
            let (primary_slot, primary_pos) = if count_a <= count_b {
                (crossing.slot_a, crossing.pos_in_a)
            } else {
                (crossing.slot_b, crossing.pos_in_b)
            };
            best_score = score;
            best_choice = Some(BranchPoint {
                slot_id: primary_slot,
                pos_in_slot: primary_pos,
                viable_mask,
                row,
                col,
                affected: [
                    (crossing.slot_a, crossing.pos_in_a),
                    (crossing.slot_b, crossing.pos_in_b),
                ],
            });
        }

        if evaluated >= CROSSING_SCAN_LIMIT {
            break;
        }
    }

    best_choice
}

/// A crossing cell selected for branching.
pub(crate) struct BranchPoint {
    /// Slot to restrict (the one with fewer candidates).
    pub(crate) slot_id: usize,
    /// Position within that slot of the branching cell.
    pub(crate) pos_in_slot: usize,
    /// Bitmask of viable letters (bit i = letter i is viable, A=0).
    pub(crate) viable_mask: u32,
    /// Grid row/col of the branching cell.
    pub(crate) row: usize,
    pub(crate) col: usize,
    /// The two slots that cross at this cell: (slot_id, pos_in_slot).
    /// For the MRV fallback (single-slot), both entries are the same slot.
    pub(crate) affected: [(usize, usize); 2],
}

/// Select a crossing cell for branching.
///
/// Evaluates up to `CROSSING_SCAN_LIMIT` crossings (sorted by length-sum)
/// using the sum-of-products heuristic. Falls back to MRV when no crossings
/// have multiple viable letters.
pub(crate) fn select_branch(
    state: &SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
) -> Option<BranchPoint> {
    find_best_crossing(state, graph, dict, grid)
        .or_else(|| select_branch_fallback(state, dict, grid))
}

/// Fallback for `select_branch`: pick the smallest-domain undecided slot
/// and find a position with 2+ viable letters. This only fires when no crossings
/// have multiple viable letters (typically once per solution, at the leaf).
fn select_branch_fallback(
    state: &SolverState,
    dict: &Dictionary,
    grid: &Grid,
) -> Option<BranchPoint> {
    // MRV: pick the constrained, non-check-only slot with smallest domain > 1.
    let slot_id = grid
        .slots
        .iter()
        .enumerate()
        .filter(|(i, s)| s.constrained && !s.check_only && state.domains[*i].count > 1)
        .min_by_key(|(i, _)| state.domains[*i].count)
        .map(|(i, _)| i)?;

    let bucket = dict.bucket(grid.slots[slot_id].len)?;
    let count = state.domains[slot_id].count;
    let slot_len = grid.slots[slot_id].len;

    // Find a position where the domain disagrees (2+ viable letters).
    // Hardcoding position 0 causes an infinite loop when all remaining
    // words agree at position 0 but differ elsewhere.
    for pos in 0..slot_len {
        let viable_mask =
            compute_possible_letters_at(&state.domains[slot_id].candidates, count, bucket, pos);
        if viable_mask.count_ones() >= 2 {
            let (row, col) = grid.slots[slot_id].cells[pos];
            return Some(BranchPoint {
                slot_id,
                pos_in_slot: pos,
                viable_mask,
                row,
                col,
                affected: [(slot_id, pos), (slot_id, pos)],
            });
        }
    }

    // This is unreachable: deduplicated words that agree at all positions
    // would be the same word.
    unreachable!();
}

/// At a leaf node (all domains are singletons), validate the fill and
/// report the solution. Returns true if we should stop (MaxSolutions reached).
fn check_solution<F>(
    state: &mut SolverState,
    dict: &Dictionary,
    grid: &Grid,
    config: &SearchConfig,
    disallow_shared_substring: usize,
    on_solution: &mut F,
) -> bool
where
    F: FnMut(&Grid, &[Option<&str>]),
{
    let assignments = match validate_fill(state, dict, grid, disallow_shared_substring) {
        Some(a) => a,
        None => {
            state.stats.dupes_skipped += 1;
            return false;
        }
    };

    state.stats.solutions += 1;
    on_solution(grid, &assignments);

    config.max_solutions > 0 && state.stats.solutions >= config.max_solutions
}

/// A frame on the explicit search stack, replacing one level of recursion.
struct SearchFrame {
    /// The branch decision at this level.
    slot_id: usize,
    pos_in_slot: usize,
    /// Bitmask of remaining letters to try (bit i = letter i not yet tried).
    /// Consumed via trailing_zeros + clear-lowest-bit.
    remaining_letters: u32,
    /// True if we descended into a child after an Ok propagation and haven't
    /// yet popped the trail for that letter attempt.
    child_trail_active: bool,
    /// The letter chosen at this level (set when descending into a child).
    /// Used by mid-search splitting to reconstruct the path from root to current node.
    chosen_letter: u8,
}

/// Backtrack to the nearest frame with remaining letters.
/// Pops exhausted frames and undoes their trail entries.
/// Returns `true` if a viable frame was found, `false` if stack is empty.
#[inline]
fn backtrack_to_viable(stack: &mut Vec<SearchFrame>, state: &mut SolverState) -> bool {
    loop {
        let frame = match stack.last_mut() {
            Some(f) => f,
            None => return false,
        };
        if frame.child_trail_active {
            state.pop_level();
            frame.child_trail_active = false;
        }
        if frame.remaining_letters != 0 {
            return true;
        }
        stack.pop();
    }
}

/// Core search loop (iterative with explicit stack to avoid stack overflow).
///
/// When `grid_text` is `Some` and `config.split_timeout_secs > 0`, the search
/// will split remaining work into sub-partitions after the timeout is exceeded.
fn search<F>(
    state: &mut SolverState,
    graph: &ConstraintGraph,
    dict: &Dictionary,
    grid: &Grid,
    config: &SearchConfig,
    disallow_shared_substring: usize,
    on_solution: &mut F,
    grid_text: Option<&str>,
) -> (bool, Vec<SubPartition>)
where
    F: FnMut(&Grid, &[Option<&str>]),
{
    let mut stack: Vec<SearchFrame> = Vec::new();
    // Split timeout: None if disabled, cleared after the first split fires.
    let mut split_state = if config.split_timeout_secs > 0 && grid_text.is_some() {
        Some((
            std::time::Duration::from_secs(config.split_timeout_secs),
            std::time::Instant::now(),
        ))
    } else {
        None
    };
    let mut sub_partitions: Vec<SubPartition> = Vec::new();
    let mut post_split = false;

    // Outer loop: at each iteration we've arrived at a new node to explore
    // (either the root, or we just descended after a successful propagation).
    'descend: loop {
        state.stats.nodes += 1;

        // Progress reporting
        if config.progress_interval > 0 && state.stats.nodes % config.progress_interval == 0 {
            eprintln!("[progress] {}", state.stats);
        }

        // Mid-search split: after timeout, extract remaining untried letters from every
        // stack frame as sub-partitions, then finish the current path.
        if let Some((timeout, start)) = split_state {
            if state.stats.nodes % 1_000 == 0 && start.elapsed() >= timeout {
                sub_partitions = split_remaining_work(&mut stack, grid, grid_text.unwrap());
                if !sub_partitions.is_empty() {
                    eprintln!(
                        "[split] Timeout after {:.1}s at {} nodes, split into {} sub-partitions",
                        start.elapsed().as_secs_f64(),
                        state.stats.nodes,
                        sub_partitions.len(),
                    );
                }
                split_state = None;
                post_split = true;
            }
        }

        // Select the next cell to branch on
        let branch = select_branch(state, graph, dict, grid);

        // Forced move: cell has exactly 1 viable letter, no branching needed.
        // Apply inline and save domain into the parent's trail entry so
        // backtracking undoes everything at once. This prevents unbounded
        // trail growth on deep forced chains.
        if let Some(ref b) = branch {
            if b.viable_mask.count_ones() == 1 && !state.trail_levels.is_empty() {
                let forced_letter = b.viable_mask.trailing_zeros() as u8;
                let fb_bucket = match dict.bucket(grid.slots[b.slot_id].len) {
                    Some(bk) => bk,
                    None => {
                        state.stats.backtracks += 1;
                        if !backtrack_to_viable(&mut stack, state) {
                            return (true, sub_partitions);
                        }
                        continue 'descend;
                    }
                };
                state.save_domain(b.slot_id);
                let letter_bits = &fb_bucket.letter_bits[b.pos_in_slot][forced_letter as usize];
                state.domains[b.slot_id].intersect_incremental(letter_bits);
                if state.domains[b.slot_id].is_empty() {
                    state.stats.backtracks += 1;
                } else {
                    if propagate(state, graph, dict, grid, b.slot_id) {
                        if let Some((ref ca, ref cb)) = config.symmetry_break_cells {
                            if !enforce_cell_symmetry(state, graph, dict, grid, ca, cb) {
                                state.stats.backtracks += 1;
                                if !backtrack_to_viable(&mut stack, state) {
                                    return (true, sub_partitions);
                                }
                                continue 'descend;
                            }
                        }
                        continue 'descend;
                    } else {
                        state.stats.backtracks += 1;
                    }
                }
                // Forced move wiped out — backtrack to a viable frame.
                if !backtrack_to_viable(&mut stack, state) {
                    return (true, sub_partitions);
                }
                // Fall through to try next letter at the current frame
            } else {
                // Normal branch (2+ viable letters): push frame
                if dict.bucket(grid.slots[b.slot_id].len).is_none() {
                    state.stats.backtracks += 1;
                } else if post_split {
                    // Post-split: emit sub-partitions instead of descending.
                    let (path_grid, path_desc) = build_path_grid(&stack, grid, grid_text.unwrap());
                    let (br_row, br_col) = grid.slots[b.slot_id].cells[b.pos_in_slot];
                    emit_sub_partitions(
                        &mut sub_partitions,
                        &path_grid,
                        &path_desc,
                        br_row,
                        br_col,
                        b.viable_mask,
                        "deep-split",
                    );
                    eprintln!(
                        "[split] Post-split: {} deep sub-partitions at ({},{})",
                        b.viable_mask.count_ones(),
                        br_row,
                        br_col
                    );
                } else {
                    stack.push(SearchFrame {
                        slot_id: b.slot_id,
                        pos_in_slot: b.pos_in_slot,
                        remaining_letters: b.viable_mask,
                        child_trail_active: false,
                        chosen_letter: 0,
                    });
                }
            }
        } else {
            // Leaf: all constrained non-check-only domains are singletons
            if check_solution(
                state,
                dict,
                grid,
                config,
                disallow_shared_substring,
                on_solution,
            ) {
                // Unwind the entire stack
                for frame in stack.iter().rev() {
                    if frame.child_trail_active {
                        state.pop_level();
                    }
                }
                return (false, sub_partitions);
            }
        }

        // Backtrack loop: pop the trail from the previous child (if any),
        // try the next letter at the current frame, or pop exhausted frames.
        loop {
            let frame = match stack.last_mut() {
                Some(f) => f,
                None => return (true, sub_partitions),
            };

            // If we just returned from exploring a child subtree, pop its trail
            if frame.child_trail_active {
                state.pop_level();
                frame.child_trail_active = false;
            }

            // Try each remaining letter at this frame
            while frame.remaining_letters != 0 {
                let letter = frame.remaining_letters.trailing_zeros() as u8;
                frame.remaining_letters &= frame.remaining_letters - 1;

                let bucket = dict.bucket(grid.slots[frame.slot_id].len).unwrap();

                state.push_level();
                state.save_domain(frame.slot_id);

                // Restrict domain: keep only words with this letter at this position
                let letter_bits = &bucket.letter_bits[frame.pos_in_slot][letter as usize];
                state.domains[frame.slot_id].intersect_incremental(letter_bits);

                if state.domains[frame.slot_id].is_empty() {
                    state.stats.backtracks += 1;
                    state.pop_level();
                    continue;
                }

                // Propagate from the restricted slot
                let prop_result = propagate(state, graph, dict, grid, frame.slot_id);

                if prop_result {
                    // Symmetry check
                    if let Some((ref ca, ref cb)) = config.symmetry_break_cells {
                        if !enforce_cell_symmetry(state, graph, dict, grid, ca, cb) {
                            state.stats.backtracks += 1;
                            state.pop_level();
                            continue; // try next letter
                        }
                    }
                    // Record which letter we're exploring before descending
                    frame.chosen_letter = letter;
                    // Mark that we have an active child trail to pop later
                    frame.child_trail_active = true;
                    // Descend: go to the outer loop to explore the child node
                    continue 'descend;
                } else {
                    state.stats.backtracks += 1;
                }

                state.pop_level();
            }

            // All letters exhausted at this frame — pop it and backtrack further
            stack.pop();
        }
    }
}

/// Build the path grid and description prefix from ancestor frames.
/// Returns (path_grid, desc_prefix) where desc_prefix is "(...) + " or empty.
fn build_path_grid(ancestors: &[SearchFrame], grid: &Grid, grid_text: &str) -> (String, String) {
    let mut path_grid = grid_text.to_string();
    let mut desc_parts = Vec::new();
    for frame in ancestors {
        let (row, col) = grid.slots[frame.slot_id].cells[frame.pos_in_slot];
        let letter_char = (b'A' + frame.chosen_letter) as char;
        path_grid = set_letter_in_grid_text(&path_grid, row, col, letter_char);
        desc_parts.push(format!("({},{})={}", row, col, letter_char));
    }
    let prefix = if desc_parts.is_empty() {
        String::new()
    } else {
        desc_parts.join(" + ") + " + "
    };
    (path_grid, prefix)
}

/// Emit sub-partitions for each letter in `letter_mask` at the given cell.
fn emit_sub_partitions(
    out: &mut Vec<SubPartition>,
    path_grid: &str,
    path_desc: &str,
    row: usize,
    col: usize,
    letter_mask: u32,
    tag: &str,
) {
    let mut bits = letter_mask;
    while bits != 0 {
        let letter = bits.trailing_zeros() as u8;
        bits &= bits - 1;
        let letter_char = (b'A' + letter) as char;
        let sub_grid = set_letter_in_grid_text(path_grid, row, col, letter_char);
        out.push(SubPartition {
            grid_contents: sub_grid,
            seed_desc: format!("{}({},{})={} [{}]", path_desc, row, col, letter_char, tag),
        });
    }
}

/// Build sub-partitions from the remaining work on the search stack.
///
/// Iterates ALL frames in the stack. For each frame with remaining letters,
/// builds a path grid from ancestors (frames 0..split_idx), creates one
/// sub-partition per remaining letter, then zeroes `remaining_letters`.
/// After this, the worker has no branching left — it finishes only the
/// single current leaf path and returns in seconds.
fn split_remaining_work(
    stack: &mut [SearchFrame],
    grid: &Grid,
    grid_text: &str,
) -> Vec<SubPartition> {
    let mut sub_partitions = Vec::new();

    for split_idx in 0..stack.len() {
        let remaining = stack[split_idx].remaining_letters;
        if remaining == 0 {
            continue;
        }
        stack[split_idx].remaining_letters = 0;

        let (path_grid, path_desc) = build_path_grid(&stack[..split_idx], grid, grid_text);
        let (row, col) = grid.slots[stack[split_idx].slot_id].cells[stack[split_idx].pos_in_slot];
        emit_sub_partitions(
            &mut sub_partitions,
            &path_grid,
            &path_desc,
            row,
            col,
            remaining,
            "split",
        );
    }

    sub_partitions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::test_utils::test_dict;

    #[test]
    fn test_solve_grid_exhaustive() {
        let dict = test_dict();
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        let config = SearchConfig::default();

        let result = super::solve_grid(&grid, &dict, &config, 0, None);
        assert!(result.exhausted, "Should exhaust the search space");
        // Every solution should have no duplicate words
        for (_, words) in &result.solutions {
            let unique: HashSet<&str> = words.iter().map(|s| s.as_str()).collect();
            assert_eq!(
                unique.len(),
                words.len(),
                "Found a solution with duplicate words: {:?}",
                words,
            );
        }
    }

    #[test]
    fn test_split_remaining_work_basic() {
        // Construct a mock stack and verify split_remaining_work produces correct sub-partitions.
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();

        // Frame 0: slot 0, pos 0, currently exploring letter A (0), remaining B (1) and C (2)
        // Frame 1: slot 1, pos 1, currently exploring letter D (3), no remaining
        let mut stack = vec![
            SearchFrame {
                slot_id: 0,
                pos_in_slot: 0,
                remaining_letters: 0b110, // bits 1 (B) and 2 (C)
                child_trail_active: true,
                chosen_letter: 0, // A
            },
            SearchFrame {
                slot_id: 1,
                pos_in_slot: 1,
                remaining_letters: 0,
                child_trail_active: true,
                chosen_letter: 3, // D
            },
        ];

        let subs = split_remaining_work(&mut stack, &grid, grid_str);

        // Should split at frame 0 (only frame with remaining_letters != 0)
        assert_eq!(
            subs.len(),
            2,
            "Expected 2 sub-partitions for letters B and C"
        );
        assert_eq!(
            stack[0].remaining_letters, 0,
            "remaining_letters should be zeroed"
        );

        // Each sub-partition should have the split letter applied at frame 0's cell
        let (row, col) = grid.slots[0].cells[0];
        for sp in &subs {
            // The grid should have a letter at the split cell
            let lines: Vec<&str> = sp.grid_contents.lines().collect();
            let data_line = lines[1]; // first data line (row 0)
            let chars: Vec<char> = data_line.chars().collect();
            assert!(
                chars[col].is_ascii_uppercase(),
                "Sub-partition grid should have a letter at ({},{}): got '{}'",
                row,
                col,
                chars[col]
            );
        }

        // Verify seed_desc mentions [split]
        for sp in &subs {
            assert!(
                sp.seed_desc.contains("[split]"),
                "seed_desc should contain [split]"
            );
        }
    }

    #[test]
    fn test_split_remaining_work_with_ancestors() {
        // Verify that ancestor frames' chosen_letter values are applied to the path grid.
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();

        // Frame 0 is already exhausted (remaining=0), chosen_letter=E
        // Frame 1 has remaining letters
        let mut stack = vec![
            SearchFrame {
                slot_id: 0,
                pos_in_slot: 0,
                remaining_letters: 0,
                child_trail_active: true,
                chosen_letter: 4, // E
            },
            SearchFrame {
                slot_id: 1,
                pos_in_slot: 0,
                remaining_letters: 0b1001, // A (0) and D (3)
                child_trail_active: true,
                chosen_letter: 5, // F (currently exploring)
            },
        ];

        let subs = split_remaining_work(&mut stack, &grid, grid_str);

        assert_eq!(subs.len(), 2);
        assert_eq!(stack[1].remaining_letters, 0);

        // Each sub-partition should have ancestor's letter (E) applied
        let (anc_row, anc_col) = grid.slots[0].cells[0];
        for sp in &subs {
            let lines: Vec<&str> = sp.grid_contents.lines().collect();
            let data_line = lines[1 + anc_row];
            let chars: Vec<char> = data_line.chars().collect();
            assert_eq!(
                chars[anc_col], 'E',
                "Ancestor letter E should be in sub-partition grid at ({},{})",
                anc_row, anc_col
            );
        }

        // Verify seed_desc includes ancestor path
        for sp in &subs {
            assert!(
                sp.seed_desc
                    .contains(&format!("({},{})=E", anc_row, anc_col)),
                "seed_desc should mention ancestor: {}",
                sp.seed_desc
            );
        }
    }

    #[test]
    fn test_split_remaining_work_multiple_frames() {
        // Verify that split_remaining_work splits ALL frames with remaining letters,
        // not just the shallowest one.
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();

        // Frame 0: remaining B,C (2 letters). Frame 1: remaining D,E,F (3 letters).
        let mut stack = vec![
            SearchFrame {
                slot_id: 0,
                pos_in_slot: 0,
                remaining_letters: 0b110, // B (1) and C (2)
                child_trail_active: true,
                chosen_letter: 0, // A (currently exploring)
            },
            SearchFrame {
                slot_id: 1,
                pos_in_slot: 0,
                remaining_letters: 0b111000, // D (3), E (4), F (5)
                child_trail_active: true,
                chosen_letter: 6, // G (currently exploring)
            },
        ];

        let subs = split_remaining_work(&mut stack, &grid, grid_str);

        // 2 from frame 0 + 3 from frame 1 = 5 total
        assert_eq!(subs.len(), 5, "Expected 5 sub-partitions from both frames");
        assert_eq!(
            stack[0].remaining_letters, 0,
            "Frame 0 remaining should be zeroed"
        );
        assert_eq!(
            stack[1].remaining_letters, 0,
            "Frame 1 remaining should be zeroed"
        );

        // Frame 0 subs should NOT have ancestor letters (frame 0 is root)
        // Frame 1 subs should have frame 0's chosen_letter (A) as ancestor
        let (cell0_row, cell0_col) = grid.slots[0].cells[0];
        let (cell1_row, cell1_col) = grid.slots[1].cells[0];

        // First 2 subs are from frame 0 (B, C) — split cell is cell0
        for sp in &subs[..2] {
            let lines: Vec<&str> = sp.grid_contents.lines().collect();
            let chars: Vec<char> = lines[1 + cell0_row].chars().collect();
            assert!(
                chars[cell0_col] == 'B' || chars[cell0_col] == 'C',
                "Frame 0 sub should have B or C at ({},{}): got '{}'",
                cell0_row,
                cell0_col,
                chars[cell0_col]
            );
        }

        // Last 3 subs are from frame 1 (D, E, F) — should include ancestor A at cell0
        for sp in &subs[2..] {
            let lines: Vec<&str> = sp.grid_contents.lines().collect();
            let chars0: Vec<char> = lines[1 + cell0_row].chars().collect();
            assert_eq!(
                chars0[cell0_col], 'A',
                "Frame 1 sub should have ancestor A at ({},{}): got '{}'",
                cell0_row, cell0_col, chars0[cell0_col]
            );
            let chars1: Vec<char> = lines[1 + cell1_row].chars().collect();
            assert!(
                chars1[cell1_col] == 'D' || chars1[cell1_col] == 'E' || chars1[cell1_col] == 'F',
                "Frame 1 sub should have D/E/F at ({},{}): got '{}'",
                cell1_row,
                cell1_col,
                chars1[cell1_col]
            );
        }
    }

    #[test]
    fn test_split_remaining_work_empty_stack() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        let mut stack: Vec<SearchFrame> = Vec::new();

        let subs = split_remaining_work(&mut stack, &grid, grid_str);
        assert!(
            subs.is_empty(),
            "Empty stack should produce no sub-partitions"
        );
    }

    #[test]
    fn test_split_remaining_work_no_remaining() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        let mut stack = vec![SearchFrame {
            slot_id: 0,
            pos_in_slot: 0,
            remaining_letters: 0,
            child_trail_active: true,
            chosen_letter: 0,
        }];

        let subs = split_remaining_work(&mut stack, &grid, grid_str);
        assert!(
            subs.is_empty(),
            "No remaining letters should produce no sub-partitions"
        );
    }

    #[test]
    fn test_solve_grid_with_split_enabled_matches_baseline() {
        // Verify that enabling split timeout doesn't change search results
        // when the search finishes before the timeout fires.
        let dict = test_dict();
        let grid_str = "3 3\n...\n...\n...\n";

        // Run without split
        let config_nosplit = SearchConfig {
            max_solutions: 0,
            progress_interval: 0,
            symmetry_break_cells: None,
            split_timeout_secs: 0,
        };
        let baseline = super::solve_grid(
            &Grid::parse(grid_str).unwrap(),
            &dict,
            &config_nosplit,
            0,
            None,
        );

        // Run with split enabled but long timeout (search finishes first)
        let config_split = SearchConfig {
            max_solutions: 0,
            progress_interval: 0,
            symmetry_break_cells: None,
            split_timeout_secs: 600,
        };
        let split_result = super::solve_grid(
            &Grid::parse(grid_str).unwrap(),
            &dict,
            &config_split,
            0,
            Some(grid_str),
        );

        assert!(
            split_result.sub_partitions.is_empty(),
            "No split should occur when search finishes before timeout"
        );
        assert_eq!(
            baseline.solutions.len(),
            split_result.solutions.len(),
            "Split-enabled search should find same solutions as baseline"
        );
        assert_eq!(baseline.exhausted, split_result.exhausted);
    }
}
