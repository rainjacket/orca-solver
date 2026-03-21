//! Work-stealing parallel search: generates partitions, then executes each
//! as an independent rayon task with mid-search splitting for load balancing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use orca_core::dict::Dictionary;
use orca_core::grid::Grid;

use crate::partition::{generate_partitions, PartitionSpec};
use crate::search::{solve_grid, SearchConfig};
use crate::stats::SolverStats;

/// Parallel search result containing solutions and merged stats.
pub struct ParallelResult {
    /// Filled grids as (grid_text, word_list) pairs.
    pub solutions: Vec<(String, Vec<String>)>,
    /// Merged stats from all threads.
    pub stats: SolverStats,
    /// Whether search was exhaustive or stopped early.
    pub exhausted: bool,
}

/// Shared progress state for multi-threaded search, readable by a display thread.
pub struct ParallelProgress {
    /// Per-thread description of the current partition (indexed by rayon thread index).
    pub thread_descriptions: Vec<Mutex<String>>,
    /// Number of partitions completed so far.
    pub completed_partitions: AtomicU64,
    /// Total partitions (increases when splits create sub-partitions).
    pub total_partitions: AtomicU64,
    /// Cumulative nodes across all completed partitions.
    pub total_nodes: AtomicU64,
    /// Cumulative solutions found.
    pub total_solutions: AtomicU64,
    /// Whether the search is still running.
    pub running: AtomicBool,
}

impl ParallelProgress {
    pub fn new(num_threads: usize) -> Self {
        ParallelProgress {
            thread_descriptions: (0..num_threads)
                .map(|_| Mutex::new(String::new()))
                .collect(),
            completed_partitions: AtomicU64::new(0),
            total_partitions: AtomicU64::new(0),
            total_nodes: AtomicU64::new(0),
            total_solutions: AtomicU64::new(0),
            running: AtomicBool::new(true),
        }
    }
}

/// Run parallel search with partition splitting.
///
/// Self-contained: parses the grid, generates partitions, then executes
/// each partition as an independent rayon work item.
pub fn solve_parallel(
    grid_text: &str,
    dict: &Dictionary,
    config: &SearchConfig,
    num_threads: usize,
    disallow_shared_substring: usize,
) -> ParallelResult {
    solve_parallel_inner(grid_text, dict, config, num_threads, disallow_shared_substring, None)
}

/// Like `solve_parallel`, but with shared progress state for a display thread.
pub fn solve_parallel_with_progress(
    grid_text: &str,
    dict: &Dictionary,
    config: &SearchConfig,
    num_threads: usize,
    disallow_shared_substring: usize,
    progress: Arc<ParallelProgress>,
) -> ParallelResult {
    solve_parallel_inner(
        grid_text,
        dict,
        config,
        num_threads,
        disallow_shared_substring,
        Some(progress),
    )
}

fn solve_parallel_inner(
    grid_text: &str,
    dict: &Dictionary,
    config: &SearchConfig,
    num_threads: usize,
    disallow_shared_substring: usize,
    progress: Option<Arc<ParallelProgress>>,
) -> ParallelResult {
    let mut stats = SolverStats::new();
    stats.start();

    if let Err(e) = Grid::parse(grid_text) {
        eprintln!("[parallel] Failed to parse grid: {}", e);
        return ParallelResult {
            solutions: vec![],
            stats,
            exhausted: true,
        };
    }

    // Generate partitions.
    // For local parallel, cap at ~16x threads — enough for good load balancing
    // without excessive partition generation overhead.
    let max_partitions = num_threads * 16;
    let partition_specs = generate_partitions(
        grid_text,
        dict,
        disallow_shared_substring,
        max_partitions,
        None,
    );

    let initial_count = partition_specs.len();

    // Initialize progress tracking if provided
    if let Some(ref p) = progress {
        p.total_partitions
            .store(initial_count as u64, Ordering::Relaxed);
    }

    // Shared work queue with condvar for work-stealing
    let queue: Arc<(Mutex<VecDeque<PartitionSpec>>, Condvar)> =
        Arc::new((Mutex::new(VecDeque::from(partition_specs)), Condvar::new()));
    let active_workers = Arc::new(AtomicU64::new(0));
    let total_solutions = Arc::new(AtomicU64::new(0));
    let total_nodes = Arc::new(AtomicU64::new(0));


    // Collected results from all threads
    let results: Arc<Mutex<Vec<(Vec<(String, Vec<String>)>, SolverStats, bool)>>> =
        Arc::new(Mutex::new(Vec::new()));

    // Configure rayon thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .expect("Failed to create rayon thread pool");

    let split_timeout = config.split_timeout_secs;

    pool.scope(|s| {
        for _ in 0..num_threads {
            let queue = Arc::clone(&queue);
            let active_workers = Arc::clone(&active_workers);
            let total_solutions = Arc::clone(&total_solutions);
            let total_nodes = Arc::clone(&total_nodes);

            let results = Arc::clone(&results);
            let progress = progress.clone();
            let config = SearchConfig {
                max_solutions: config.max_solutions,
                progress_interval: 0,
                symmetry_break_cells: config.symmetry_break_cells.clone(),
                split_timeout_secs: split_timeout,
            };

            s.spawn(move |_| {
                loop {
                    // Try to get work from the queue
                    let spec = {
                        let (lock, cvar) = &*queue;
                        let mut q = lock.lock().unwrap();
                        loop {
                            if let Some(spec) = q.pop_front() {
                                active_workers.fetch_add(1, Ordering::SeqCst);
                                break Some(spec);
                            }
                            // No work available — are all workers idle?
                            if active_workers.load(Ordering::SeqCst) == 0 {
                                // No work and nobody producing more — we're done
                                cvar.notify_all();
                                break None;
                            }
                            // Wait for new work or termination
                            q = cvar.wait(q).unwrap();
                        }
                    };

                    let spec = match spec {
                        Some(s) => s,
                        None => {
                            // Clear this thread's description when done
                            if let Some(ref p) = progress {
                                let thread_idx = rayon::current_thread_index().unwrap_or(0);
                                if thread_idx < p.thread_descriptions.len() {
                                    p.thread_descriptions[thread_idx]
                                        .lock()
                                        .unwrap()
                                        .clear();
                                }
                            }
                            return;
                        }
                    };

                    // Check if we've found enough solutions
                    if config.max_solutions > 0
                        && total_solutions.load(Ordering::Relaxed) >= config.max_solutions
                    {
                        active_workers.fetch_sub(1, Ordering::SeqCst);
                        let (_, cvar) = &*queue;
                        cvar.notify_all();
                        continue;
                    }

                    let part_grid = match Grid::parse(&spec.grid_text) {
                        Ok(g) => g,
                        Err(_) => {
                            active_workers.fetch_sub(1, Ordering::SeqCst);
                            let (_, cvar) = &*queue;
                            cvar.notify_all();
                            continue;
                        }
                    };

                    // Update thread description for display
                    if let Some(ref p) = progress {
                        let thread_idx = rayon::current_thread_index().unwrap_or(0);
                        if thread_idx < p.thread_descriptions.len() {
                            *p.thread_descriptions[thread_idx].lock().unwrap() =
                                spec.seed_desc.clone();
                        }
                    }

                    let grid_text_for_split = if split_timeout > 0 {
                        Some(spec.grid_text.as_str())
                    } else {
                        None
                    };

                    let result = solve_grid(
                        &part_grid,
                        dict,
                        &config,
                        disallow_shared_substring,
                        grid_text_for_split,
                    );

                    // Feed sub-partitions back into the queue
                    if !result.sub_partitions.is_empty() {
                        let count = result.sub_partitions.len();
                        let (lock, cvar) = &*queue;
                        let mut q = lock.lock().unwrap();
                        for sub in result.sub_partitions {
                            q.push_back(PartitionSpec {
                                grid_text: sub.grid_contents,
                                seed_desc: sub.seed_desc,
                                seed_cells: vec![],
                            });
                        }
                        drop(q);

                        if let Some(ref p) = progress {
                            p.total_partitions
                                .fetch_add(count as u64, Ordering::Relaxed);
                        }
                        cvar.notify_all();
                    }

                    let sol_count = result.solutions.len() as u64;
                    if sol_count > 0 {
                        total_solutions.fetch_add(sol_count, Ordering::Relaxed);
                    }

                    let nodes = result.stats.nodes;
                    total_nodes.fetch_add(nodes, Ordering::Relaxed);

                    // Update progress counters for display thread
                    if let Some(ref p) = progress {
                        p.completed_partitions.fetch_add(1, Ordering::Relaxed);
                        p.total_nodes.fetch_add(nodes, Ordering::Relaxed);
                        p.total_solutions.fetch_add(sol_count, Ordering::Relaxed);
                    }

                    results.lock().unwrap().push((
                        result.solutions,
                        result.stats,
                        result.exhausted,
                    ));

                    // Mark done, wake waiters
                    active_workers.fetch_sub(1, Ordering::SeqCst);
                    let (_, cvar) = &*queue;
                    cvar.notify_all();
                }
            });
        }
    });

    // Signal completion
    if let Some(ref p) = progress {
        p.running.store(false, Ordering::SeqCst);
    }

    // Merge results
    let results = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
    let mut all_solutions = Vec::new();
    let mut merged_stats = SolverStats::new();
    merged_stats.start_time = stats.start_time;
    let mut all_exhausted = true;

    for (solutions, part_stats, exhausted) in results {
        all_solutions.extend(solutions);
        merged_stats.merge(&part_stats);
        if !exhausted {
            all_exhausted = false;
        }
    }

    ParallelResult {
        solutions: all_solutions,
        stats: merged_stats,
        exhausted: all_exhausted,
    }
}
