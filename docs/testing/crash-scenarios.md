# Crash-Scenario Test Reference

> Tracking issue: [#453](https://github.com/madmax983/AletheiaDB/issues/453) —
> meta-issue for crash-recovery test coverage.

This document catalogs the crash-, corruption-, and recovery-scenario tests that
guard AletheiaDB's durability guarantees. It is a **reference index**: for each
scenario it names the test(s) (file + symbol), what failure is simulated, the
invariant asserted, and how to run it. Use it to answer "is failure mode X
covered, and by which test?" and to find the "gaps / not-yet-covered" list when
adding new coverage.

It is intentionally accurate to the tests that exist in-tree; every test named
here is real. When you add or move a crash test, update this file (and the
per-scenario counts).

## How crash recovery works (in one paragraph)

AletheiaDB is crash-safe via a **write-ahead log (WAL)** plus periodic
**index-persistence snapshots / checkpoints**. Every committed operation is
appended (and, per durability mode, fsynced) to the WAL before it is
acknowledged. On restart the database loads the most recent snapshot/checkpoint
and then **replays** the WAL forward from the snapshot's LSN to reconstruct the
exact pre-crash state (current + bi-temporal history). Two boundary behaviors
matter for the scenarios below:

- **Torn tail is tolerated.** A crash can leave the *final* WAL entry
  partially written (truncated payload, or a full header followed by an
  unwritten/garbage op-type byte). Recovery discards that never-acknowledged
  trailing entry and keeps every prior intact entry, so `AletheiaDB::open` still
  succeeds. A strict/fail-stop policy is available as an opt-out.
- **Mid-log corruption is fail-stop.** Corruption that is *not* a torn tail
  (an intact-looking but wrong entry in the middle) is treated as
  `CorruptedData` rather than silently skipped, so a partial/prefix replay never
  masquerades as success.

Transaction **framing** (begin/commit markers with an entry count) makes a
multi-write transaction atomic across replay: an uncommitted batch prefix left
by a crash-during-commit-flush is discarded, and one transaction's versions all
receive a single transaction timestamp.

See [docs/ARCHITECTURE.md](../ARCHITECTURE.md) for the bi-temporal storage model,
[docs/WAL.md](../WAL.md) for WAL internals, and
[tests/recovery/README.md](../../tests/recovery/README.md) for the replay-suite
overview.

## Crash-scenario reference table

| Scenario | Test(s) — `file::symbol` | Simulates | Invariant asserted | Run |
|---|---|---|---|---|
| **Torn WAL tail (default: tolerate)** | `tests/wal_torn_tail_replay.rs::open_recovers_from_zeroed_optype_torn_tail`, `::open_default_policy_tolerates_torn_tail_via_unified_config`, `::wal_config_default_tolerates_torn_tail`; `tests/lsn_recovery_regression.rs::t11_constructor_tolerates_torn_wal_tail` | Full 24-byte header followed by unwritten/zeroed op-type byte at end of final segment; snapshot deleted so reopen must replay from LSN 1 | `open` recovers; every prior committed write reappears; the torn (never-acked) entry is dropped | `cargo test --test wal_torn_tail_replay` |
| **Torn WAL tail (fail-stop opt-out)** | `tests/wal_torn_tail_replay.rs::open_fail_stop_errors_on_torn_tail_when_opted_out` | Same torn tail, strict policy selected | Replay errors (`CorruptedData`) instead of silently truncating | `cargo test --test wal_torn_tail_replay open_fail_stop` |
| **Mid-log / structured corruption** | Fuzz target `wal_replay` (see [TESTING.md](../../TESTING.md#fuzz-testing)); torn-tail fail-stop test above pins the non-tail error path | Coverage-guided WAL mutation streams replayed on recovery | No panic / no silent prefix-accept; corruption is surfaced, not skipped | `just fuzz-run wal_replay` |
| **Transaction framing / atomic batch** | `tests/wal_tx_framing.rs::replay_discards_uncommitted_batch_prefix`, `::replay_keeps_fully_committed_batch`, `::committed_tx_kept_following_uncommitted_tx_discarded`, `::torn_partial_commit_marker_keeps_prior_tx`, `::single_op_tx_is_framed`, `::committed_tx_has_trailing_commit_marker_with_entry_count`, `::interleaved_concurrent_txs_each_atomic`, `::atomic_batch_shares_single_transaction_time`, `::distinct_batches_have_distinct_transaction_times`, `::test_commit_timestamp_matches_live_commit`, `::test_empty_tx_writes_no_marker` (Issue #3413) | Crash during commit-flush leaves a batch *prefix*; replay must accept whole-committed batches only | All-or-nothing per transaction; one transaction time per batch; `AS OF SYSTEM_TIME` never observes a half-batch | `cargo test --test wal_tx_framing` |
| **Checkpoint + WAL replay** | `tests/recovery/checkpoint_recovery_tests.rs::test_checkpoint_recovery_basic`, `::test_checkpoint_with_persisted_state_and_wal_replay`, `::test_checkpoint_recovery_preserves_edges` (16 tests); `tests/recovery/replay_loop_tests.rs::test_recover_from_checkpoint_lsn`, `::test_recover_handles_checkpoint_marker` | Persisted snapshot at checkpoint LSN, then WAL replay of entries after the checkpoint | State after checkpoint + differential replay == full-replay state; LSN consistency; edges preserved | `cargo test --test recovery checkpoint_recovery_tests` |
| **Full DB crash + reopen (no checkpoint)** | `tests/wal_recovery_integration.rs` (Issue #365); `tests/regression_wal_replay.rs::test_repro_data_loss_missing_wal_replay` | Durable write, drop DB without checkpoint, reopen with full WAL replay from LSN 0 | Current reads, bi-temporal history (version counts, valid/tx bounds), and point-in-time reads all identical to pre-crash | `cargo test --test wal_recovery_integration` |
| **Durability: Synchronous crash** | `tests/lsn_recovery_regression.rs::t1_first_post_persist_write_survives_crash` … `::t10_deleted_node_id_is_never_reissued_across_restart`, `::t3429_below_manifest_constraint_and_tail_survive_single_pass_startup` (12 tests, `std::mem::forget(db)` crash) | Process-kill after fsync under `DurabilityMode::Synchronous` | Every acked/fsynced write survives; no manifest off-by-one skip (#3419); LSN monotonic across sessions (#3420) | `cargo test --test lsn_recovery_regression` |
| **Durability: GroupCommit crash** | `tests/wal_group_commit_recovery.rs::gc1_acknowledged_writes_survive_group_commit_crash`, `::gc2_persist_between_two_writes_boundary`, `::gc3_lsn_allocation_continues_across_group_commit_sessions` (Issue #3430) | `mem::forget` after a GroupCommit batch fsync returns Ok (the documented `open()` default mode) | Every write whose batch flush was acknowledged survives; #3419/#3420 invariants hold under batched fsync | `cargo test --test wal_group_commit_recovery` |
| **Encrypted-WAL crash recovery** | `tests/wal_group_commit_recovery.rs::encrypted::e1_encrypted_wal_allocator_seeded_across_sessions`, `::encrypted::e2_encrypted_segment_without_cipher_scan_skips_but_read_errors` (Issue #3420 encrypted variant) | Encrypted WAL segments scanned at startup to seed the LSN allocator; restart | Allocator seeded past encrypted segments' max LSN, no LSN reuse; documented leniency when a cipher is absent | `cargo test --test wal_group_commit_recovery encrypted` |
| **LSN / manifest / ID-generator recovery** | `tests/recovery/replay_id_tracking_tests.rs` (8, Issue #291); `tests/recovery/tombstone_version_id_tests.rs` (10); `tests/lsn_recovery_regression.rs::t8_manifest_lsn_floor_survives_wal_truncation`, `::t9_recovery_and_seeding_across_rotated_segments` | Restart across rotated/truncated segments and manifest floor | ID generators reinit to `max_observed+1` (no reuse, gaps handled); manifest LSN floor honored; deleted ids never reissued | `cargo test --test recovery replay_id_tracking_tests` |
| **Op-type replay (create/update/delete)** | `tests/recovery/replay_create_tests.rs` (10, #288), `tests/recovery/replay_update_tests.rs` (10, #289), `tests/recovery/replay_delete_tests.rs` (10, #290) | Replay of each WAL op type incl. version chains, tombstones, vector (de)indexing | Post-replay current + historical state (labels, props, version chains, tombstones, time-travel) matches pre-crash | `cargo test --test recovery replay_delete_tests` |
| **Auth / provenance survival across crash** | `tests/auth_recovery.rs::recovery_preserves_data_provenance_principal_and_key_store`, `::crash_recovery_via_wal_replay_preserves_provenance_principal_and_key_store` (#3350 Ph3); `tests/destructive_provenance_recovery.rs::crash_recovery_preserves_delete_and_retract_provenance_principal`, `::crash_recovery_cascade_delete_stamps_provenance_on_co_deleted_edges` (#3427 Ph B) | Reopen forced to reconstruct from WAL alone (indexes not persisted) | Provenance principal on both write and *destructive* (delete/retract/cascade) versions survives replay; auth key store reloads, revocations persist | `cargo test --test auth_recovery --features mcp-server`; `cargo test --test destructive_provenance_recovery` |
| **Property-based recovery invariants** | `tests/recovery/property_based_tests.rs` (proptest, Issue #295) | Randomized valid operation sequences, recover, re-check | Temporal consistency, version-chain integrity, and related invariants hold for any valid sequence | `cargo test --test recovery property_based_tests` |
| **WAL batch gaps / no-residue** | `tests/havoc/havoc_wal_gaps.rs::test_havoc_wal_batch_gaps`, `::test_havoc_wal_batch_last_entry_failure_no_residue`, `::test_havoc_wal_batch_no_residue_on_replay` | Partial/failed batch append leaving potential residue | No LSN gaps; a failed batch leaves no replayable residue | `cargo test --test havoc test_havoc_wal_batch` |
| **Sharded / distributed (2PC) recovery** | `tests/sentry_shard_recovery.rs::test_shard_recovery_data_loss_repro` | Shards marked unavailable after logging to force 2PC commit failure | No data loss / consistent state after coordinator commit failure | `cargo test --test sentry_shard_recovery` |
| **Fault injection: flush I/O error** ⚠️ *uid-0 caveat* | `tests/regression_flush_corruption.rs::test_metadata_corruption_on_error`; `tests/havoc/havoc_flush_deadlock.rs::test_flush_deadlock_on_io_error` | WAL dir made read-only (`set_mode(0o444)`) to force I/O error on segment/metadata write | No deadlock, no metadata corruption when flush fails; error surfaced cleanly | `cargo test --test regression_flush_corruption`; `cargo test --test havoc test_flush_deadlock_on_io_error` |
| **Large-dataset recovery (perf/durability)** | `tests/recovery/large_dataset_recovery.rs` (6, `#[ignore]`, Issue #294) | 10K nodes / 50K edges replay | Recovery completes correctly under target time budget (~<10s) | `cargo test --test recovery large_dataset -- --ignored` |

**Implementation under test:** the replay/recovery logic lives in
`src/storage/recovery.rs` (WAL entry replay, "resurrection"),
`src/storage/checkpoint.rs` (checkpoint create/load, LSN tracking), and the WAL
subsystem under `src/storage/wal/`. Those modules also carry `#[cfg(test)]`
unit tests exercised by the crates' normal `cargo test` run.

## Scenario groups (detail)

### Torn tail vs. mid-log corruption
The default recovery policy **tolerates a torn tail** — the never-acknowledged
final entry from a crash mid-append is dropped and `open` proceeds
(`tests/wal_torn_tail_replay.rs`, `lsn_recovery_regression::t11`,
`::t3429_...`). A strict/**fail-stop** policy is available and pinned by
`open_fail_stop_errors_on_torn_tail_when_opted_out`. Corruption that is **not**
a torn tail must be surfaced as `CorruptedData`, never silently prefix-accepted;
the coverage-guided `wal_replay` fuzz target
([TESTING.md](../../TESTING.md#fuzz-testing)) drives structured mid-stream
mutations against replay.

### Checkpoint + replay
`tests/recovery/checkpoint_recovery_tests.rs` and the checkpoint-marker cases in
`replay_loop_tests.rs` verify the two-stage recovery path: load the snapshot at
its checkpoint LSN, then differentially replay WAL entries after it, and confirm
the result equals a full replay (edges included, LSN consistent).

### Durability-mode crashes
Recovery is exercised under **Synchronous** (`lsn_recovery_regression.rs`, 12
tests) and **GroupCommit** (`wal_group_commit_recovery.rs`, the `open()`
default). Both simulate a real crash with `std::mem::forget(db)` so `Drop`/final
persist never runs, and both use *inert* persistence policies so a leaked
background worker cannot persist "post-crash" and invalidate the simulation.

### Transaction framing
`tests/wal_tx_framing.rs` (Issue #3413) pins begin/commit framing: a crash that
persists only a batch *prefix* is discarded on replay, a fully-committed batch is
kept, and all versions of one transaction share a single transaction timestamp
(no timestamp bisection under `AS OF SYSTEM_TIME`).

### Encrypted WAL
The `encrypted` module in `wal_group_commit_recovery.rs` writes an encrypted
WAL, restarts, and asserts the LSN allocator is seeded past the encrypted
segments' max LSN with no reuse — plus the documented leniency discrepancy when
an encrypted segment exists but no cipher is configured.

### Auth / provenance survival
`tests/auth_recovery.rs` and `tests/destructive_provenance_recovery.rs` force a
WAL-only reconstruction (indexes intentionally not persisted) and assert that the
provenance **principal** survives on both ordinary writes and *destructive*
(delete / retract / cascade) versions, and that the auth key store reloads with
revocations intact.

### LSN / idempotency / ID-generator recovery
`replay_id_tracking_tests.rs`, `tombstone_version_id_tests.rs`, and the manifest
/ segment-rotation cases in `lsn_recovery_regression.rs` ensure ID generators
reinitialize to `max_observed + 1`, deleted ids are never reissued across a
restart, and the manifest LSN floor and rotated-segment seeding stay correct
(guards against #3419 off-by-one and #3420 allocator-restart bugs).

### Fault injection (uid-0 caveat)
`tests/regression_flush_corruption.rs::test_metadata_corruption_on_error` and
`tests/havoc/havoc_flush_deadlock.rs::test_flush_deadlock_on_io_error` inject a
flush/IO failure by making the WAL directory read-only via
`std::fs::set_permissions(..., mode 0o444)` and asserting the flush path fails
cleanly (no deadlock, no metadata corruption).

> ⚠️ **These two tests do not fail cleanly when the test process runs as `root`
> (uid 0).** The kernel lets uid 0 bypass directory write permissions, so the
> intended I/O error never occurs and the test's expectation is not met. Run the
> suite as a non-root user. (Recorded sandbox limitation:
> `aletheiadb-sandbox-limits`.)

## Gaps / not-yet-covered

These are scenarios with no dedicated crash test today, called out so future
coverage work has a target. (Add a row to the table above when one lands.)

- **Async durability crash boundary.** Dedicated crash-recovery tests exist for
  `Synchronous` and `GroupCommit`, but there is no analogous test that crashes
  under `DurabilityMode::Async` and asserts the *eventual*-durability contract
  (writes not yet background-fsynced may be lost; those that were must survive).
- **Explicit non-tail mid-log corruption assertion.** Mid-stream corruption is
  covered by the `wal_replay` fuzzer and implied by the fail-stop opt-out test,
  but there is no fixed integration test that plants a corrupt entry *before*
  the tail and asserts fail-stop with a specific error at a specific LSN.
- **Checkpoint written but WAL truncated/lost.** Recovery from a checkpoint plus
  replay is covered; the inverse (checkpoint present, subsequent WAL segment
  missing or truncated below the manifest floor) is only partially probed by
  `t8_manifest_lsn_floor_survives_wal_truncation`.
- **Crash during checkpoint/snapshot write.** No test simulates a crash *while a
  checkpoint/index snapshot is being written* (partial snapshot file) and
  asserts the reopen falls back to the previous snapshot + full WAL replay.
- **Cold-tier (Redb) crash consistency.** Recovery tests target the hot tier +
  WAL; a crash mid-migration to the cold Redb tier, and recovery of the merged
  extent, is not directly exercised here.
- **Concurrent recovery / reads during replay.** No test drives reads or writes
  concurrently with an in-progress replay.

## Related

- [tests/recovery/README.md](../../tests/recovery/README.md) — replay-suite
  overview and per-module breakdown.
- [docs/WAL.md](../WAL.md) — WAL architecture and durability modes.
- [docs/ARCHITECTURE.md](../ARCHITECTURE.md) — bi-temporal storage and recovery flow.
- [TESTING.md](../../TESTING.md) — running tests, coverage, and the `wal_replay`
  fuzz target.
