# Five-letter filter tables: production validation

September 10, 2026. Apple M5, Rust 1.93.1, release profile (thin LTO,
codegen-units=1), `RUSTFLAGS='-C target-cpu=native'`. Ordinary CLI builds,
without experimental features, tracing, policy dispatch, or search-budget hooks.

## Implementation

At each word position, precompute unions for the 32 subsets of each group
ABCDE / FGHIJ / KLMNO / PQRST / UVWXY. Z uses its existing letter index.
A restriction then copies one selected bitset and ORs at most five others,
instead of combining up to 26 individual-letter bitsets. Singleton masks use
the existing index directly without initializing tables.

Each position's flat table is initialized once, only when a multi-letter filter
actually uses it. `Arc` and `OnceLock` share initialization and storage across
cloned buckets and concurrent searches. Payload per initialized position is
`160 * ceil(words_of_this_length / 64) * 8` bytes. Tables live with the dictionary.
This measurement predates persistent discovery caches: at the time, both
discovery and exact counting used 2,000-candidate cutoffs. The current
[propagation implementation](propagation.md) uses masks, witnesses and a 512
discovery cutoff; exact counts remain at 2,000. The filter tables themselves
are unchanged. No complement/density policy or runtime configuration is added.

## Measurements

Runs were strictly sequential, with one search thread and no concurrent builds
or tests. Each comparison used baseline / candidate / candidate / baseline;
the table reports mean **whole-process elapsed time**, including dictionary
loading and lazy table construction. Binaries were frozen for measurement.
These are small-sample laptop measurements, not universal speedup guarantees.

| Workload | Baseline | Five-letter tables | Less elapsed time |
|---|---:|---:|---:|
| bench_7x7_long, 20,000-node report | 7.125s | 6.064s | 14.9% |
| bench_15x15_long, 20,000-node report | 13.973s | 11.597s | 17.0% |
| bench_15x15_with_tiers, exhaustive | 21.464s | 17.815s | 17.0% |

All comparisons used the same local `wl.dict` and grid inputs. The long grids
were stopped immediately after the ordinary CLI's first 20,000-node progress
report (`--progress-interval 20000`), which occurs before expanding that node.
They were **not** searched exhaustively. The 7x7 used
`--symmetry-break 0,1,1,0`. All three used `--disallow-shared-substring 0`,
`-n 0`, `-j 1` and the default scan window. The tiered run used
`--progress-interval 0` and completed all 36,711 nodes, 162,842 backtracks and
16 solutions. Every counter matched its baseline; exhaustive solution output
also matched byte for byte.

### Repository benchmark fixtures (exhaustive)

| Grid | Baseline | Five-letter tables | Less elapsed time | Solutions |
|---|---:|---:|---:|---:|
| bench_15x15_with_tiers | 17.397s | 14.007s | 19.5% | 16 |

Used `wl.dict` plus the checked-in tiered benchmark supplement, with substring limit
6, as in `bench.sh`. The separate plain ONDA fixture was not included in this
exhaustive comparison; its documented runtime is about ten minutes per run.

Each fixture matched all search counters and complete solution output. A separate
2-thread exhaustive tiered check (splitting disabled) returned the same set of
16 solutions in baseline and candidate builds. These runs are excluded from
the timing table.

## Validation and reproduction

New tests compare grouped filters with the independent per-letter union for
zero/all masks, each singleton and complement, every subset of all five groups
with/without Z, and 2,000 deterministic random masks. They cover partially used
final bitset blocks and reuse of nonzero scratch buffers. Another test checks
lazy allocation and shared concurrent initialization across bucket clones.

`cargo test --workspace`, `cargo fmt --all -- --check`, and
`cargo clippy --all-targets` passed (the private workspace retains existing
unrelated Clippy warnings). The public repository also passes Clippy with
`-D warnings`. Build baseline and candidate in **separate target directories**
and copy each executable to a fixed path before running comparisons; verify
their hashes differ. Reproduce full fixtures with the commands in `bench.sh`,
using `--progress-interval 0` to suppress timing noise.

Baseline revision: `4380b11bbfcd2e507ee1dc2eaa8af5d479938f22`.

base executable SHA-256: `fea5e2a07313c80f2cbb19562b2ce647396238d29b1688e1f27f7228ac2bdda5`.

new executable SHA-256: `ba07c00eddee9825afd1ccd45e76373ce59c0c64cbae26521c70dfa1338fc8f5`.

Dictionary SHA-256: `daa7126c6875f77a8ae3d2cb9d9ca08e30a21116cfcbcd8c700c700bfbb073e9`.
