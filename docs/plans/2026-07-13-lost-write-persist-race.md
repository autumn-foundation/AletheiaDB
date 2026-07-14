# Lost-write crash-recovery race between an in-flight commit and index persistence

Date: 2026-07-13
Status: Implemented

## Summary

A durably-acknowledged write could be **silently lost** on crash recovery when
index persistence raced an in-flight commit. The fix records the *applied
frontier watermark* in the index manifest (instead of the WAL allocation
frontier) and captures the graph, temporal, and manifest-LSN as one coherent
snapshot.

## Root cause

Two compounding bugs.

### Bug 1 — manifest records the allocation frontier, not the applied frontier

The commit path (`src/api/transaction/write/mod.rs::commit_with_timestamp_inner`)
makes a write **durable** before it is **applied**, and no lock spans both:

1. Under the `current_timestamp` lock it allocates LSNs + WAL-appends
   (`log_operations_to_wal`), fsyncs (`wal.commit()`), and (GroupCommit) waits
   for the flush — the write is now durable and acknowledged.
2. The `current_timestamp` lock drops.
3. Only *then* does it acquire `historical.write()` and call
   `apply::apply_changes` + `finalize_current_commit_timestamps` — the write
   becomes visible in the in-memory current/historical stores.

`persist_indexes` (`src/db/admin.rs`) stamped `manifest.lsn =
wal.current_lsn()` — the **allocation frontier** (next-to-allocate). Startup
replay (`src/db/config.rs`) drops WAL entries with `lsn < manifest.lsn`
(inclusive-from-manifest, #3419). So a write that was **fsynced** (its
`lsn < frontier`) but **not yet applied** when the snapshot was taken was BOTH
absent from the persisted snapshot AND dropped by replay — a net lost,
already-acknowledged write.

### Bug 2 — torn snapshot (graph vs temporal observe different instants)

`persist_graph_index` scanned live `current.all_nodes()` with **no lock**, while
`persist_temporal_index` scanned under `historical.read()`. Under a concurrent
writer the two scans observed different instants, so recovery could restore
e.g. 85 graph nodes against 86 temporal versions.

### Key safety fact the fix relies on

Startup replay is **idempotent**, keyed by `version_id` (`src/storage/recovery.rs`,
`src/storage/historical/mod.rs`). Re-applying an already-applied op reuses the
same `version_id` and is a no-op. So replaying an *overlap band* `[watermark,
frontier)` — re-applying writes that DID make it into the snapshot — never
duplicates history.

## Approaches considered

1. **Watermark-only.** Record `min(in-flight LSN)` in the manifest; leave the
   graph/temporal scans as-is. Fixes Bug 1 but leaves Bug 2 (torn snapshot): the
   manifest could be consistent while the graph and temporal indexes still
   disagree by one under concurrency. Rejected as incomplete.

2. **Drain barrier.** Quiesce all in-flight commits (drain to zero) before
   persisting. Correct but couples persistence latency to write latency and can
   stall persistence indefinitely under sustained write load. Rejected.

3. **Watermark + coherent barrier (chosen).** Record the applied watermark AND
   capture the graph, temporal, and manifest-LSN as ONE atomic observation under
   a short in-memory barrier, then serialize off-lock from immutable snapshots
   (exactly the proven checkpoint/backup pattern). Fixes both bugs; the
   persistence critical section holds locks only across in-memory clones, never
   across disk I/O.

## Implementation (Approach 3)

### (1) In-flight LSN tracker → applied watermark

- `src/api/transaction/write/in_flight.rs`: `InFlightLsns` wraps a
  `Mutex<BTreeSet<u64>>` with `register(lsn) -> InFlightGuard`, `min()`, and
  RAII `deregister` on guard drop. Shared on the DB as
  `Arc<InFlightLsns>` (`src/db/mod.rs`), threaded to each `WriteTransaction`
  (`with_in_flight_tracker`).
- Commit path: `log_operations_to_wal` now returns the base (lowest) LSN of the
  commit's contiguous band. The commit **registers** that LSN AFTER the WAL
  append allocates it but BEFORE the durability fsync (so a durable write is
  always registered), holding the `InFlightGuard` in the outer function scope so
  it is deregistered only AFTER `apply_changes` + finalize (success path) or on
  any early-return / panic path.
- Invariant: a *deregistered* LSN is guaranteed present in any
  current+historical snapshot (deregister strictly follows finalize); a
  *registered* LSN's `lsn >= manifest.lsn`, so replay re-covers it.

### (2) Coherent snapshot in `persist_indexes` (`src/db/admin.rs`)

Under a short barrier that mirrors `backup.rs`'s proven lock order —
`current_timestamp` (class 1) → `historical.read()` (class 3) →
`current.snapshot_lock.write()` — it captures:

- `manifest_lsn = in_flight.min().unwrap_or(frontier)` (when nothing is in
  flight this equals today's frontier — idle persist keeps the identical
  manifest LSN);
- a coherent `current.create_snapshot()` and `historical.create_snapshot()`.

Then it **releases** the locks and serializes off-lock via new snapshot-based
functions `persist_graph_index_from_snapshot` /
`persist_temporal_index_from_snapshot`
(`src/storage/index_persistence/operations.rs`). The graph snapshot carries no
CSR adjacency arrays; the loader rebuilds adjacency via `compact_adjacency()`
when they are absent (identical to the checkpoint path).

Why `current_timestamp` is held across the frontier+min read: a commit's LSN
band is allocated (bumping the frontier) INSIDE `append_batch`, but its in-flight
registration happens just AFTER the append returns — both under the commit's own
`current_timestamp` hold. Reading the frontier without `current_timestamp` could
observe a frontier already advanced past such a commit while `in_flight.min()`
does not yet see it, leaving the manifest above a soon-to-be-durable write.
Holding `current_timestamp` guarantees no commit is between allocation and
registration, so the frontier and the in-flight set are mutually consistent.

### Lock ordering / deadlock analysis

- `InFlightLsns`'s mutex is a **leaf**: held only for a single set insert /
  remove / min, never while acquiring another primitive.
- `persist_indexes` acquires `current_timestamp` (1) → `historical.read()` (3) →
  `snapshot_lock.write()`, in CLAUDE.md order. Apply-phase commits hold
  `historical.write()` but NOT `current_timestamp` (dropped before apply), so
  persist waiting on `historical.read()` cannot deadlock against a commit; a
  commit waiting on `current_timestamp` cannot deadlock against persist (persist
  waits only on `historical`, released by the applying commit). The
  `historical.read()` → `snapshot_lock.write()` order matches backup/checkpoint,
  so no AB-BA with the commit path's `historical.write()` →
  `snapshot_lock.read()`.

Replay semantics (`src/db/config.rs`) are unchanged (still inclusive-from-manifest
per #3419); only the manifest LSN it reads is now the applied watermark.

## Follow-ups / out of scope

Two known follow-ups remain out of scope for this change:

- **(F7) Snapshot-coherent vector index.** Vector indexes are persisted from
  LIVE current storage — AFTER the coherence barrier is released — rather than
  from the coherent snapshot, so a graph-vs-vector torn snapshot is possible on
  recovery. This is NOT a lost write (replay re-indexes every entry with
  `lsn >= manifest_lsn`), but an entry that is in the vector file AND also
  replayed can get a double HNSW insert. Fix options: snapshot the vectors under
  the same barrier, or gate loaded vector entries by `lsn <= manifest` on
  restore. The reported torn-snapshot symptom is graph-vs-temporal (both fixed by
  the coherence barrier above).

- **(F2) Abort-after-fsync WAL-framing replay.** A commit that crashes between
  WAL fsync and in-memory apply, or the narrow crash-during-commit-flush window,
  is bounded by the pre-existing lack of WAL transaction framing (Issues #3413 /
  #3416). Durable WAL transaction framing so partial/aborted commits are cleanly
  skipped on replay is tracked there, not here.

### Note on the `current_timestamp` hold (addressed)

`persist_indexes` now holds `current_timestamp` (lock-order class 1, the top of
every commit) ONLY for the O(log n) frontier + `in_flight.min()` watermark read,
then RELEASES it before the two O(N) snapshot clones — so a background persist no
longer stalls all writers across the deep clones. Releasing early is safe: the
manifest LSN equals `min(in-flight bases at T0)`, every write with
`lsn < manifest_lsn` was already applied at T0 and is present in the coherent
snapshot taken at T1 > T0 under `historical.read()`, and every durable write not
in the snapshot has `lsn >= manifest_lsn` and is re-applied idempotently by
inclusive replay. The frontier / `in_flight.min()` are not re-read after release.

## Test / risk matrix

| Risk | Guard |
|------|-------|
| Durable-but-unapplied write lost on crash | `test_a_durable_unapplied_write_survives_persist_then_crash` (deterministic seam) |
| Overlap-band replay duplicates an in-snapshot write | `test_b_overlap_band_replay_does_not_duplicate` (deterministic seam) |
| Racy write vs repeated persist, lost or duplicated | `lsn_recovery_regression::t7` (0/500 under `nproc` CPU load; baseline 2/300) |
| Manifest off-by-one / LSN allocator restart / truncation | existing `lsn_recovery_regression` T1–T11 (all pass) |
| Torn graph/temporal snapshot | coherent barrier; T7 under load |
| Idempotent replay | overlap-band Test B + existing `recovery::tests` |
| Lock-order regression | full suite + T7 stress; analysis above |

`tests/lost_write_persist_race.rs` forces the race deterministically via an
always-compiled one-shot commit seam
(`api::transaction::write::race_seam`) that parks exactly one commit at the
durable-but-not-applied point while the test drives `persist_indexes()`. The
seam is a single acquire atomic load when unarmed (zero production cost), and its
arming surface is compiled only under the `testing` feature (gated out of
default builds, so the commit path cannot be parked in production).
