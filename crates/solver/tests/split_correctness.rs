//! Integration test: verify that mid-search splitting produces correct results.
//! Compares split search (parent + sub-partitions) against a no-split baseline
//! on the 7x7 benchmark grid with the full dictionary.
//!
//! Ignored in debug builds — too slow without optimizations to be meaningful.

use std::collections::HashSet;
use std::path::Path;

use orca_core::dict::Dictionary;
use orca_core::grid::Grid;
use orca_solver::{solve_grid, SearchConfig};

#[test]
#[cfg_attr(debug_assertions, ignore)]
fn mid_search_split_correctness() {
    let dict_path = Path::new("../../dictionaries/wl.dict");
    let grid_path = Path::new("../../grids/bench_7x7.grid");
    if !dict_path.exists() || !grid_path.exists() {
        eprintln!("Skipping: test files not found");
        return;
    }
    let grid_text = std::fs::read_to_string(grid_path).expect("read grid");
    let dict = Dictionary::load(dict_path).expect("load dict");
    let grid = Grid::parse(&grid_text).expect("parse grid");
    let nosplit = SearchConfig {
        progress_interval: 0,
        ..SearchConfig::default()
    };

    // Baseline: exhaustive search without splitting
    let baseline = solve_grid(&grid, &dict, &nosplit, 0, None);
    assert!(baseline.exhausted);
    let baseline_solutions: HashSet<String> =
        baseline.solutions.iter().map(|(g, _)| g.clone()).collect();
    eprintln!(
        "Baseline: {} solutions, {} nodes",
        baseline_solutions.len(),
        baseline.stats.nodes,
    );

    // Split search: 1s timeout triggers split early
    let split_config = SearchConfig {
        split_timeout_secs: 1,
        progress_interval: 0,
        ..SearchConfig::default()
    };
    let split_result = solve_grid(&grid, &dict, &split_config, 0, Some(&grid_text));

    // Split should fire and produce sub-partitions
    assert!(
        !split_result.sub_partitions.is_empty(),
        "Expected sub-partitions with 1s timeout",
    );

    // Parent should explore far fewer nodes than baseline (split shed the work)
    assert!(
        split_result.stats.nodes < baseline.stats.nodes / 2,
        "Parent should explore <50% of baseline nodes ({} vs {})",
        split_result.stats.nodes,
        baseline.stats.nodes,
    );

    // Post-split branch points should emit deep-split sub-partitions
    // instead of descending into new subtrees
    let deep_splits = split_result
        .sub_partitions
        .iter()
        .filter(|p| p.seed_desc.contains("[deep-split]"))
        .count();
    assert!(deep_splits > 0, "Expected [deep-split] sub-partitions");

    // Correctness: solve each sub-partition and verify combined solutions match
    let mut all_solutions: HashSet<String> = split_result
        .solutions
        .iter()
        .map(|(g, _)| g.clone())
        .collect();
    for sp in &split_result.sub_partitions {
        let sp_grid = Grid::parse(&sp.grid_contents).expect("parse sub-partition");
        let sp_result = solve_grid(&sp_grid, &dict, &nosplit, 0, None);
        for (g, _) in &sp_result.solutions {
            all_solutions.insert(g.clone());
        }
    }
    assert_eq!(
        baseline_solutions,
        all_solutions,
        "Split search must find same solutions as baseline ({} vs {})",
        baseline_solutions.len(),
        all_solutions.len(),
    );
}
