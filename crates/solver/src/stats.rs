//! Performance counters for the solver (nodes, backtracks, propagations, etc.).

use std::fmt;
use std::time::Instant;

/// Performance counters for the solver.
#[derive(Debug, Clone)]
pub struct SolverStats {
    pub nodes: u64,
    pub backtracks: u64,
    pub propagations: u64,
    pub wipeouts: u64,
    pub solutions: u64,
    pub dupes_skipped: u64,
    pub start_time: Option<Instant>,
}

impl SolverStats {
    pub fn new() -> Self {
        SolverStats {
            nodes: 0,
            backtracks: 0,
            propagations: 0,
            wipeouts: 0,
            solutions: 0,
            dupes_skipped: 0,
            start_time: None,
        }
    }

    pub fn start(&mut self) {
        self.start_time = Some(Instant::now());
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.start_time
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    /// Merge counters from another stats instance into this one.
    /// Does not modify start_time.
    pub fn merge(&mut self, other: &SolverStats) {
        self.nodes += other.nodes;
        self.backtracks += other.backtracks;
        self.propagations += other.propagations;
        self.wipeouts += other.wipeouts;
        self.solutions += other.solutions;
        self.dupes_skipped += other.dupes_skipped;
    }

    pub fn nodes_per_sec(&self) -> f64 {
        let elapsed = self.elapsed_secs();
        if elapsed > 0.0 {
            self.nodes as f64 / elapsed
        } else {
            0.0
        }
    }
}

impl Default for SolverStats {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SolverStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "nodes={} bt={} prop={} wipe={} dupe={} sol={} {:.1}s ({:.0} n/s)",
            self.nodes,
            self.backtracks,
            self.propagations,
            self.wipeouts,
            self.dupes_skipped,
            self.solutions,
            self.elapsed_secs(),
            self.nodes_per_sec(),
        )
    }
}
