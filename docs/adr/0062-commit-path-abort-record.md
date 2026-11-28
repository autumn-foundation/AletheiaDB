# ADR-0062: Commit-Path Abort Record

**Status:** Accepted
**Date:** 2026-11-28
**Deciders:** AletheiaDB Core Team
**Categories:** wal, recovery, durability
**Related:** Issue #3799, Issue #3798 (bounded group-commit lock),
Issue #3413 (WAL transaction framing), Issue #3805 (append + register step)

## Context

A commit appends its frame `[BeginTx, ..ops.., CommitTx]` to the WAL ring
buffer before it waits for durability. Three steps after the append can fail:

| Site | Step | Live state |
|------|------|------------|
| A | `commit()` inside the append step (register timeout, sync flush error) | Not applied |
| B | `apply_changes` | Partly applied |
| C | `wait_for_flush` (timeout, flush error, lock timeout) | Applied |

The caller gets `Err`. But the frame is in the ring buffer, so the flusher can
still write it, and recovery replays it. Before this ADR the error could only
say "durability status UNKNOWN".

Issue #3413 added `BeginTx`/`CommitTx`. It did not add an abort record. (Some
docs said it did. That was incorrect.)

## Decision

1. **New WAL record** `AbortTx { tx_id, begin_lsn }` (op tag 14, 16-byte
   payload). It names one band by its `BeginTx` LSN and transaction id. The
   LSN makes the key unique: transaction ids restart in each process.
2. **Commit path** writes the record after a failure at A, B, or C, and then
   waits for its durability with the normal mode rules. It releases the commit
   clock first, so a slow wait does not stall other committers.
3. **Two outcomes.** If the record is durable, the error says the transaction
   is aborted and recovery will not replay it. If not (write failed, wait
   failed, or `Async` mode), the error keeps "durability status UNKNOWN".
4. **Recovery** (`recovery::drop_aborted_frames`) removes every band that a
   framed `AbortTx` names, and the record itself, before frame resolution. The
   check does not depend on order. Crash recovery, PITR, and the replica
   applier use the same helper.
5. **Divergence fence.** At B and C the live state holds writes that the WAL
   disowns. The commit path marks the in-flight tracker. While marked,
   `persist_indexes` and `backup` refuse with `FailedPrecondition`. This
   includes the final persist on shutdown. A reopen clears the mark: recovery
   rebuilds state from the last good snapshot plus the WAL.
6. **No WAL version bump.** A healthy WAL never holds the record, so it stays
   byte-identical. An older binary that reads tag 14 stops replay with
   "Unknown WAL operation type" and does not open. It does not misparse or
   drop data. A bump would collide with the reserved v15 gap and the v16
   keyversioned container.

## Remaining windows (unknown by nature)

- **Crash after the frame is durable and before the abort is durable.**
  Recovery replays the frame. The error already said UNKNOWN.
- **Abort record not confirmed.** Same as above.
- **Live readers** see the writes of B/C until the process restarts. This was
  true before this ADR. The fence stops them from reaching a snapshot.
- **Replicas** that applied the band in an earlier batch keep it. A replica
  drops the band only when the band and its abort arrive in one batch.

## Consequences

- An acknowledged-failed commit with a durable abort record never comes back
  after a restart or a PITR.
- A failure path can wait up to one more flush timeout for the abort record.
- After B or C the operator must reopen the database to persist again.

## Alternatives rejected

- **Rewrite the frame in place.** Segments are append-only, and the frame can
  already be on disk.
- **Undo the live apply.** There is no undo log.
- **Apply after flush.** This reverts the group-commit throughput fix.
- **Key the abort by `tx_id` only.** Transaction ids repeat across runs.
