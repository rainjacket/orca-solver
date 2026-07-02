# Orca

**[How Orca Works](https://rainjacket.github.io/orca-solver/)** — a blog post style explanation of the algorithm and design.

A high-performance crossword grid filler.

Orca is designed for wide-open grids that are difficult for other solvers. It uses AC-3 propagation with cell-level branching, and tuned heuristics for rapid exhaustive search. Multi-threaded search is supported via partition-based parallelism.

## Installation

### Pre-built binaries

Download from [GitHub Releases](https://github.com/rainjacket/orca-solver/releases).

### Build from source

```bash
cargo build --release
# Binary is at ./target/release/orca
```

## Quick start

```bash
# Interactive mode — prompts for the common options
orca fill

# Find all solutions
orca fill my_grid.txt my_words.dict

# Find first solution only
orca fill my_grid.grid my_words.dict -n 1

# Use 4 threads (with live progress display)
orca fill my_grid.grid my_words.dict -j 4
```

When run with multiple (2 or more) threads on a terminal, Orca shows a live progress display with a partition progress bar and per-thread status. When solutions are found, an HTML solution browser is automatically generated for reviewing and comparing fills.

## Dictionary

Orca takes any `.dict` file as a command-line argument — you supply your own dictionary.

### Dictionary format

Orca uses `.dict` files with one entry per line:

```
WORD;SCORE
```

- Words are uppercased on load; entries containing non-letters are skipped
- Words shorter than 3 letters are ignored
- Lines starting with `#` are comments
- Scores are currently unused

## Grid format

Grid files can use `.grid` or `.txt` extensions. The first non-comment line is `rows cols`, followed by the grid:

```
# This is a comment
5 5
#..#.
.....
.....
.....
.#..#
```

| Character | Meaning |
|-----------|---------------------------|
| `#`       | Black square              |
| `.`       | Empty cell (to be filled) |
| `*`       | Wild cell (unconstrained) |
| `A-Z`     | Prefilled letter          |
| `[ABC]`   | Letter subset constraint  |
| `0-9`     | Empty cell with a scan-order tier (see below) |

Comments (lines starting with `# `) are only allowed before the dimensions line. The space matters: a bare `#####` line is grid data (a row of black squares).

### Scan-order tiers

A white cell may be a digit `0`-`9` instead of `.`, tagging it with a *scan-order tier*.
Crossings are sorted by tier (`0` first, then `1` … `9`, then untiered `.` cells), nudging
the solver to fill lower-tier cells earlier. This is a bias, not a strict branch order: the
branch heuristic scans a window of crossings (the scan limit), which may span multiple
tiers. A digit cell is otherwise an ordinary empty cell. See
[`grids/bench_15x15_with_tiers.grid`](grids/bench_15x15_with_tiers.grid) for an example.

## CLI reference

### `orca fill`

Run without arguments to enter interactive mode, which prompts for the common options and auto-detects features like diagonal symmetry breaking.

### `orca fill <GRID> <DICT>`

Fill a crossword grid with words from a dictionary.

| Option                          | Default   | Description                    |
|---------------------------------|-----------|--------------------------------|
| `-n, --max-solutions N`         | `0` (all) | Stop after finding N solutions |
| `-j, --threads N`               | `1`       | # of parallel threads          |
| `--disallow-shared-substring N` | `6`       | Set to `0` to disable          |
| `--symmetry-break "r1,c1,r2,c2"`|           | Enforce l(r1,c1) <= l(r2,c2)   |
| `--progress-interval N`         | `10000`   | Report progress every N nodes  |
| `--split-timeout N`             | `3` (sec) | Task timeout (multi-core only) |

Solutions are printed to stdout; progress and stats go to stderr.

### `orca info <GRID> <DICT>`

Print grid layout, slot details, and dictionary statistics.

### `orca validate-dict <DICT>`

Check a dictionary file for format issues.

## Benchmarking

Two 15x15 benchmark grids and a script are included. Bring your own wordlist — we recommend [Spread the Wordlist](https://www.spreadthewordlist.com/) (~300K entries) for comparable results:

```bash
mv ~/Downloads/spreadthewordlist_caps.dict dictionaries/
./bench.sh                      # or: DICT=/path/to/your.dict ./bench.sh
```

The script builds a release binary and runs an exhaustive search on both grids (`bench_15x15.grid` and `bench_15x15_with_tiers.grid`, the scan-order tier demo). Each grid has a small companion supplement in `dictionaries/` — the entries of a known fill — appended to your wordlist at run time, so both grids are fillable with any large list. The plain grid is a long exhaustive run (on the order of ten minutes sequentially with a ~300K wordlist); the tiers grid finishes in seconds. Use `./bench.sh --parallel N` to benchmark multi-threaded search.

## License

MIT
