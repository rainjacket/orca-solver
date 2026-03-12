use std::path::PathBuf;
use std::process;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use orca_core::dict::Dictionary;
use orca_core::grid::Grid;
use orca_solver::{resolve_cell, solve_grid, solve_parallel, SearchConfig};

#[derive(Parser)]
#[command(
    name = "orca",
    about = "High-performance crossword grid filler",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fill a crossword grid with words from a dictionary.
    #[command(after_help = "\
Examples:
  orca fill grid.grid words.dict              Find all solutions
  orca fill grid.grid words.dict -n 1         Find first solution only
  orca fill grid.grid words.dict -j 4         Use 4 threads
  orca fill grid.grid words.dict -n 0 --disallow-shared-substring 0
                                              Exhaustive, no substring constraint")]
    Fill {
        /// Path to the .grid file.
        grid: PathBuf,

        /// Path to the .dict file.
        dict: PathBuf,

        /// Maximum number of solutions to find (0 = unlimited).
        #[arg(short = 'n', long, default_value = "0")]
        max_solutions: u64,

        /// Report progress every N nodes (0 = disabled).
        #[arg(long, default_value = "10000")]
        progress_interval: u64,

        /// Disallow shared substrings of this length or longer between entries.
        /// E.g., 6 means no two entries can share a 6+ letter substring.
        /// 0 disables the constraint but still prevents exact duplicate words.
        #[arg(long, default_value = "6")]
        disallow_shared_substring: usize,

        /// Symmetry breaking: "r1,c1,r2,c2" enforces letter(r1,c1) <= letter(r2,c2)
        /// during propagation. Picks two cells that are symmetric mirrors;
        /// halves the search space by pruning transpose-equivalent fills.
        #[arg(long)]
        symmetry_break: Option<String>,

        /// Number of threads for parallel search (1 = sequential, >1 = parallel).
        #[arg(short = 'j', long, default_value = "1")]
        threads: usize,

        /// Mid-search split timeout in seconds for parallel search. When a partition
        /// runs longer than this, remaining work is split into sub-partitions and
        /// redistributed to other threads. 0 = disabled. Default: 3s when parallel.
        #[arg(long)]
        split_timeout: Option<u64>,
    },

    /// Print information about a grid and dictionary.
    #[command(after_help = "\
Examples:
  orca info grid.grid words.dict")]
    Info {
        /// Path to the .grid file.
        grid: PathBuf,

        /// Path to the .dict file.
        dict: PathBuf,
    },

    /// Validate a dictionary file format.
    #[command(after_help = "\
Examples:
  orca validate-dict words.dict")]
    ValidateDict {
        /// Path to the .dict file.
        dict: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Fill {
            grid,
            dict,
            max_solutions,
            progress_interval,
            disallow_shared_substring,
            symmetry_break,
            threads,
            split_timeout,
        } => cmd_fill(
            grid,
            dict,
            max_solutions,
            progress_interval,
            disallow_shared_substring,
            symmetry_break,
            threads,
            split_timeout,
        ),
        Commands::Info { grid, dict } => cmd_info(grid, dict),
        Commands::ValidateDict { dict } => cmd_validate_dict(dict),
    };

    if let Err(e) = result {
        eprintln!("Error: {:#}", e);
        process::exit(1);
    }
}

/// Parse "r1,c1,r2,c2" into four coordinates.
fn parse_cell_pair(s: &str) -> Result<(usize, usize, usize, usize)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        bail!(
            "Expected format 'r1,c1,r2,c2' for symmetry-break, got {:?}",
            s
        );
    }
    Ok((
        parts[0].trim().parse().context("Invalid row")?,
        parts[1].trim().parse().context("Invalid col")?,
        parts[2].trim().parse().context("Invalid row")?,
        parts[3].trim().parse().context("Invalid col")?,
    ))
}

fn parse_symmetry_break(
    s: &str,
    grid: &Grid,
) -> Result<(orca_solver::CellSymInfo, orca_solver::CellSymInfo)> {
    let (r1, c1, r2, c2) = parse_cell_pair(s)?;
    let cell_a = resolve_cell(grid, r1, c1)
        .with_context(|| format!("Cell ({},{}) not in any constrained slot", r1, c1))?;
    let cell_b = resolve_cell(grid, r2, c2)
        .with_context(|| format!("Cell ({},{}) not in any constrained slot", r2, c2))?;
    Ok((cell_a, cell_b))
}

fn cmd_fill(
    grid_path: PathBuf,
    dict_path: PathBuf,
    max_solutions: u64,
    progress_interval: u64,
    disallow_shared_substring: usize,
    symmetry_break: Option<String>,
    threads: usize,
    split_timeout: Option<u64>,
) -> Result<()> {
    let grid_text = std::fs::read_to_string(&grid_path).context("Failed to read grid file")?;
    let grid = Grid::parse(&grid_text).context("Failed to parse grid")?;
    let dict = Dictionary::load(&dict_path).context("Failed to load dictionary")?;

    let symmetry_break_cells = symmetry_break
        .as_deref()
        .map(|s| parse_symmetry_break(s, &grid))
        .transpose()?;

    eprintln!(
        "Grid: {}x{}, {} slots, {} crossings",
        grid.rows,
        grid.cols,
        grid.slots.len(),
        grid.crossings.len()
    );
    eprintln!("Dictionary: {} words", dict.total_words());
    if disallow_shared_substring > 0 {
        eprintln!(
            "Shared substring constraint: no two entries share a {}+ letter substring",
            disallow_shared_substring
        );
    }
    if let Some((ref ca, ref cb)) = symmetry_break_cells {
        eprintln!(
            "Symmetry breaking: cell at slot {}[{}] <= cell at slot {}[{}]",
            ca.slot_id, ca.pos_in_slot, cb.slot_id, cb.pos_in_slot
        );
    }

    // Check that all slot lengths have dictionary entries
    for (i, slot) in grid.slots.iter().enumerate() {
        if slot.constrained && dict.bucket(slot.len).is_none() {
            eprintln!(
                "Warning: no dictionary words of length {} for slot {} ({} at {:?})",
                slot.len, i, slot.direction, slot.start
            );
        }
    }

    // Default split timeout: 3s for parallel, 0 for sequential
    let split_timeout_secs = split_timeout.unwrap_or(if threads > 1 { 3 } else { 0 });

    let config = SearchConfig {
        max_solutions,
        progress_interval,
        symmetry_break_cells,
        split_timeout_secs,
    };

    let (solutions, stats, exhausted) = if threads > 1 {
        eprintln!("Using {} threads for parallel search", threads);
        let r = solve_parallel(
            &grid_text,
            &dict,
            &config,
            threads,
            disallow_shared_substring,
        );
        (r.solutions, r.stats, r.exhausted)
    } else {
        let r = solve_grid(&grid, &dict, &config, disallow_shared_substring, None);
        (r.solutions, r.stats, r.exhausted)
    };

    for (i, (grid_text, _words)) in solutions.iter().enumerate() {
        println!("--- Solution {} ---", i + 1);
        println!("{}", grid_text);
        println!();
    }

    if exhausted {
        eprintln!("Search exhausted. Total solutions: {}", stats.solutions);
    } else {
        eprintln!("Stopped after {} solutions (max reached).", stats.solutions);
    }
    eprintln!("Final stats: {}", stats);

    Ok(())
}

fn cmd_info(grid_path: PathBuf, dict_path: PathBuf) -> Result<()> {
    let grid = Grid::load(&grid_path).context("Failed to load grid")?;
    let dict = Dictionary::load(&dict_path).context("Failed to load dictionary")?;

    println!("Grid: {}x{}", grid.rows, grid.cols);
    println!("{}", grid);

    // Count crossings per slot from the flat crossings list
    let mut crossings_per_slot = vec![0usize; grid.slots.len()];
    for c in &grid.crossings {
        crossings_per_slot[c.slot_a] += 1;
        crossings_per_slot[c.slot_b] += 1;
    }

    println!("Slots: {}", grid.slots.len());
    for (i, slot) in grid.slots.iter().enumerate() {
        let candidates = dict
            .bucket(slot.len)
            .map(|b| b.candidates(&slot.pattern).map_or(0, |c| c.count_ones()))
            .unwrap_or(0);
        let flags = if slot.check_only {
            " check_only"
        } else if !slot.constrained {
            " unconstrained"
        } else {
            ""
        };
        println!(
            "  Slot {} {} len={} at ({},{}) crossings={} candidates={}{}",
            i,
            slot.direction,
            slot.len,
            slot.start.0,
            slot.start.1,
            crossings_per_slot[i],
            candidates,
            flags,
        );
    }

    println!("\nCrossings: {}", grid.crossings.len());

    println!("\nDictionary: {} total words", dict.total_words());
    for len in dict.lengths() {
        let bucket = dict.bucket(len).unwrap();
        println!("  Length {}: {} words", len, bucket.words.len());
    }

    Ok(())
}

fn cmd_validate_dict(dict_path: PathBuf) -> Result<()> {
    let issues = Dictionary::validate(&dict_path)?;

    if issues.is_empty() {
        println!("Dictionary is valid.");
    } else {
        println!("Found {} issues:", issues.len());
        for issue in &issues {
            println!("  {}", issue);
        }
    }

    Ok(())
}
