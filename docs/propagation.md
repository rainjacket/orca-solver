# Propagation and backtracking

Orca branches on letters at crossing cells, but AC-3 domains remain sets of whole
words. A guess restricts the participating slots, then a smallest-domain-first
queue propagates restrictions until a fixpoint or an empty domain. These
optimizations preserve branch selection, search order and exhaustive results.

## Discover letters, then filter words

For each dequeued source slot, discover the viable letters at its crossing
positions before restricting neighboring domains:

- **At most 512 candidates:** iterate the remaining word IDs once, accumulating
  letter masks for all crossing positions. The iterator visits indexed nonempty
  64-bit blocks, then enumerates their set bits. This path
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

For the neighbor, select the precomputed sources from the
[letter-group subset tables](#letter-group-filter-tables). Fuse their OR with the domain intersection.
Skip all-26-letter filters and unchanged filters within the same propagation
call. Otherwise save the domain, apply the intersection and count removed bits
in one pass. If none were removed, skip the statistics update and enqueueing.
There is no separate subset precheck. Filter construction has no 512 cutoff.

## Letter-group filter tables

Filter construction uses the fixed groups **AEHIOU / BCGMP / DLNRST / FKVWY /
JXZ / Q**. Each touched group contributes one precomputed bitset; Q reuses its
existing letter index. Select these sources once, then OR their words and
intersect each domain word immediately, counting removed candidates in the
same pass. This avoids writing and rereading a temporary filter. Kernels are
specialized for one through six sources. Zero and singleton masks bypass the tables.

The five non-singleton groups have 6, 5, 6, 5 and 3 letters, requiring
64 + 32 + 64 + 32 + 8 = **200 subset rows**, including empty subsets.
Each position's flat table is initialized lazily through `OnceLock` and shared
across bucket clones and concurrent searches through `Arc`. Tables are immutable
and are not copied into search states or backtracking snapshots.

Payload per initialized position is `200 * ceil(word_count / 64) * 8` bytes,
where `word_count` is the number of dictionary words of that slot length.
The grouping was selected using observed filter masks to reduce the number of
source bitsets read; it is fixed, with no runtime tuning or policy switch.

Tests compare grouped filters with independent per-letter unions, including
every subset of each group with and without Q, singleton and complement masks,
random cross-group masks, partial tail blocks, and shared lazy initialization.

## Domain storage and two independent choices

`CandidateSet` owns a private bitset, cached count, and sorted nonempty-block
index. Every mutation maintains all three; a snapshot restores them together.
`SlotDomain` adds the snapshot-backed letter bounds. Propagation handles snapshot
timing and scheduling, without choosing intersection kernels or updating indexes.

| Operation | Choice |
|---|---|
| Discover crossing letters in source A | At most 512 words: iterate candidates through the nonempty-block index. Otherwise: witnesses and bitset support searches. |
| Apply the filter to neighbor B | Below 25% nonempty blocks: indexed intersection. At or above 25%: dense intersection. |

The 25% switch applies **only to filter application**. Small-domain iteration
always skips empty blocks. Replacement-witness searches and exact heuristic
counts retain their existing dense-bitset implementations. Both filtering paths
fuse source unions, AND, and removed-bit counting. Dense filtering refreshes the
index after a change; indexed filtering maintains it during traversal. Plain
bitset intersection shares this kernel. Symmetry exclusion also updates metadata
through the domain interface.

Negative letter bounds are facts about a domain state and must be restored.
Witnesses are hints checked before reuse and need not be trailed. The last-applied
arc mask records work already done on a neighbor during one propagation call.
These remain distinct because their lifetimes differ. Saved-level markers are
not used; existing per-level snapshot deduplication is unchanged.

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
first candidate update attempt for a slot at that decision level, `save_domain`
clones its complete domain (candidate bitset, cached count and letter masks). A no-op
intersection may therefore save a domain too. Later updates of that slot at the
same level do not add another snapshot. `pop_level` restores all saved domains. Forced moves share their enclosing decision's trail.

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
