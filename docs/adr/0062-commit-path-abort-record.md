# ADR-0062: Commit-Path Abort Record

**Status:** Accepted
**Date:** 2026-11-28
**Deciders:** AletheiaDB Core Team
**Categories:** wal, recovery, durability
**Related:** Issue #3799, Issue #3798 (bounded group-commit lock),
Issue #3413 (WAL transaction framing), Issue #3805 (append + register step)

## Context

A commit appends its frame `[BeginTx, ..ops.., CommitTx]` to the WAL before
it waits for durability. Three steps after the append can fail:

| Site | Step | Live state |
|------|------|------------|
| A | `commit()` inside the append step (register timeout, sync flush error) | Not applied |
| B | `apply_changes` | Partly applied, not published |
| C | `wait_for_flush` (timeout, flush error, lock timeout) | Applied and published |

The caller gets `Err`, but the frame can still reach disk, and recovery then
replays it. Before this change, the error always said "durability status
UNKNOWN".

Issue #3413 added `BeginTx` and `CommitTx`. It did not add an abort record.
Some earlier documents said that it did. Those documents are corrected.

## Decision

1. **New WAL record** `AbortTx { tx_id, begin_lsn }` (op tag 14, 16-byte
   payload). It names one band by its `BeginTx` LSN and its transaction id.
   The LSN is the unique key, because transaction ids restart in each process.
2. **Sites A and B write the record.** The commit path first releases the
   commit clock and the changefeed slot. Then it writes the record and waits
   for it with the mode rules. `Synchronous` waits on the record's own
   completion handle. `GroupCommit` waits for the record's epoch.
3. **Site C writes no record.** At site C the writes are already published,
   and the commit clock is already released. Other transactions can read the
   writes and commit on top of them. If recovery dropped the band, those
   transactions would refer to missing data. So the outcome stays UNKNOWN,
   and the error says why.
4. **Two outcomes.** If the record is durable, the error says "Transaction
   aborted: the abort record is durable". Otherwise (write failed, wait
   failed, or `Async`/`AsyncBatched` mode), it says "The durability status is
   UNKNOWN". Only a `StorageError::WalError` gets this text. Other error types
   keep their type, so their classification does not change. For them, the
   outcome goes to the WAL diagnostic log.
5. **Recovery** (`recovery::drop_aborted_frames`) removes every band that a
   framed `AbortTx` names, and the record itself, before frame resolution.
   The position of the record does not matter. Crash recovery, PITR, and the
   replica applier use the same helper.
6. **Divergence fence (site B).** `apply_changes` sets a fence on
   `InFlightLsns` before it releases `historical.write()` and the commit clock.
   While the fence is set:
   - commits refuse with `FailedPrecondition`, so no durable write can depend
     on the disowned writes;
   - `persist_indexes` and `backup` refuse with `FailedPrecondition`. Both
     check the fence under `historical.read()`;
   - the background persistence worker skips each cycle and the shutdown
     persist.
   A reopen clears the fence. Recovery rebuilds the state from the last good
   snapshot and the WAL.
7. **Caller-made vector errors fail early.** A wrong dimension, NaN, or
   infinity used to fail inside `apply_changes`. That would now fence the
   whole database (and an ephemeral database loses its data at reopen). The
   commit path checks every buffered node vector against the enabled vector
   indexes before the WAL append, next to the #3413 guards. Thus such an
   error writes no frame and sets no fence. The fence stays for failures that
   break an internal invariant.
8. **No WAL version bump.** A healthy WAL never holds the record, so its
   bytes do not change. A version bump is not possible without new numbers
   for every format: version 15 is reserved, and version 16 is the
   key-versioned container.

## Downgrade is not safe after an abort record

An older binary does not know tag 14:

- If the record is the last entry, the older binary reads it as a torn tail.
  It replays the aborted band and opens. It can also allocate the record's
  LSN again.
- If valid entries follow the record, the older binary refuses to open
  ("Unknown WAL operation type").

Thus do not downgrade a database whose WAL holds an abort record.

## Windows that stay unknown

- A crash after the frame is durable and before the record is durable.
- An abort record that is not confirmed durable.
- Site C, as described above.
- The background worker persists each index without `historical.read()`, and
  `persist_indexes` writes the vector and temporal-adjacency files from live
  state. Both re-check the fence before each such write and before the
  manifest. A failed apply between the last check and the write can still
  reach that file.
- A replica that applied the band in an earlier batch than the record keeps
  it. The replica logs this, but it does not refuse promotion. Re-bootstrap
  the replica to repair it.
- Live readers see the partly applied writes of site B until reopen.

## Consequences

- A site A or B failure with a durable abort record never comes back after a
  restart or a PITR.
- A failure path can wait for one more flush.
- After a site B failure, the database refuses writes and snapshots until the
  operator reopens it.

## Alternatives rejected

- **Abort at site C.** Later transactions can depend on the published writes.
- **Rewrite the frame in place.** Segments are append-only, and the frame can
  already be on disk.
- **Undo the live apply.** There is no undo log.
- **Apply after flush.** This reverts the group-commit throughput fix.
- **Key the abort by `tx_id` only.** Transaction ids repeat across runs.
