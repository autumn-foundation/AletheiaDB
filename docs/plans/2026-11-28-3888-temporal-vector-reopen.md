# Plan: Temporal Vector Index Survives Reopen (Issue #3888)

## Problem

The temporal vector index is in memory only. After a reopen:

1. The index is not enabled. `at_time(t)` search fails.
2. If the caller enables it again, it is empty. It does not use the history
   that historical storage still holds. `at_time(t)` before the reopen gives
   no hits.

## Brainstorming (options)

| # | Option | Note |
|---|--------|------|
| A | Persist all HNSW snapshots to disk | New large file format. Must stay in step with WAL, encryption, rotation. |
| B | Rebuild snapshots from history at enable | History does not change, so it is the correct source. Also covers data written before enable. |
| C | Record the enabled config; enable again at open | Small. Alone it gives an empty index. |
| D | Change `default_with_hnsw` to keep all snapshots | Unbounded memory for all users. |
| E | Rebuild in `rebuild_vector_index` | Wrong place: that call is for the current-state index. |

## Reverse brainstorming (how can the fix fail?)

| Way to fail | Control |
|-------------|---------|
| A commit runs during the backfill. Its vector is lost or applied two times. | Hold the `historical` write lock (the commit guard) for enable + backfill. |
| Backfill reads a structural or backfill version as the current state. | Read the state at `(valid = far future, tx = commit)`. That is the open head, the same state current storage holds. |
| Retract and delete are not seen. | They close the head at far-future valid time, so the read gives "no vector". |
| Backfill builds one HNSW per change. Memory and time grow without limit. | Build only the newest `max_snapshots` change points. Apply older changes without a snapshot. |
| A bad sidecar stops the database from opening. | Move it aside (`*.dat.corrupt-<micros>`), warn, and continue. |
| A failed restore deletes the config from the sidecar. | Merge on save: keep entries on disk that are not in memory. |
| Apps that call enable at each start get "already enabled". | Enable with the same config is a no-op. A different config is an error. |
| History holds a vector with the wrong dimension or NaN. | Skip it. The live index can never hold it. |
| The sidecar needs `serde`, so `default-features = false` loses it. | Use bitcode + CRC, which is always compiled. |
| Enable creates an empty current-state index that persistence then writes over the good files. | When the vector index is missing, use `rebuild_vector_index` (enable + backfill). |
| The persisted HNSW config has a custom metric (a closure). | It cannot be stored. Skip that entry and warn. |

## Six thinking hats

- **Facts (white hat):** Hot storage (`node_versions`) holds the open head of
  each node. Closed versions can move to the cold tier. Snapshot builds read
  only `current_state.vectors`.
- **Feelings (red hat):** A user expects an as-of search to work after a
  reopen, as every other temporal read does.
- **Risks (black hat):** Open time increases with history size. The live
  snapshot at an anchor time `t` holds the state before the write at `t`. A
  rebuilt snapshot at `t` holds the state after it.
- **Benefits (yellow hat):** No new index format. An index that you enable on
  old data covers its past. Snapshots occur at real vector changes, not at
  each anchor.
- **Ideas (green hat):** Later, persist snapshots (option A) to make open
  faster.
- **Process (blue hat):** Write failing tests first. Then make them pass.
  Then clean up. Then review from several angles.

## Decision

Do B + C. See [ADR-0062](../adr/0062-temporal-vector-index-rebuild-from-history.md).

## Invariants (spec)

- **I1:** After enable, for each kept snapshot time `t`, the snapshot holds
  exactly the vectors that current storage held after the last commit `<= t`.
- **I2:** After enable, the live index holds exactly the vectors in current
  storage.
- **I3:** At most `max_snapshots` snapshots exist.
- **I4:** Enable with an equal config is a no-op. It does not change the index.
- **I5:** A reopen enables each index that the sidecar records, with I1 and I2.

Verus is not set up in this repository. A property test checks I1 and I2
against a model for random write sequences.

## Steps

1. Failing tests first: integration tests (issue reproduction, enable on old data,
   idempotent enable, corrupt sidecar), unit tests (backfill, timeline,
   sidecar), property test.
2. Make them pass: `TemporalVectorIndex::backfill`, `HistoricalStorage::node_vector_changes`,
   sidecar module, restore at open, enable changes.
3. Clean up: docs (CLAUDE.md, guide, `rebuild_vector_index`, config defaults).
4. Review from several angles. Fix the findings.
