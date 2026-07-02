//! The Orca search engine: AC-3 constraint propagation with cell-level
//! branching, exhaustive by construction, plus partition generation and
//! work-stealing parallel search. Entry points: [`solve_grid`] and
//! [`solve_parallel`].

mod constraint;
mod parallel;
mod partition;
mod propagate;
mod search;
mod state;
mod stats;
#[cfg(test)]
mod test_utils;

pub use parallel::{
    solve_parallel, solve_parallel_with_progress, ParallelProgress, ParallelResult,
};
pub use partition::{generate_partitions, PartitionSpec, SeedCell, DEFAULT_MAX_PARTITIONS};
pub use search::{
    prepare_search, resolve_cell, solve_grid, CellSymInfo, PartitionResult, PrepareOutcome,
    PreparedSearch, SearchConfig, SubPartition,
};
pub use stats::SolverStats;
