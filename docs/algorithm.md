# How Orca Works

Orca is a constraint-satisfaction solver specialized for crossword grid filling. It performs exhaustive search -- finding all valid fills -- using constraint propagation and backtracking search.

## Overview

The solver works in three phases:

1. **Setup**: Parse the grid, build a constraint graph of crossing slots, initialize candidate word domains from the dictionary.
2. **Propagation**: Use arc consistency (AC-3) to eliminate impossible candidates.
3. **Search**: Branch on cells, propagate after each choice, backtrack on contradictions.

## Constraint propagation (AC-3)

Each slot has a **domain** -- the set of dictionary words that could still fit. When a slot's domain shrinks, Orca re-examines every crossing neighbor: for each crossing position, it computes which letters are still possible in the updated slot, then filters the neighbor's domain to only words compatible with those letters. This process repeats until no more reductions are possible (fixpoint) or a domain becomes empty (contradiction).

Orca processes slots in **priority queue order**, always propagating from the slot with the smallest domain first.

### Key optimizations

- **Bitset domains**: Each slot's candidate set is a bitset over word IDs, enabling fast AND-based filtering.
- **Precomputed letter_bits**: For each (slot_length, position, letter), a bitset marks which words have that letter at that position. Filtering a domain to "words with letter L at position P" is a single bitwise AND.
- **Incremental counting**: Domain sizes are maintained incrementally (tracking removed bits) rather than recomputed from scratch.
- **Small-domain threshold**: For domains under 2000 words, letter counts are computed by direct iteration rather than bitset intersection, which is faster due to cache locality.

## Cell-level branching

Traditional crossword solvers branch at the **slot level**: pick a slot, try each word. Orca instead branches at the **cell level**: pick a single cell at a crossing, try each viable letter. This decomposes each word choice into independent per-position letter choices, sharing propagation work across all words that agree at each position.

With a naive minimum-remaining-values (MRV) heuristic, cell-level branching is actually *slower* than slot-level because it creates more nodes. The advantage only materializes with a crossing-aware heuristic:

## Branch selection heuristic (SoCDP)

At each search node, Orca evaluates up to 15 crossings (pre-sorted by slot length sum, descending) and selects the one minimizing:

```
score = sum over each letter L of: count_across[L] * count_down[L]
```

where `count_across[L]` is the number of across-slot candidates with letter L at the crossing position, and likewise for `count_down[L]`. This **sum of crossing domain products** (SoCDP) estimates the total sub-tree size across all branches at that crossing.

Crossings between longer slots are evaluated first (via a static sort by length sum), since these tend to be more constraining. The bounded scan of 15 crossings balances evaluation cost against selection quality.

## Iterative search with forced-move inlining

The search uses an explicit stack rather than recursion, avoiding stack overflow on deep search trees. When a cell has exactly one viable letter, it is applied inline without creating a stack frame or trail entry -- its domain save is folded into the parent branch's trail. This prevents memory blowup on long chains of forced moves.

## Duplicate and substring constraints

At leaf nodes (all domains are singletons), Orca validates the fill:

- **No duplicate words**: No word may appear twice in the same grid.
- **Shared substring constraint**: By default, no two entries may share a 6+ letter substring. This is configurable via `--disallow-shared-substring`.

These checks are deferred to leaf nodes rather than maintained during search, since the overhead of incremental tracking outweighs the pruning benefit.

## Parallel search

Multi-threaded search works by partitioning the search tree. The root state is split at the best crossing cell: each viable letter becomes a separate partition. Partitions with the highest estimated work (sum of log2 domain sizes) are split further until the desired partition count is reached. Threads pull partitions from a shared work queue.

Within each thread, a **mid-search split** mechanism handles load imbalance: if a partition runs longer than a timeout (default 3 seconds), all remaining untried letters at every stack frame are extracted as new sub-partitions and fed back into the queue. The current thread finishes only its single remaining leaf path.

## Symmetry breaking

Grids with diagonal symmetry (where transposing rows and columns produces an equivalent grid) can yield pairs of transpose-equivalent fills. Orca prunes these by enforcing `letter(cell_a) <= letter(cell_b)` for a pair of diagonally-mirrored cells, halving the search space. This is integrated into the AC-3 fixpoint loop.
