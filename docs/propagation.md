# Propagation and backtracking

Orca branches on letters at crossing cells, but AC-3 domains remain sets of whole
words. A guess restricts the participating slots, then a smallest-domain-first
queue propagates restrictions until a fixpoint or an empty domain. These
optimizations preserve branch selection, search order and exhaustive results.

## Discover letters, then filter words

For each dequeued source slot, discover the viable letters at its crossing
positions before restricting neighboring domains:

- **At most 512 candidates:** iterate the remaining word IDs once, accumulating
  letter masks for all crossing positions. `iter_ones` scans bitset blocks and
  enumerates their set bits; it does not inspect every dictionary word. This path
  refreshes the stored masks but does not populate witness IDs.
- **More than 512 candidates:** check only letters in the intersection of the
  source position's stored mask and this call's directed-arc mask. For each
  letter, first test whether its cached supporting word is still in the domain.
  If not, scan the domain AND the position/letter index until finding a supporting
  word, or exhaust the intersection and remove that letter from the result.

The result is exact on either path. Moving across the cutoff is safe: masks stay
valid and every reused witness is checked, including after small-domain work.
The separate standalone letter-discovery and exact SoCDP letter-count functions
still use a **2,000-candidate cutoff**. Witnesses establish existence, not counts;
they do not replace the exact counts needed by the branching heuristic.

For the neighbor, build the OR of the viable-letter indexes using the existing
[five-letter subset tables](five-letter-filters.md), then AND with its domain.
Skip all-26-letter filters, unchanged filters within the same propagation call,
and intersections that would remove no candidates. Update counts from removed
bits and enqueue each changed neighbor. Filter construction has no 512 cutoff.

## Three kinds of cached information

| Cache | Meaning | Lifetime and backtracking |
|---|---|---|
| Per-domain position mask | Conservative upper bound on viable letters; exact after discovery | Persists across propagation calls; cloned and restored with its domain |
| Per-directed-arc witness IDs | One supporting word per letter, or unknown | Persists across calls and backtracks; validate membership on every reuse |
| Last applied mask per directed arc | Neighbor has already been filtered by these letters | Resets to all 26 letters every propagation call |

Within one call domains only shrink, so a rejected letter cannot return and an
unchanged filter cannot remove anything new. Across calls, restored neighbor
domains may need the same filter applied again: a persistent discovery mask does
**not** justify skipping that filter. This is why the last-applied cache resets.

Witnesses carry only positive support. Absence is stored in the domain's mask,
not as a persistent failure in the witness cache. No epochs, separate negative
witness state, or witness trail are needed. A cache belongs to one fixed graph
and dictionary; new searches initialize it, while state clones copy it.

## Snapshot semantics

`push_level` records a trail boundary; it does not clone all domains. Before the
first candidate mutation of a slot at that decision level, `save_domain` clones
its complete domain (candidate bitset, cached count and letter masks). Later
mutations of that slot at the same level do not add another snapshot. `pop_level`
restores all saved domains. Forced moves share their enclosing decision's trail.

Refining a letter mask alone requires no new snapshot. If the slot's candidates
changed at this level, the pre-change snapshot already preserves its parent
mask. If they did not change, the refinement is valid for the parent domain too.
Restoring a broader mask can cost extra discovery work but cannot remove a valid
letter. Candidate mutations must still save the domain before modifying it.

The masks occupy an inline `[u32; 32]`: **128 additional bytes per current domain
and saved domain**, without another heap allocation per snapshot. This matches
the existing 32-position batch-propagation limit. Witnesses use **104 bytes per
directed arc**, allocated in a reusable vector per solver state, not per snapshot.

## Implementation and validation

- `crates/solver/src/propagate.rs`: discovery, cutoff and last-applied arc masks.
- `crates/solver/src/witnesses.rs`: positive support lookup and revalidation.
- `crates/solver/src/state.rs`: domain masks and existing trail restoration.

These are unconditional production paths: no experimental features, runtime
policy switches or alternate cache implementations. Tests cover nested restores,
mask refinement without candidate changes, cloned state, stale witnesses,
propagation failure and sibling branches, and transitions at 512/513 candidates.
See [production verification](propagation-validation.md) for benchmark scope and
search-equivalence checks.
