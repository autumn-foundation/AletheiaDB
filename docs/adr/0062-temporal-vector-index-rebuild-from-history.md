# ADR-0062: Rebuild the Temporal Vector Index from History

**Status:** Accepted
**Date:** 2026-11-28
**Deciders:** AletheiaDB Core Team
**Categories:** vector, temporal, durability
**Related:** Issue #3888, ADR-0017 (temporal vector strategy), ADR-0061

## Context

The temporal vector index (`enable_temporal_vector_index`) is in memory only.
A reopen loses it. Enable after a reopen gives an empty index, so an as-of
vector search before the reopen gives no hits. Historical storage still holds
every version, so the data to rebuild the index is on disk.

## Decision

1. **Backfill at enable.** `enable_temporal_vector_index` reads the vector
   timeline of the property from historical storage and builds the snapshots
   from it. The state at commit `c` is the version visible at
   `(valid = MAX_VALID_TIMESTAMP, tx = c)`: the open head, which is the state
   that current storage holds. Retract and delete close that head, so they
   read as "no vector".
2. **One snapshot per vector change.** Each commit that changes a vector of
   the property gets one snapshot. Only the newest `max_snapshots` change
   points get a snapshot. Older changes are applied without one.
3. **Live state from current storage.** After the replay, the live index is
   set to the vectors in current storage.
4. **No race with commits.** Enable and backfill run under the `historical`
   write lock, which also serializes commits.
5. **Sidecar.** Each enable writes the config to
   `{persistence.data_dir}/temporal_vector_indexes.dat` (bitcode + CRC,
   atomic write). At open, after recovery, the database enables each recorded
   index again (with backfill). A bad file is quarantined. Save merges with
   the file, so a failed restore does not delete an entry.
6. **Idempotent enable.** Enable with an equal config is a no-op. A different
   config is an error.
7. **Current-state index.** If no vector index exists for the property,
   enable uses `rebuild_vector_index` (enable + backfill from current nodes).

## Consequences

- As-of vector search survives a reopen, and an index enabled on old data
  covers its past.
- Open time grows with the hot history size of each temporal index.
- History in the cold tier is not read. Only hot history is replayed.
- A config with a custom metric is not persisted (a closure cannot be stored).
- `TemporalVectorConfig` defaults are unchanged. `max_snapshots` still limits
  how far back an as-of search can go. Use a larger value for long history.

## Rejected alternatives

- **Persist HNSW snapshots.** A large new format that must follow WAL,
  encryption, and key rotation. Possible later, to make open faster.
- **Keep all snapshots by default.** Memory has no limit for all users.
