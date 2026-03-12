//! Partition generation: splits a search space into balanced sub-problems
//! for distributed or parallel execution. Uses a priority-queue strategy
//! that repeatedly splits the largest estimated partition.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;

use orca_core::dict::Dictionary;
use orca_core::grid::{set_cell_in_grid_text, set_letter_in_grid_text, Grid};

use crate::propagate::propagate_from_slots;
use crate::search::{prepare_search, select_branch, PrepareOutcome, PreparedSearch};
use crate::state::SlotDomain;

/// A cell seeded during partition splitting.
#[derive(Debug, Clone)]
pub struct SeedCell {
    pub row: usize,
    pub col: usize,
    pub letter: char,
}

/// A partition specification: a seeded grid text and human-readable description.
pub struct PartitionSpec {
    /// Grid text with seeded letters at crossing cells.
    pub grid_text: String,
    /// Human-readable description of the seeds, e.g. "(0,3)=A (1,2)=B".
    pub seed_desc: String,
    /// Cells seeded during partition splitting.
    pub seed_cells: Vec<SeedCell>,
}

/// Default maximum number of partitions for priority-queue splitting.
pub const DEFAULT_MAX_PARTITIONS: usize = 10_000;

/// Estimate remaining search work as sum of log2(domain_size) over undecided
/// slots. Approximates log of the search tree size. Used only to prioritize
/// which partition to split next (biggest estimated tree = split first).
fn partition_weight(state: &crate::state::SolverState, grid: &Grid) -> f64 {
    grid.slots
        .iter()
        .enumerate()
        .filter(|(i, s)| s.constrained && !s.check_only && state.domains[*i].count > 1)
        .map(|(i, _)| (state.domains[i].count as f64).log2())
        .sum()
}

/// A partition candidate in the priority queue, ordered by work estimate (highest first).
struct QueueEntry {
    weight: f64,
    domains: Vec<SlotDomain>,
    grid_text: String,
    seed_desc: String,
    seed_cells: Vec<SeedCell>,
}

impl Eq for QueueEntry {}
impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.weight == other.weight
    }
}
impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.weight
            .partial_cmp(&other.weight)
            .unwrap_or(CmpOrdering::Equal)
    }
}

/// Split a search space into partitions using a priority-queue strategy.
///
/// Maintains a max-heap and repeatedly splits the largest partition one level
/// until the partition count reaches `max_partitions` or no more splits are
/// possible.
///
/// This produces much more balanced partitions than fixed-depth splitting:
/// easy branches get split less, hard branches get split more.
///
/// If `prepared` is provided, reuses the already-computed search state.
/// Otherwise, parses the grid and runs `prepare_search` internally.
pub fn generate_partitions(
    grid_text: &str,
    dict: &Dictionary,
    disallow_shared_substring: usize,
    max_partitions: usize,
    prepared: Option<PreparedSearch>,
) -> Vec<PartitionSpec> {
    let start = std::time::Instant::now();

    let single = vec![PartitionSpec {
        grid_text: grid_text.to_string(),
        seed_desc: "all".to_string(),
        seed_cells: Vec::new(),
    }];

    let grid = match Grid::parse(grid_text) {
        Ok(g) => g,
        Err(_) => return single,
    };

    let prepared = match prepared {
        Some(p) => p,
        None => match prepare_search(&grid, dict, disallow_shared_substring) {
            PrepareOutcome::Ready(p) => p,
            _ => return single,
        },
    };

    let graph = prepared.graph;
    let mut root_state = prepared.state;

    // Clear trail — not needed for partition splitting
    root_state.trail.clear();
    root_state.trail_levels.clear();

    let root_weight = partition_weight(&root_state, &grid);

    // Extract root domains; the rest of root_state becomes a shared "shell"
    // that provides prop_scratch, stats, etc. for function calls.
    // Entry domains are swapped into the shell before each call.
    let root_domains = std::mem::take(&mut root_state.domains);
    let mut shell = root_state;

    let mut heap: BinaryHeap<QueueEntry> = BinaryHeap::new();
    let mut finished: Vec<PartitionSpec> = Vec::new();
    let mut splits = 0u64;

    heap.push(QueueEntry {
        weight: root_weight,
        domains: root_domains,
        grid_text: grid_text.to_string(),
        seed_desc: String::new(),
        seed_cells: Vec::new(),
    });

    while let Some(mut entry) = heap.pop() {
        let total = heap.len() + finished.len() + 1; // +1 for the entry we just popped

        // Swap entry domains into shell for branch selection
        std::mem::swap(&mut shell.domains, &mut entry.domains);

        // Select the best crossing cell to split on
        let branch = match select_branch(&shell, &graph, dict, &grid) {
            Some(c) => c,
            None => {
                // Swap back before emitting
                std::mem::swap(&mut shell.domains, &mut entry.domains);
                emit_finished(&mut finished, entry);
                continue;
            }
        };

        // Swap parent domains back out of shell
        std::mem::swap(&mut shell.domains, &mut entry.domains);

        // Extract viable letters from bitmask
        let mut viable_letters = Vec::new();
        let mut mask = branch.viable_mask;
        while mask != 0 {
            viable_letters.push(mask.trailing_zeros() as u8);
            mask &= mask - 1;
        }
        let num_children = viable_letters.len();

        // How many new children can we afford?  total already counts the
        // entry we popped; splitting replaces it with N children.
        let budget = max_partitions.saturating_sub(total - 1);
        if budget < 2 {
            emit_finished(&mut finished, entry);
            continue;
        }

        // Group viable letters into `budget` buckets when there are more
        // viable letters than the budget allows.  Each bucket becomes one
        // child whose domain is the union of the per-letter restrictions.
        let groups: Vec<Vec<u8>> = if num_children <= budget {
            // One letter per group — the common case.
            viable_letters.iter().map(|&l| vec![l]).collect()
        } else {
            let mut gs: Vec<Vec<u8>> = (0..budget).map(|_| Vec::new()).collect();
            for (i, &l) in viable_letters.iter().enumerate() {
                gs[i % budget].push(l);
            }
            gs
        };

        splits += 1;

        // Split: create one child per group via incremental state update
        for group in &groups {
            // Build grid text and description for this group
            let (child_grid, child_desc, child_seeds) = if group.len() == 1 {
                let letter = group[0];
                let ch = (letter + b'A') as char;
                let g = set_letter_in_grid_text(&entry.grid_text, branch.row, branch.col, ch);
                let d = if entry.seed_desc.is_empty() {
                    format!("({},{})={}", branch.row, branch.col, ch)
                } else {
                    format!("{} ({},{})={}", entry.seed_desc, branch.row, branch.col, ch)
                };
                let mut seeds = entry.seed_cells.clone();
                seeds.push(SeedCell {
                    row: branch.row,
                    col: branch.col,
                    letter: ch,
                });
                (g, d, seeds)
            } else {
                let subset: String = group.iter().map(|&l| (l + b'A') as char).collect();
                let bracket = format!("[{}]", subset);
                let g = set_cell_in_grid_text(&entry.grid_text, branch.row, branch.col, &bracket);
                let d = if entry.seed_desc.is_empty() {
                    format!("({},{})={}", branch.row, branch.col, bracket)
                } else {
                    format!(
                        "{} ({},{})={}",
                        entry.seed_desc, branch.row, branch.col, bracket
                    )
                };
                // Multi-letter group: no seed_cells (not a fixed assignment)
                (g, d, entry.seed_cells.clone())
            };

            // Clone parent domains and restrict at affected slots.
            // For a single letter, AND with that letter's bitset.
            // For multiple letters, AND with the union of their bitsets.
            let mut child_domains = entry.domains.clone();
            let mut contradiction = false;

            for &(slot_id, pos_in_slot) in &branch.affected {
                let slot_len = grid.slots[slot_id].len;
                if let Some(bucket) = dict.bucket(slot_len) {
                    if group.len() == 1 {
                        child_domains[slot_id]
                            .intersect(&bucket.letter_bits[pos_in_slot][group[0] as usize]);
                    } else {
                        // Union the letter_bits for all letters in the group
                        let first = &bucket.letter_bits[pos_in_slot][group[0] as usize];
                        let mut combined = first.clone();
                        for &l in &group[1..] {
                            combined.or_with(&bucket.letter_bits[pos_in_slot][l as usize]);
                        }
                        child_domains[slot_id].intersect(&combined);
                    }
                    if child_domains[slot_id].is_empty() {
                        contradiction = true;
                        break;
                    }
                } else {
                    contradiction = true;
                    break;
                }
            }

            if contradiction {
                continue;
            }

            // Swap child domains into shell for propagation
            std::mem::swap(&mut shell.domains, &mut child_domains);

            let changed_slots: Vec<usize> = branch.affected.iter().map(|&(sid, _)| sid).collect();
            let prop_result = propagate_from_slots(&mut shell, &graph, dict, &grid, &changed_slots);

            if !prop_result {
                std::mem::swap(&mut shell.domains, &mut child_domains);
                continue;
            }

            let child_weight = partition_weight(&shell, &grid);

            // Swap child domains back out of shell
            std::mem::swap(&mut shell.domains, &mut child_domains);

            heap.push(QueueEntry {
                weight: child_weight,
                domains: child_domains,
                grid_text: child_grid,
                seed_desc: child_desc,
                seed_cells: child_seeds,
            });
        }
    }

    if finished.is_empty() {
        finished.push(PartitionSpec {
            grid_text: grid_text.to_string(),
            seed_desc: "all".to_string(),
            seed_cells: Vec::new(),
        });
    }

    let elapsed = start.elapsed();
    eprintln!(
        "[partition] {} partitions in {:.1}s ({} splits)",
        finished.len(),
        elapsed.as_secs_f64(),
        splits,
    );

    finished
}

fn emit_finished(finished: &mut Vec<PartitionSpec>, entry: QueueEntry) {
    finished.push(PartitionSpec {
        grid_text: entry.grid_text,
        seed_desc: if entry.seed_desc.is_empty() {
            "all".to_string()
        } else {
            entry.seed_desc
        },
        seed_cells: entry.seed_cells,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_utils::test_dict;

    #[test]
    fn test_generate_partitions_groups_when_budget_exceeded() {
        let dict = test_dict();
        let grid_str = "3 3\n...\n...\n...\n";

        let uncapped = generate_partitions(grid_str, &dict, 0, 1000, None);
        if uncapped.len() <= 2 {
            return;
        }

        let specs = generate_partitions(grid_str, &dict, 0, 2, None);
        assert!(
            specs.len() >= 2,
            "max_partitions=2 should produce at least 2 partitions (uncapped={}), got {}",
            uncapped.len(),
            specs.len()
        );
        assert!(
            specs.len() <= 2,
            "max_partitions=2 should produce at most 2 partitions, got {}",
            specs.len()
        );
    }
}
