# ADR-0060: Background Adjacency Maintenance

**Status:** Accepted
**Date:** 2026-08-31
**Deciders:** AletheiaDB Core Team
**Categories:** index, concurrency, performance
**Issue:** #3810
**Supersedes (in part):** the "Background Compaction Strategy" section of
[ADR-0026](0026-incremental-csr-adjacency.md)

## Context

ADR-0026 introduced a two-tier adjacency index: an immutable frozen CSR plus a
mutable delta buffer and a tombstone set. Its read fast path
(`IncrementalAdjacencyIndex::frozen_view`, ~8-14ns) is available **only when the
delta buffer and the tombstone set are globally empty** — a state only
`compact()` produces. Phase 5 of that ADR shipped a per-index
`CompactionScheduler`, and Phase 6 was recorded as "CurrentIndexes integration".

Neither was ever reached by a shipping database. `CurrentStorage::new()` — the
single constructor behind both `AletheiaDB::new()` and `AletheiaDB::open()` —
built its indexes with `CurrentIndexes::new()`, which starts no scheduler;
`new_with_background_compaction()` was called from exactly one integration test
and from nowhere in `src/`. Compaction therefore never ran in a real database,
for any graph size, for the life of the process.

Profiling (Issue #3810, `examples/bolt_workload.rs`, 1,000 nodes / 6,000 edges /
3,000 read iterations) measured the consequence: **100% of 129,000
`get_outgoing_edges` calls took the merged (delta) path**, paying a `NodeId`
SipHash lookup into the delta `DashMap`, the delta iterator machinery, and a
per-call `Vec` allocation the frozen path avoids — about 15.5% of the profile,
plus a share of the ~20% spent in `malloc`/`free`.

Two further gaps made a naive "just start the scheduler" fix insufficient:

1. **Threshold bootstrap gap.** `should_compact()`'s ratio branch is
   `frozen > 0 && delta >= frozen * ratio`. On a fresh index `frozen == 0`, so
   only the absolute threshold (10,000 delta edges) can fire. A graph smaller
   than that — the common case — would never compact even with a scheduler
   running.
2. **A dormant torn-read bug.** `compact()` retired the delta entries *first*
   and published the rebuilt CSR *second*. A reader landing in that window
   paired a pre-compaction CSR with a post-retire (empty) delta and returned an
   adjacency list **missing** those edges; on a freshly built graph, where every
   edge is still in the delta, it returned an **empty** list. Nothing shipped
   compaction concurrently with reads, so the bug never fired in production —
   starting a background compactor would have made that interleaving routine.
   `compact()` was also not serialized with itself: two concurrent compactions
   each rebuild from the same frozen CSR while only one retires a given delta
   entry, and the later publish drops the other's edges.

## Decision

### 1. One shared, process-wide maintenance worker

A single background thread (`src/index/adjacency_maintenance.rs`) services every
registered adjacency index through `Weak` references. `CurrentIndexes::new()`
registers both of a database's indexes (outgoing + incoming).

Rejected: a scheduler pair per index (the shape ADR-0026 shipped). A database
owns two adjacency indexes and the test suite alone constructs hundreds of
ephemeral databases, so that is two threads per database, a `Drop`/shutdown
obligation on every owner (`shutdown_background_compaction` takes `&mut self`),
and teardown cost on every test. A shared worker is one thread per process
regardless of database count, and `Weak` registration means a dropped database
deregisters itself — no `Drop` impl, no join, no shutdown call.

### 2. Write-quiescence is the trigger, not just size thresholds

Per tick, an index with pending work whose `delta + tombstone` count is
**unchanged** across `quiet_ticks` consecutive ticks is compacted. Both counters
are monotonic between compactions, so equality across a tick *is* the absence of
writes; no new write-path counter is needed.

This is what actually unlocks the read fast path. Compacting *during* a write
burst does not: the very next insert re-disables the fast path, so only draining
the delta after writes stop changes what reads cost. It is also what closes the
threshold bootstrap gap — quiescence is size-independent, so a 600-edge graph
reaches the frozen path exactly as a 6,000,000-edge one does.

`should_compact()`'s size thresholds are kept unchanged and still apply: they
bound delta growth under a write burst that never goes quiet. Deliberately, the
ratio branch was **not** "fixed" to fire at `frozen == 0` — `delta >= 0` is
true for every insert, which would compact on every write.

### 3. A self-tuning duty-cycle rate limit

After compacting, an index is ineligible for
`max(min_compaction_interval, cost * (100 - duty) / duty)` where `cost` is how
long that compaction took (default duty: 10%). Compaction may therefore consume
at most ~10% of one core per index: a large graph, where a rebuild is expensive,
is compacted proportionally less often, and a read/write-interleaved workload
cannot be driven into the O(E log E) rebuild cliff ADR-0026 exists to avoid.

### 4. All policy off the hot path

Quiescence detection, thresholds and rate limiting run on the worker. Neither
the read path nor the write path gains an instruction of policy — in particular
no per-read atomic RMW, which would undo the multi-threaded read scaling work in
#3811. This is the decisive argument against the alternative of triggering
compaction lazily from the read path.

### 5. Publish-before-retire, with a de-duplicating publish window

`compact()` now publishes the rebuilt CSR **before** retiring the delta entries
it absorbed, and retires them **selectively** (only the entries in its snapshot,
so a write that lands mid-compaction stays in the delta rather than being
dropped).

Some window is unavoidable — compaction moves edges between two structures a
reader observes with separate loads — so the choice is which window:

| Order | Window contains | Reader sees |
|---|---|---|
| retire → publish (before) | edge in neither layer | **missing** edges (empty list on a fresh graph) |
| publish → retire (now) | edge in both layers | a duplicate — which is *filterable* |

A duplicate is recoverable: while the window is open, the delta half of a read
is de-duplicated against the frozen slice being emitted (a binary search over
one node's run, which `AdjacencyIndex::build` leaves sorted by
`(target, edge_id)`). A missing edge is not recoverable by any reader-side
filter.

Read order is part of the contract and is documented at both readers:

- fast path: **counters before frozen** — counters drop only after the CSR is
  published, so observing zero proves the CSR loaded next is the new one;
- merged path: **delta before frozen, publish-window flag after frozen** — an
  entry missing from the delta read must already have been retired, which
  happens strictly after the CSR carrying it was published; and the window flag
  is set before publication, so a reader holding the new CSR always sees it.

Taking the delta reference first also pins that shard against retirement for the
lifetime of the guard, which is what keeps the window flag meaningful for
long-lived guards (e.g. `get_outgoing_edges_iter`).

`compact()` is additionally serialized with itself, and with
`import_frozen_csr`, by a mutex taken only by compaction — never on the read or
insert path.

### 6. An amortization floor on the quiescence trigger

A quiescent index is compacted only when `pending >= frozen_edges /
quiescent_amortization` (default 10,000, i.e. 0.01% of the graph). Without a
floor, one edge per second written into a 6M-edge graph goes quiescent every
second and buys a full O(E log E) rebuild -- roughly 340MB of transient
allocation -- to merge a single edge, forever. The duty-cycle limiter bounds the
CPU that costs but not the memory churn or the pointlessness.

The floor never blocks the case Issue #3810 is about: a small graph (600 edges,
`frozen` still 0 on the first compaction) always clears it. Its cost is that a
very large graph taking a slow trickle of writes keeps its reads on the merged
path for longer; `should_compact()`'s absolute thresholds still bound how far
the delta grows. Set `quiescent_amortization: 0` to compact on any pending work.

### 7. Default on, opt-out via the unified config

`AletheiaDBConfig::adjacency` (`AdjacencyMaintenanceConfig`) tunes or disables
it; `AdjacencyMaintenanceConfig::disabled()` restores exactly the pre-#3810
behavior (correct reads via the merged path, compaction only when
`AletheiaDB::compact_adjacency()` is called). Registration is also a no-op on
`wasm32` (no threads) and under Miri.

Default-on rather than opt-in: the documented performance characteristics of
this subsystem are unreachable otherwise, and an opt-in flag nobody sets leaves
the product exactly where Issue #3810 found it.

## Consequences

### Positive

- Adjacency reads reach the frozen CSR fast path in a shipping database.
  Measured on the issue's own harness (`bolt_workload`, 1,000 nodes / degree 6 /
  3,000 read iterations): **356.6M → 304.6M instructions, -14.6%**, with the
  merged-guard read machinery (delta `DashMap` lookup, SipHash, `iter_set`
  iteration) gone from the profile.
- A latent torn-read bug that could return an empty adjacency list during any
  concurrent compaction is fixed, and concurrent compactions can no longer lose
  edges.
- One thread per process; dropping a database needs no shutdown call.
- Observability: `AletheiaDB::adjacency_stats()` reports each layer's occupancy,
  and `is_fully_compacted()` is exactly "reads are on the fast path".

### Negative

- A background thread exists by default (parked when no index has pending work,
  waking at the idle tick otherwise). Processes that want none must opt out.
- Compaction now runs concurrently with application reads and writes as a matter
  of course, which is why the publish protocol above is a correctness contract
  rather than an implementation detail.
- The merged read path carries one extra atomic load (the publish-window flag)
  and, inside a window, one binary search per delta entry.
- Retirement takes the delta shard write locks, so a thread that holds a
  `MergedAdjacencyGuard` (or an `OutgoingEdgesIter`) and then calls
  `compact_adjacency()` on the same database from the same thread would
  deadlock. Documented on `AletheiaDB::compact_adjacency`. Other threads are
  unaffected: they never wait on compaction, compaction waits on them -- which
  in turn means one long-held adjacency iterator delays the shared worker for
  every database in the process.
- The two counter loads on the read fast path went `Relaxed` -> `Acquire`. Free
  on x86-64; a real (small) barrier on aarch64. They are the synchronisation
  edge that makes the fast path safe, so they cannot be weakened; packing the
  two counters into one `AtomicU64` to halve them is a possible follow-up.
- The process-global worker and its registry mutex are not `fork()`-safe: a
  child process inherits the registry but not the thread, so maintenance
  silently stops there (and a fork taken while the registry lock was held would
  deadlock the child's first database construction). Embedders that fork after
  constructing a database should disable maintenance in the child, or fork
  first.
- A write-only database (ingest sink, replica) pays the compaction budget for a
  read fast path nobody uses. A read-demand signal was considered and rejected
  for v1: the cheap forms still touch the read path, which this ADR is
  deliberately keeping free of policy.

### Neutral

- `CompactionScheduler` and `CurrentIndexes::new_with_background_compaction()`
  remain for callers that want a dedicated compactor; indexes created that way
  do not also enroll in the shared worker, so exactly one compactor owns them.

## Alternatives Considered

**Lazy compaction triggered from the read path.** Attractive (no thread, fully
deterministic) but needs a shared counter RMW on every adjacency read to
amortize, and puts an O(E log E) rebuild on an application read's critical path.
Rejected on both counts.

**Compaction at write-transaction commit boundaries** (via the unused
`WriteBuffer::has_edge_operations()` hook). Compacting on every edge-carrying
commit is the rebuild cliff; compacting on a threshold leaves a residual delta
after the last commit, which is precisely the state that keeps reads off the
fast path. Rejected.

**Opt-in only.** Leaves the shipped product with an unreachable fast path.
Rejected.

**Making `should_compact()`'s ratio branch fire at `frozen == 0`.** Degenerates
to "compact on every insert". Rejected in favor of the quiescence trigger.

## Adjacent defects fixed here

Enabling compaction made three latent bugs reachable; all three are fixed in the
same change, with regression tests:

1. **Torn reads** (above): the publish/retire order.
2. **Non-idempotent recovery.** `AdjacencyIndex::build` does not de-duplicate,
   so a compaction that unwound between publishing and retiring left the next
   compaction collecting the same edge twice -- permanently. The publish window
   is now an RAII guard that records the interruption, and the next compaction
   de-duplicates (paid only after an actual panic).
3. **Persisted CSR skew.** The outgoing and incoming indexes are compacted
   independently, so persistence exporting them with two separate calls could
   capture an edge in one direction's CSR and not the other's; the restore path
   then decided *both* directions' delta from the outgoing set alone, silently
   duplicating that edge in one direction or dropping it from the other.
   Persistence now exports the pair under both compaction locks, and delta
   reconstruction derives each direction from its own CSR. Relatedly, a CSR
   entry whose edge is missing from the persisted edge list is now dropped on
   import instead of being materialized as a phantom edge to node 0.

## References

- [ADR-0026: Incremental CSR Adjacency Index](0026-incremental-csr-adjacency.md)
- Issue #3810 — Background adjacency compaction is never started
- `src/index/adjacency_maintenance.rs`, `src/index/incremental_adjacency.rs`
- `tests/adjacency_maintenance.rs`
