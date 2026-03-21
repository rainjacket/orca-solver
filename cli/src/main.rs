mod html_browser;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{cursor, execute, terminal};

use orca_core::dict::Dictionary;
use orca_core::grid::Grid;
use orca_solver::{
    resolve_cell, solve_grid, solve_parallel, solve_parallel_with_progress, ParallelProgress,
    SearchConfig,
};

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
    ///
    /// Run without arguments to enter interactive mode.
    #[command(after_help = "\
Examples:
  orca fill                                       Interactive mode
  orca fill grid.txt words.dict                   Find all solutions
  orca fill grid.grid words.dict -n 1             Find first solution only
  orca fill grid.grid words.dict -j 4             Use 4 threads
  orca fill grid.grid words.dict -n 0 --disallow-shared-substring 0
                                                  Exhaustive, no substring constraint")]
    Fill {
        /// Path to the grid file (.grid or .txt format).
        grid: Option<PathBuf>,

        /// Path to the dictionary/wordlist file (.dict format: WORD;SCORE per line).
        dict: Option<PathBuf>,

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
        /// Path to the grid file (.grid or .txt format).
        grid: PathBuf,

        /// Path to the dictionary/wordlist file.
        dict: PathBuf,
    },

    /// Validate a dictionary file format.
    #[command(after_help = "\
Examples:
  orca validate-dict words.dict")]
    ValidateDict {
        /// Path to the dictionary/wordlist file.
        dict: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Fill {
            grid: None, dict: _, ..
        } => interactive_fill(),
        Commands::Fill {
            grid: Some(grid_path),
            dict,
            max_solutions,
            progress_interval,
            disallow_shared_substring,
            symmetry_break,
            threads,
            split_timeout,
        } => {
            let dict_path = match dict {
                Some(d) => d,
                None => {
                    eprintln!("Error: dictionary path is required when grid path is provided.");
                    eprintln!("Run `orca fill` without arguments for interactive mode.");
                    process::exit(1);
                }
            };
            cmd_fill(
                grid_path,
                dict_path,
                max_solutions,
                progress_interval,
                disallow_shared_substring,
                symmetry_break,
                threads,
                split_timeout,
            )
        }
        Commands::Info { grid, dict } => cmd_info(grid, dict),
        Commands::ValidateDict { dict } => cmd_validate_dict(dict),
    };

    if let Err(e) = result {
        eprintln!("Error: {:#}", e);
        process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Interactive mode
// ---------------------------------------------------------------------------

fn interactive_fill() -> Result<()> {
    use dialoguer::{Confirm, Input};

    eprintln!("Orca Interactive Mode");
    eprintln!("=====================\n");

    // 1. Grid file — load eagerly to validate and inform later prompts
    let grid_path: String = Input::new()
        .with_prompt("Grid file path (.grid or .txt)")
        .interact_text()?;
    let grid_path = expand_path(grid_path.trim());

    let grid_text = std::fs::read_to_string(&grid_path).context("Failed to read grid file")?;
    let grid = Grid::parse(&grid_text).context("Failed to parse grid")?;
    eprintln!(
        "  Loaded: {}x{}, {} slots, {} crossings\n",
        grid.rows,
        grid.cols,
        grid.slots.len(),
        grid.crossings.len()
    );

    // 2. Dictionary — load eagerly to validate and show stats
    let dict_path: String = Input::new()
        .with_prompt("Dictionary/wordlist file path")
        .interact_text()?;
    let dict_path = expand_path(dict_path.trim());

    let dict = Dictionary::load(&dict_path).context("Failed to load dictionary")?;
    eprintln!("  Loaded: {} words\n", dict.total_words());

    // 3. Threads
    let threads: usize = Input::new()
        .with_prompt("Number of threads (1 = sequential)")
        .default(1)
        .interact_text()?;

    // 4. Max solutions
    let max_solutions: u64 = Input::new()
        .with_prompt("Max solutions (0 = unlimited)")
        .default(0u64)
        .interact_text()?;

    // 5. Shared substring constraint
    let disallow_shared_substring: usize = Input::new()
        .with_prompt("Disallow shared substring length (0 = disabled)")
        .default(6usize)
        .interact_text()?;

    // 6. Symmetry break (only if grid has diagonal symmetry)
    let symmetry_break_cells = if grid.has_diagonal_symmetry() {
        let use_sym = Confirm::new()
            .with_prompt(
                "Grid has diagonal symmetry. Enable symmetry breaking? (halves search space)",
            )
            .default(true)
            .interact()?;
        if use_sym {
            auto_detect_symmetry_break(&grid)
                .map(|s| parse_symmetry_break(&s, &grid))
                .transpose()?
        } else {
            None
        }
    } else {
        None
    };

    eprintln!();

    let split_timeout_secs = if threads > 1 { 3 } else { 0 };

    let config = SearchConfig {
        max_solutions,
        progress_interval: 0,
        symmetry_break_cells,
        split_timeout_secs,
    };

    print_fill_header(&grid, &dict, &config, disallow_shared_substring, threads);
    run_fill(&grid_text, &grid, &dict, &config, threads, disallow_shared_substring)
}

/// Expand `~` at the start of a path to the user's home directory.
fn expand_path(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

/// Find the first pair of diagonally-mirrored cells suitable for symmetry breaking.
fn auto_detect_symmetry_break(grid: &Grid) -> Option<String> {
    for r in 0..grid.rows {
        for c in (r + 1)..grid.cols {
            if grid.cells[r][c].is_constrained()
                && grid.cells[c][r].is_constrained()
                && resolve_cell(grid, r, c).is_some()
                && resolve_cell(grid, c, r).is_some()
            {
                return Some(format!("{},{},{},{}", r, c, c, r));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Fill command
// ---------------------------------------------------------------------------

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

/// CLI entry point: load files from paths, then delegate to `run_fill`.
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

    let split_timeout_secs = split_timeout.unwrap_or(if threads > 1 { 3 } else { 0 });

    let config = SearchConfig {
        max_solutions,
        progress_interval,
        symmetry_break_cells,
        split_timeout_secs,
    };

    print_fill_header(&grid, &dict, &config, disallow_shared_substring, threads);
    run_fill(&grid_text, &grid, &dict, &config, threads, disallow_shared_substring)
}

/// Print grid/dict summary and warnings before starting a fill.
fn print_fill_header(
    grid: &Grid,
    dict: &Dictionary,
    config: &SearchConfig,
    disallow_shared_substring: usize,
    threads: usize,
) {
    eprintln!(
        "Grid: {}x{}, {} slots, {} crossings",
        grid.rows, grid.cols, grid.slots.len(), grid.crossings.len()
    );
    eprintln!("Dictionary: {} words", dict.total_words());
    if disallow_shared_substring > 0 {
        eprintln!(
            "Shared substring constraint: no two entries share a {}+ letter substring",
            disallow_shared_substring
        );
    }
    if let Some((ref ca, ref cb)) = config.symmetry_break_cells {
        eprintln!(
            "Symmetry breaking: cell at slot {}[{}] <= cell at slot {}[{}]",
            ca.slot_id, ca.pos_in_slot, cb.slot_id, cb.pos_in_slot
        );
    }
    for (i, slot) in grid.slots.iter().enumerate() {
        if slot.constrained && dict.bucket(slot.len).is_none() {
            eprintln!(
                "Warning: no dictionary words of length {} for slot {} ({} at {:?})",
                slot.len, i, slot.direction, slot.start
            );
        }
    }
    if threads > 1 {
        eprintln!("Using {} threads for parallel search", threads);
    }
}

/// Core fill logic shared by `cmd_fill` and `interactive_fill`.
fn run_fill(
    grid_text: &str,
    grid: &Grid,
    dict: &Dictionary,
    config: &SearchConfig,
    threads: usize,
    disallow_shared_substring: usize,
) -> Result<()> {
    let is_tty = std::io::stderr().is_terminal();

    // Disable legacy per-node progress lines when using live display
    let config = if is_tty && config.progress_interval > 0 {
        SearchConfig {
            progress_interval: 0,
            ..config.clone()
        }
    } else {
        config.clone()
    };

    let (solutions, stats, exhausted) = if threads > 1 && is_tty {
        run_parallel_with_display(grid_text, dict, &config, threads, disallow_shared_substring)
    } else if threads > 1 {
        let r = solve_parallel(grid_text, dict, &config, threads, disallow_shared_substring);
        (r.solutions, r.stats, r.exhausted)
    } else {
        let r = solve_grid(grid, dict, &config, disallow_shared_substring, None);
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

    // Generate HTML browser if we have solutions
    if !solutions.is_empty() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let html_path = PathBuf::from(format!("solutions_{}.html", timestamp));
        html_browser::generate_html_browser(&solutions, grid.rows, grid.cols, &html_path)?;
        eprintln!("Solution browser: {}", html_path.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Live progress display
// ---------------------------------------------------------------------------

/// Format elapsed seconds as a human-readable duration.
fn format_elapsed(secs: f64) -> String {
    if secs < 60.0 {
        format!("{:.1}s", secs)
    } else if secs < 3600.0 {
        format!("{}m{:02}s", secs as u64 / 60, secs as u64 % 60)
    } else {
        format!(
            "{}h{:02}m{:02}s",
            secs as u64 / 3600,
            (secs as u64 % 3600) / 60,
            secs as u64 % 60
        )
    }
}

/// Format a count for display (1234567 -> "1.2M", 45678 -> "45.7K").
fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Clear N lines of previously printed terminal output.
fn clear_display(stderr: &mut std::io::Stderr, lines: u16) {
    if lines > 0 {
        execute!(
            stderr,
            cursor::MoveUp(lines),
            terminal::Clear(terminal::ClearType::FromCursorDown)
        )
        .ok();
    }
}

/// Run parallel search with live terminal display (progress bar + per-thread info).
fn run_parallel_with_display(
    grid_text: &str,
    dict: &Dictionary,
    config: &SearchConfig,
    num_threads: usize,
    disallow_shared_substring: usize,
) -> (Vec<(String, Vec<String>)>, orca_solver::SolverStats, bool) {
    let progress = Arc::new(ParallelProgress::new(num_threads));

    // Spawn display thread
    let display_progress = Arc::clone(&progress);
    let display_handle = std::thread::spawn(move || {
        let mut stderr = std::io::stderr();
        let mut lines_printed = 0u16;
        let start = std::time::Instant::now();

        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));

            if !display_progress.running.load(Ordering::SeqCst) {
                clear_display(&mut stderr, lines_printed);
                break;
            }

            let completed = display_progress
                .completed_partitions
                .load(Ordering::Relaxed);
            let total = display_progress.total_partitions.load(Ordering::Relaxed);

            if total == 0 {
                continue;
            }

            clear_display(&mut stderr, lines_printed);

            // Progress bar
            let pct = (completed as f64 / total as f64 * 100.0).min(100.0);
            let bar_width = 30;
            let filled = (pct / 100.0 * bar_width as f64) as usize;
            let bar: String = "=".repeat(filled)
                + if filled < bar_width { ">" } else { "" }
                + &" ".repeat(bar_width.saturating_sub(filled + 1));

            let total_sol = display_progress.total_solutions.load(Ordering::Relaxed);
            let total_nodes = display_progress.total_nodes.load(Ordering::Relaxed);
            let elapsed = start.elapsed().as_secs_f64();
            let nps = if elapsed > 0.0 {
                total_nodes as f64 / elapsed
            } else {
                0.0
            };

            eprintln!(
                "[{}] {}/{} partitions | {} sol | {} | {} n/s",
                bar, completed, total, total_sol, format_elapsed(elapsed), format_count(nps as u64),
            );

            // Per-thread info
            let mut count = 1u16;
            for (i, desc_mutex) in display_progress.thread_descriptions.iter().enumerate() {
                if let Ok(guard) = desc_mutex.lock() {
                    if guard.is_empty() {
                        eprintln!("T{}: idle", i);
                    } else {
                        eprintln!("T{}: {}", i, *guard);
                    }
                } else {
                    eprintln!("T{}: idle", i);
                }
                count += 1;
            }
            lines_printed = count;
            stderr.flush().ok();
        }
    });

    let r = solve_parallel_with_progress(
        grid_text,
        dict,
        config,
        num_threads,
        disallow_shared_substring,
        Arc::clone(&progress),
    );

    progress.running.store(false, Ordering::SeqCst);
    display_handle.join().ok();

    (r.solutions, r.stats, r.exhausted)
}

// ---------------------------------------------------------------------------
// Info & validate commands
// ---------------------------------------------------------------------------

fn cmd_info(grid_path: PathBuf, dict_path: PathBuf) -> Result<()> {
    let grid = Grid::load(&grid_path).context("Failed to load grid")?;
    let dict = Dictionary::load(&dict_path).context("Failed to load dictionary")?;

    println!("Grid: {}x{}", grid.rows, grid.cols);
    println!("{}", grid);

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
