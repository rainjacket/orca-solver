//! Constraint graph: per-slot adjacency lists built from grid crossings.
//! Crossings are sorted by length-sum (descending) for the bounded
//! crossing scan in the solver's branching heuristic.

use orca_core::grid::{Crossing, Grid};

/// The constraint graph: crossings between slots and per-slot adjacency lists.
#[derive(Debug, Clone)]
pub struct ConstraintGraph {
    /// All crossing pairs.
    pub crossings: Vec<Crossing>,
    /// Per-slot adjacency list: `neighbors[slot_id]` = list of (crossing_idx, is_slot_a).
    /// `is_slot_a` indicates this slot's role in the crossing — pass it to
    /// `crossing_info()` to get the *other* slot's id and positions.
    pub neighbors: Vec<Vec<(usize, bool)>>,
}

impl ConstraintGraph {
    /// Build a constraint graph from a parsed grid.
    pub fn from_grid(grid: &Grid) -> Self {
        let num_slots = grid.slots.len();
        let mut crossings = grid.crossings.clone();

        // Sort crossings by length-sum (descending): sum of the lengths of
        // both participating slots. (On valid crossword grids, crossing_count
        // == slot.len for all constrained slots, so this is equivalent to
        // sorting by sum of crossing counts.) Longer-slot crossings are
        // evaluated first by the bounded scan in find_best_crossing().
        let slot_len = grid.slots.iter().map(|s| s.len).collect::<Vec<_>>();
        crossings.sort_by(|a, b| {
            let score_a = slot_len[a.slot_a] + slot_len[a.slot_b];
            let score_b = slot_len[b.slot_a] + slot_len[b.slot_b];
            score_b.cmp(&score_a) // descending
        });

        let mut neighbors: Vec<Vec<(usize, bool)>> = vec![Vec::new(); num_slots];

        for (crossing_idx, crossing) in crossings.iter().enumerate() {
            neighbors[crossing.slot_a].push((crossing_idx, true));
            neighbors[crossing.slot_b].push((crossing_idx, false));
        }

        ConstraintGraph {
            crossings,
            neighbors,
        }
    }

    /// Get the neighbor slot and positions for a given slot through a crossing.
    /// Returns (other_slot_id, pos_in_this_slot, pos_in_other_slot).
    #[inline]
    pub fn crossing_info(&self, crossing_idx: usize, is_slot_a: bool) -> (usize, usize, usize) {
        let c = &self.crossings[crossing_idx];
        if is_slot_a {
            (c.slot_b, c.pos_in_a, c.pos_in_b)
        } else {
            (c.slot_a, c.pos_in_b, c.pos_in_a)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orca_core::grid::Grid;

    #[test]
    fn test_constraint_graph_3x3() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        let cg = ConstraintGraph::from_grid(&grid);

        assert_eq!(cg.neighbors.len(), 6); // 3 across + 3 down
        assert_eq!(cg.crossings.len(), 9); // 9 crossing cells

        // Each slot crosses 3 others (3 across × 3 down)
        for slot_id in 0..6 {
            assert_eq!(cg.neighbors[slot_id].len(), 3);
        }
    }

    #[test]
    fn test_crossing_info() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        let cg = ConstraintGraph::from_grid(&grid);

        // Check that crossing_info returns consistent data
        for (idx, crossing) in cg.crossings.iter().enumerate() {
            let (other_b, pos_a, pos_b) = cg.crossing_info(idx, true);
            assert_eq!(other_b, crossing.slot_b);
            assert_eq!(pos_a, crossing.pos_in_a);
            assert_eq!(pos_b, crossing.pos_in_b);

            let (other_a, pos_b2, pos_a2) = cg.crossing_info(idx, false);
            assert_eq!(other_a, crossing.slot_a);
            assert_eq!(pos_b2, crossing.pos_in_b);
            assert_eq!(pos_a2, crossing.pos_in_a);
        }
    }
}
