# Persistent propagation caches: production verification

September 10, 2026. Normal release builds with native CPU optimization, thin LTO
and one codegen unit on Apple M5. No experimental features, instrumentation,
runtime policy selection or custom search-budget hooks. Both repositories use
identical propagation, domain-state and witness implementations.

## Changes

Retain within-call rejected-letter pruning, persist conservative letter masks
with domain snapshots, reuse positive witnesses after membership checks, and
iterate words for propagation domains of at most 512 candidates. Exact heuristic
counts and standalone discovery retain 2,000. The unused uncached large-domain
batch fallback and experiment flags are removed. See [implementation and cache
invariants](propagation.md).

## Measurements

One benchmark process at a time, one search thread, no concurrent builds/tests.
For each grid: baseline / private / public / public / private / baseline. Values
are mean whole-process elapsed times, including dictionary and cache setup.
Percentages compare against the same pre-change private production executable
(`42476f9`, already including five-letter filter tables); the public column is
not a separately measured before/after public CLI comparison. These small samples
show workload-dependent improvements, not a universal speedup guarantee.

| Workload | Production baseline | New private | New public |
|---|---:|---:|---:|
| `bench_7x7_long` | 4.173s | 4.052s (-2.9%) | 4.010s (-3.9%) |
| `bench_15x15_long` | 8.197s | 7.530s (-8.1%) | 7.371s (-10.1%) |
| `bench_15x15_with_tiers` | 12.636s | 10.139s (-19.8%) | 10.046s (-20.5%) |

Both long grids stop at the ordinary CLI's first 20,000-node progress report;
they are **not exhaustive**. The tiered grid is exhaustive. All runs use the same
local `wl.dict` and private benchmark fixtures, unchanged prefills/tiers, unlimited
solutions, one thread, scan limit 15 and substring exclusion disabled. The 7x7
long grid uses `--symmetry-break 0,1,1,0`. Public runs disable HTML output using
`--no-browser`. The local dictionary and some benchmark fixtures are not bundled
with the public repository.

## Equivalence and checks

All 18 timed runs match nodes, backtracks, propagations, wipeouts, duplicate
rejections and solution counts. Complete tiered solution sets match exactly.

| Workload | Nodes | Backtracks | Solutions |
|---|---:|---:|---:|
| 7x7 long prefix | 20,000 | 94,436 | 0 in prefix |
| 15x15 long prefix | 20,000 | 118,541 | 0 in prefix |
| Tiered exhaustive | 36,711 | 162,842 | 16 |

Additional exhaustive runs on `bench_7x7` and `bench_15x15` match all counters
and solution sets across baseline/private/public (six runs). Two-thread tiered
checks match all 16 solutions for all three binaries with splitting disabled,
and for both new binaries with a one-second split timeout enabled (five runs).
Parallel timing and counters are not compared because scheduling can differ.

- Full workspace tests pass in both repositories, including 32 solver tests.
  The private workspace retains one pre-existing ignored integration test.
- New coverage includes mask refinement without extra snapshots, nested and
  cloned-state restoration, stale witnesses, and real propagation through the
  512/513 boundary, failure and sibling backtracking. Support is checked against
  direct word inspection and a clone with discovery caches cleared.
- Formatting and Clippy pass; the public workspace uses `-D warnings`.
  The private workspace retains its existing unrelated lint warnings.
- The private CLI, coordinator and worker build in release mode; the public CLI
  builds in release mode. This verification does not deploy cloud binaries.

## Reproduction

Build each revision in a separate target directory with
`RUSTFLAGS='-C target-cpu=native' cargo build --release -p orca-cli`, then freeze
the binaries before measuring. Use `orca fill GRID DICTIONARY -j 1 -n 0
--disallow-shared-substring 0 --progress-interval 20000` for long prefixes and
terminate at the first progress report. Use `--progress-interval 0` and let the
solver exhaust for finite fixtures. Add the 7x7 symmetry flag above and the public
`--no-browser` option. For parallel equivalence, use `-j 2 --split-timeout 0`
or `--split-timeout 1` and compare sorted solutions.

Private raw evidence is in `results/production-propagation/`: `verify.py`,
`runs.json`, numbered logs, build/test/lint logs and `manifest.json` with binary
and input hashes. Historical tuning results remain in `results/cutoff-retune/`
and `results/persistent-masks/`; they are not new production measurements.

Compiler: rustc 1.93.1 (01f6ddf75 2026-02-11).

Dictionary SHA-256: `daa7126c6875f77a8ae3d2cb9d9ca08e30a21116cfcbcd8c700c700bfbb073e9`.
