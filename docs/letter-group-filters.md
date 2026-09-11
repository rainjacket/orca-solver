# Letter-group filter tables

Orca builds a neighbor filter from the viable crossing letters using the fixed
groups **AEHIOU / BCGMP / DLNRST / FKVWY / JXZ / Q**. Each touched group
contributes one precomputed bitset; Q reuses its existing letter index. Copy the
first bitset and OR the rest: at most six source bitsets and five OR passes.
Zero and singleton masks bypass table initialization.

The five non-singleton groups contain 6, 5, 6, 5 and 3 letters. Their subset
tables occupy 64 + 32 + 64 + 32 + 8 = **200 bitset rows**, including empty subsets.
Each position initializes its flat table lazily through `OnceLock`; bucket
clones and concurrent searches share it through `Arc`. No per-search copies,
backtracking snapshots, runtime policies, or instrumentation are added.

Payload per initialized position is `200 * ceil(word_count / 64) * 8` bytes.
For the benchmark dictionary, initializing every supported length and position
would use **150.94 MiB**, versus 120.75 MiB for the previous 160-row layout.
This is table payload, not total process memory or a universal dictionary cap.

The layout was selected using recorded filter masks from three benchmark grids,
minimizing frequency- and bitset-size-weighted table reads. A bounded search
used swaps, moves, splits and merges, with at most six letters per group and
200 MiB fully initialized tables for that dictionary. This was a heuristic
search, not a proof of optimality. End-to-end timings also include packed-mask
extraction, memory locality and lazy initialization, which the model omits.

Experimental comparison (two sequential runs per variant, reversed order):

| Layout | 7×7 long | 15×15 long | 15×15 tiered |
|---|---:|---:|---:|
| Previous production, 160 rows | 3.680s | 7.627s | 10.447s |
| Selected, 200 rows | 3.602s | 6.995s | 9.448s |

The long grids stop at the ordinary 20,000-node report; the tiered grid is
exhaustive. Controls drifted materially, so these means do not establish a
universal speedup. All search counters and exhaustive solutions matched.
The 7×7 run uses `--symmetry-break 0,1,1,0`; all use `-j 1 -n 0
--disallow-shared-substring 0`. See [propagation](propagation.md) for the
separate discovery caches and 512-candidate cutoff.

Tests compare every subset of each group (with and without Q), all singletons
and complements, and random cross-group masks against independent per-letter
unions. They also cover partial tail blocks and shared lazy initialization.

## Clean production verification

September 11, 2026: ordinary release builds of both repositories, with
`RUSTFLAGS='-C target-cpu=native'`, no experimental hooks. One process at a
time, one run per binary and workload; baseline is private revision `bfe832c`.

| Binary | 7×7 long | 15×15 long | Tiered |
|---|---:|---:|---:|
| native | 3.990s | 7.584s | 9.863s |
| private | 3.826s | 7.160s | 9.548s |
| public | 3.794s | 7.113s | 10.395s |

All nine runs matched baseline search counters; all exhaustive outputs matched
the same solution hash. The public tiered timing was slower in this single run;
these checks establish equivalence, not statistical performance superiority.
Both repositories passed workspace tests and formatting checks; public Clippy
passed with warnings denied. Private Clippy has existing unrelated warnings.

Dictionary SHA-256: `daa7126c6875f77a8ae3d2cb9d9ca08e30a21116cfcbcd8c700c700bfbb073e9`.
