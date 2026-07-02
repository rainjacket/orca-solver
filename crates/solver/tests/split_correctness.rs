//! Integration test: mid-search splitting preserves the solution set.
//!
//! Runs in CI on the committed fixtures (`dictionaries/test_small.dict` and
//! `grids/small_3x3.grid`), using the deterministic `split_after_nodes`
//! trigger instead of a wall-clock timeout.

use std::collections::HashSet;
use std::path::Path;

use orca_core::dict::Dictionary;
use orca_core::grid::Grid;
use orca_solver::{solve_grid, SearchConfig};

#[test]
fn mid_search_split_preserves_solution_set() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let grid_text = std::fs::read_to_string(root.join("grids/small_3x3.grid")).expect("read grid");
    let dict = Dictionary::load(&root.join("dictionaries/test_small.dict")).expect("load dict");
    let grid = Grid::parse(&grid_text).expect("parse grid");
    let quiet = SearchConfig {
        progress_interval: 0,
        ..SearchConfig::default()
    };

    // Baseline: exhaustive search without splitting.
    let baseline = solve_grid(&grid, &dict, &quiet, 0, None);
    assert!(baseline.exhausted);
    let baseline_set: HashSet<String> = baseline.solutions.iter().map(|(g, _)| g.clone()).collect();
    assert_eq!(baseline_set.len(), 4, "fixture has 4 known solutions");

    // Split almost immediately: the parent finishes only its current path and
    // emits the rest of the tree as sub-partitions.
    let split_config = SearchConfig {
        split_after_nodes: 2,
        ..quiet.clone()
    };
    let split = solve_grid(&grid, &dict, &split_config, 0, Some(&grid_text));
    assert!(
        !split.sub_partitions.is_empty(),
        "split must fire and produce sub-partitions"
    );

    // The parent's solutions plus every sub-partition's solutions must equal
    // the baseline set exactly.
    let mut combined: HashSet<String> = split.solutions.iter().map(|(g, _)| g.clone()).collect();
    for sp in &split.sub_partitions {
        let sp_grid = Grid::parse(&sp.grid_contents).expect("parse sub-partition");
        let sp_result = solve_grid(&sp_grid, &dict, &quiet, 0, None);
        assert!(sp_result.exhausted);
        for (g, _) in &sp_result.solutions {
            combined.insert(g.clone());
        }
    }
    assert_eq!(
        baseline_set, combined,
        "split search must find the same solutions as the baseline"
    );
}
