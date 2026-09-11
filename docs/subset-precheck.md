# Removing the subset precheck

Measured against b3c919a with persistent caches and the 512 cutoff; only the
subset precheck was removed.
The direct variant snapshots before applying the AND, counts removed candidates,
and skips statistics updates and enqueueing if zero were removed. This preserves
backtracking even if an intersection changes the domain. No extra snapshots at a
level if that slot was already saved.

Native release builds, one measured process/thread at a time, no concurrent builds
or tests. Precheck/direct/direct/precheck, whole-process elapsed time. Long grids
use the first 20,000-node report; tiers run exhaustively. Identical search counters
and solution output in all 12 runs. Same dictionary, seeds and 7x7 symmetry flag
as preceding production benchmarks. Two observations per variant/workload; small
differences are not established wins. The direct variant is now the production implementation.

| Grid | Precheck | Direct | Direct elapsed change |
|---|---:|---:|---:|
| bench_7x7_long | 4.108s | 3.799s | -7.5% |
| bench_15x15_long | 7.763s | 7.573s | -2.4% |
| bench_15x15_with_tiers | 10.699s | 10.231s | -4.4% |

The production change passes all 32 solver tests in both repositories, plus
the public workspace tests and strict Clippy. The subset shortcut is removed from
the How Orca Works optimization list; the unchanged-arc-filter shortcut remains.
Raw timing logs and the comparison patch are retained locally in
`results/subset-precheck/`.
