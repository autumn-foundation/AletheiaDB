//! Live-apply wrapper for the replica applier (Issue #3355, Slice B).
//!
//! Models the two existing callers of
//! [`crate::storage::recovery::replay_entries_into_storage_with_constraints`]
//! -- startup replay (`src/db/config.rs`) and PITR's `finish_pitr_replay`
//! (`src/db/pitr.rs`) -- but against a RUNNING replica database instead of one
//! being constructed. `finish_pitr_replay` is the fullest bookkeeping
//! reference: historical write-scoped replay, id-generator `ensure_at_least`
//! advancement, temporal-index rebuild, and an HLC-continuity seed.
//!
//! # Frame-boundary semantics (why this exists, not just a thin wrapper)
//!
//! [`crate::storage::recovery::resolve_transaction_frames`] (private to the
//! recovery module) discards a trailing OPEN `[BeginTx .. CommitTx]` band --
//! correct for crash recovery (an uncommitted tail never happened), but on a
//! replica the same trailing band typically means "not yet arrived": the next
//! poll may well fetch its `CommitTx`. If we simply fed every fetched entry
//! into the recovery replay function we would have no way to tell which
//! *prefix* of the batch it actually applied, so we could never correctly
//! advance `applied_lsn` (the coordinate the next `fetch_entries` call resumes
//! from) -- advancing it past an incomplete frame would let a subsequent
//! `AS OF` read observe a half-applied transaction, exactly the failure mode
//! AC3 forbids.
//!
//! So this module first finds the longest **complete**-frame prefix of the
//! fetched batch itself ([`last_complete_frame_boundary`], mirroring
//! `resolve_transaction_frames`'s own acceptance checks: tx_id match,
//! entry_count match, LSN contiguity, and band brackets), truncates the batch
//! to that boundary, and only THEN hands the truncated (guaranteed-complete)
//! slice to the shared recovery replay engine. `applied_lsn` becomes the LSN
//! of that boundary entry. Any unframed control op or legacy (unframed) data
//! op is immediately complete on its own, matching the recovery resolver's
//! immediate-apply path.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::constraint::ConstraintRegistry;
use crate::core::error::Result;
use crate::core::id::IdGenerator;
use crate::core::temporal::Timestamp;
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::IndexPersistenceManager;
use crate::storage::index_persistence::tracker::PersistenceTracker;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::storage::wal::{LSN, WalEntry, WalOperation};

/// Arc-cloned storage handles the replica applier background thread needs, in
/// order to apply fetched entries without holding a `&AletheiaDB` across the
/// thread boundary.
pub(crate) struct ReplicaStorageHandles {
    pub(crate) current: Arc<CurrentStorage>,
    pub(crate) historical: Arc<RwLock<HistoricalStorage>>,
    #[allow(dead_code)]
    // Wired via `historical` today; retained for parity with the db's field set.
    pub(crate) temporal_indexes: Arc<TemporalIndexes>,
    pub(crate) wal: Arc<ConcurrentWalSystem>,
    pub(crate) node_id_gen: Arc<IdGenerator>,
    pub(crate) edge_id_gen: Arc<IdGenerator>,
    pub(crate) version_id_gen: Arc<IdGenerator>,
    pub(crate) constraint_registry: Arc<ConstraintRegistry>,
    pub(crate) current_timestamp: Arc<crate::core::commit_clock::CommitClock>,
    /// Index persistence, when configured on the replica database.
    pub(crate) persistence: Option<(Arc<IndexPersistenceManager>, Arc<PersistenceTracker>)>,
}

/// Outcome of successfully applying a batch: the new applied position.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AppliedBatch {
    /// The LSN of the last entry actually applied (the frame boundary).
    pub(crate) lsn: u64,
    /// Wallclock (micros) of the latest applied transaction's commit
    /// timestamp, used for lag observability.
    pub(crate) wallclock_micros: i64,
    /// Number of raw WAL entries applied (for periodic-persist accounting).
    pub(crate) entries_applied: u64,
}

/// Apply the longest complete-frame prefix of `pending` to the replica's
/// storage, DRAINING exactly that prefix out of `pending` and leaving any
/// trailing incomplete frame in place. Returns `Ok(None)` when no entry in
/// `pending` completes a frame yet (nothing to apply -- `pending` is left
/// untouched so the caller can append more entries fetched from the next
/// poll and retry).
///
/// # Why `pending` accumulates ACROSS polls, not just within one fetch
///
/// A transport may legitimately return small batches per call (bandwidth
/// caps, a slow/throttled link, or -- as exercised by the torn-frame test --
/// a pathological source that always caps at 1 entry). If a multi-op
/// transaction's `[BeginTx .. data ops .. CommitTx]` band is longer than a
/// single fetch's batch, the applier would never see the whole band in any
/// ONE fetch and would stall forever re-requesting the same incomplete
/// prefix. Accumulating unapplied entries in `pending` across polls (while
/// only ever requesting the NEXT unseen LSN from the source) guarantees
/// eventual convergence regardless of how small the transport's batches are.
pub(crate) fn apply_replica_batch(
    handles: &ReplicaStorageHandles,
    pending: &mut Vec<WalEntry>,
) -> Result<Option<AppliedBatch>> {
    let Some(boundary_idx) = last_complete_frame_boundary(pending) else {
        return Ok(None);
    };

    let entries_applied = (boundary_idx + 1) as u64;
    let boundary_lsn = pending[boundary_idx].lsn.0;
    let max_ts = max_applied_timestamp(&pending[..=boundary_idx]);

    // Replay a CLONE of the complete prefix and only drain it out of
    // `pending` after replay succeeds. Draining first (the original shape)
    // corrupted the applier's bookkeeping on a replay error: the entries
    // were gone but `last_applied` had not advanced, so the next poll's
    // `from_lsn = last_applied + 1 + pending.len()` re-requested the wrong
    // band and the dropped frames were skipped or double-buffered. With
    // drain-on-success, an errored batch stays in `pending` and is retried
    // verbatim next poll: a transient error heals (replay is idempotent per
    // the #3419 guards, so re-applying a half-applied frame is safe), and a
    // deterministic one stalls VISIBLY via `progress.set_error` rather than
    // corrupting the stream. The clone is bounded by one apply batch.
    let truncated: Vec<WalEntry> = pending[..=boundary_idx].to_vec();

    let initial_version_id = handles.version_id_gen.current();
    {
        let mut hist = handles.historical.write();
        // Issue #3788: whole-frame SUBMISSION (the boundary search above) is
        // not reader ISOLATION during apply. The replay below walks the frame
        // operation by operation, and current-state reads take no lock, so
        // without this gate a concurrent `get_node`/`get_edge` could observe
        // one node of a two-node commit. The publish window makes every
        // current-state mutation in this batch land atomically from a gated
        // reader's point of view: they see the batch entirely or not at all.
        //
        // Lock order: taken strictly INSIDE `historical.write()`, and no gated
        // reader ever acquires `historical`, so this adds no cycle. The gate is
        // re-entrant for this thread, which matters because the replay engine's
        // #3419 idempotency guards read current state from inside the window.
        let _publish = handles.current.begin_apply_publish();
        // Detach the live temporal indexes before replaying, so the
        // "close previous version" hooks `replay_entries_into_storage_with_constraints`
        // triggers are no-ops during this pass -- exactly like startup replay
        // and PITR's `finish_pitr_replay`, which both replay into a
        // `HistoricalStorage` whose temporal indexes are not wired yet.
        // Without this, a batch containing 2+ versions of the SAME entity
        // (e.g. a create immediately followed by its own update, landing in
        // one fetch) hits a temporal-index inconsistency: the predecessor
        // version was created moments earlier in this very replay pass and
        // has not been indexed yet (indexing only happens via the rebuild
        // below). See `HistoricalStorage::take_temporal_indexes` for the
        // full rationale (Issue #3355, Slice D chaos test).
        let saved_indexes = hist.take_temporal_indexes();
        let replay_result = crate::storage::recovery::replay_entries_into_storage_with_constraints(
            &handles.wal,
            truncated,
            &handles.current,
            &mut hist,
            initial_version_id,
            Some(&handles.constraint_registry),
        );
        if let Some(indexes) = saved_indexes {
            hist.set_temporal_indexes(indexes);
        }
        let (_final_lsn, max_node_id, max_edge_id, next_version_id) = replay_result?;
        drop(hist);

        // Replay succeeded: NOW drain the applied prefix out of `pending`.
        pending.drain(..=boundary_idx);

        if let Some(m) = max_node_id {
            handles.node_id_gen.ensure_at_least(m + 1);
        }
        if let Some(m) = max_edge_id {
            handles.edge_id_gen.ensure_at_least(m + 1);
        }
        handles.version_id_gen.ensure_at_least(next_version_id);
    }

    // Keep the temporal index consistent with the newly-applied versions so
    // point-in-time reads against the replica see them immediately. A full
    // rebuild per batch is O(total versions), not just the delta just
    // applied -- acceptable for now (mirrors PITR's `finish_pitr_replay`
    // exactly), but an incremental per-version wire-in would scale better for
    // a replica with deep history and a busy primary; left as a perf
    // follow-up for Slice C/D.
    handles
        .historical
        .write()
        .rebuild_temporal_index_from_versions();

    // HLC continuity (mirrors `seed_startup_current_timestamp`): never let a
    // post-promotion write's timestamp regress behind replicated history.
    if let Some(ts) = max_ts {
        // Raises the visibility frontier as well as the allocation frontier:
        // this runs after the batch is applied, so the replicated history it
        // covers is already readable.
        let _ = handles.current_timestamp.raise_to(ts);
    }

    Ok(Some(AppliedBatch {
        lsn: boundary_lsn,
        wallclock_micros: max_ts.map_or(0, |t| t.wallclock()),
        entries_applied,
    }))
}

/// Find the index of the last entry in `entries` that completes a frame:
/// either a legacy/unframed entry (immediate-apply), a self-committing
/// control op, or a `CommitTx` whose `[BeginTx .. CommitTx]` band is valid
/// (matching `tx_id`, `entry_count`, LSN contiguity, and band brackets --
/// mirroring `crate::storage::recovery::resolve_transaction_frames`'s own
/// acceptance checks). Returns `None` when no such entry exists (the batch is
/// empty, or contains only an as-yet-uncommitted open frame).
fn last_complete_frame_boundary(entries: &[WalEntry]) -> Option<usize> {
    let mut last_complete: Option<usize> = None;
    let mut open: Option<(u64, LSN)> = None;
    let mut pending_count: usize = 0;
    let mut pending_first: Option<LSN> = None;
    let mut pending_last: Option<LSN> = None;
    let mut pending_contiguous = true;

    for (idx, entry) in entries.iter().enumerate() {
        if !entry.framed {
            // Legacy (pre-framing) entries apply immediately, same as the
            // recovery resolver.
            last_complete = Some(idx);
            continue;
        }

        match &entry.operation {
            WalOperation::BeginTx { tx_id } => {
                open = Some((*tx_id, entry.lsn));
                pending_count = 0;
                pending_first = None;
                pending_last = None;
                pending_contiguous = true;
            }
            WalOperation::CommitTx {
                tx_id, entry_count, ..
            } => {
                if let Some((open_id, begin_lsn)) = open {
                    let brackets_ok = pending_first == Some(LSN(begin_lsn.0.wrapping_add(1)))
                        && pending_last.map(|l| l.0.wrapping_add(1)) == Some(entry.lsn.0);
                    let valid = open_id == *tx_id
                        && pending_count == *entry_count as usize
                        && pending_contiguous
                        && brackets_ok;
                    if valid {
                        last_complete = Some(idx);
                    }
                }
                open = None;
                pending_count = 0;
                pending_first = None;
                pending_last = None;
                pending_contiguous = true;
            }
            WalOperation::Checkpoint { .. }
            | WalOperation::DeclareUniqueConstraint { .. }
            | WalOperation::DropUniqueConstraint { .. } => {
                // Self-committing control op: discards any torn open frame
                // (defensive, mirrors the recovery resolver) and applies.
                open = None;
                pending_count = 0;
                pending_first = None;
                pending_last = None;
                pending_contiguous = true;
                last_complete = Some(idx);
            }
            _ => {
                if open.is_some() {
                    if let Some(last) = pending_last
                        && entry.lsn.0 != last.0.wrapping_add(1)
                    {
                        pending_contiguous = false;
                    }
                    pending_count += 1;
                    if pending_first.is_none() {
                        pending_first = Some(entry.lsn);
                    }
                    pending_last = Some(entry.lsn);
                } else {
                    // Framed data op with no open frame: the recovery
                    // resolver's documented immediate-apply landmine case
                    // (raw append into a v7+ segment). Immediately complete.
                    last_complete = Some(idx);
                }
            }
        }
    }

    last_complete
}

/// The maximum transaction-commit timestamp among `applied` (a slice already
/// known to end at a complete frame boundary). Used to seed HLC continuity so
/// a promoted replica never issues a new write timestamped behind history it
/// just replicated.
fn max_applied_timestamp(applied: &[WalEntry]) -> Option<Timestamp> {
    let mut max_ts: Option<Timestamp> = None;
    for entry in applied {
        let candidate = match &entry.operation {
            WalOperation::CommitTx {
                commit_timestamp, ..
            } => *commit_timestamp,
            WalOperation::BeginTx { .. } => continue,
            _ => entry.timestamp,
        };
        if max_ts.is_none_or(|m| candidate > m) {
            max_ts = Some(candidate);
        }
    }
    max_ts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMap;

    fn ts(v: i64) -> Timestamp {
        HybridTimestamp::new_unchecked(v, 0)
    }

    fn create_node_op(id: u64) -> WalOperation {
        WalOperation::CreateNode {
            node_id: crate::core::NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern("ApplyTest").unwrap(),
            properties: PropertyMap::new(),
            valid_from: ts(1),
            provenance: None,
        }
    }

    fn framed(lsn: u64, op: WalOperation, timestamp: Timestamp) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp,
            operation: op,
            checksum: 0,
            framed: true,
            segment_version: None,
        }
    }

    fn legacy(lsn: u64, op: WalOperation, timestamp: Timestamp) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp,
            operation: op,
            checksum: 0,
            framed: false,
            segment_version: None,
        }
    }

    #[test]
    fn boundary_none_for_empty_batch() {
        assert_eq!(last_complete_frame_boundary(&[]), None);
    }

    #[test]
    fn boundary_none_for_open_incomplete_frame() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
        ];
        assert_eq!(last_complete_frame_boundary(&entries), None);
    }

    #[test]
    fn boundary_at_commit_tx_for_complete_frame() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 1,
                    commit_timestamp: ts(100),
                },
                ts(12),
            ),
        ];
        assert_eq!(last_complete_frame_boundary(&entries), Some(2));
    }

    #[test]
    fn boundary_stays_at_last_complete_frame_when_tail_is_open() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 1,
                    commit_timestamp: ts(100),
                },
                ts(12),
            ),
            framed(4, WalOperation::BeginTx { tx_id: 2 }, ts(13)),
            framed(5, create_node_op(2), ts(14)),
            // tx 2's CommitTx has not arrived yet.
        ];
        assert_eq!(last_complete_frame_boundary(&entries), Some(2));
    }

    #[test]
    fn boundary_advances_across_multiple_complete_frames() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 1,
                    commit_timestamp: ts(100),
                },
                ts(12),
            ),
            framed(4, WalOperation::BeginTx { tx_id: 2 }, ts(13)),
            framed(5, create_node_op(2), ts(14)),
            framed(
                6,
                WalOperation::CommitTx {
                    tx_id: 2,
                    entry_count: 1,
                    commit_timestamp: ts(200),
                },
                ts(15),
            ),
        ];
        assert_eq!(last_complete_frame_boundary(&entries), Some(5));
    }

    #[test]
    fn boundary_immediate_for_legacy_unframed_entries() {
        let entries = vec![
            legacy(1, create_node_op(1), ts(50)),
            legacy(2, create_node_op(2), ts(60)),
        ];
        assert_eq!(last_complete_frame_boundary(&entries), Some(1));
    }

    #[test]
    fn boundary_rejects_frame_with_entry_count_mismatch() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 5,
                    commit_timestamp: ts(100),
                },
                ts(12),
            ),
        ];
        assert_eq!(last_complete_frame_boundary(&entries), None);
    }

    #[test]
    fn max_applied_timestamp_uses_commit_marker() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 1,
                    commit_timestamp: ts(999),
                },
                ts(12),
            ),
        ];
        assert_eq!(max_applied_timestamp(&entries), Some(ts(999)));
    }

    #[test]
    fn max_applied_timestamp_none_for_empty() {
        assert_eq!(max_applied_timestamp(&[]), None);
    }

    fn create_edge_op(id: u64, source: u64, target: u64) -> WalOperation {
        WalOperation::CreateEdge {
            edge_id: crate::core::id::EdgeId::new(id).unwrap(),
            source: crate::core::NodeId::new(source).unwrap(),
            target: crate::core::NodeId::new(target).unwrap(),
            label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            properties: PropertyMap::new(),
            valid_from: ts(1),
            provenance: None,
        }
    }

    fn test_handles() -> (tempfile::TempDir, ReplicaStorageHandles) {
        use crate::core::constraint::ConstraintRegistry;
        use crate::core::id::IdGenerator;
        use crate::storage::wal::concurrent_system::{
            ConcurrentWalSystem, ConcurrentWalSystemConfig,
        };

        let dir = tempfile::tempdir().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(dir.path().join("wal"));
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let handles = ReplicaStorageHandles {
            current: Arc::new(CurrentStorage::new()),
            historical: Arc::new(RwLock::new(HistoricalStorage::new())),
            temporal_indexes: Arc::new(TemporalIndexes::new()),
            wal,
            node_id_gen: Arc::new(IdGenerator::new()),
            edge_id_gen: Arc::new(IdGenerator::new()),
            version_id_gen: Arc::new(IdGenerator::new()),
            constraint_registry: Arc::new(ConstraintRegistry::new()),
            current_timestamp: Arc::new(crate::core::commit_clock::CommitClock::new(ts(1))),
            persistence: None,
        };
        (dir, handles)
    }

    /// Regression check for the torn-frame-safety integration test: a
    /// replayed `CreateEdge` inside a multi-op frame must update BOTH the
    /// edge map AND the outgoing/incoming adjacency index, atomically with
    /// its endpoint nodes' creation.
    #[test]
    fn apply_replica_batch_updates_adjacency_for_new_edge_in_one_frame() {
        let (_dir, handles) = test_handles();

        let mut pending = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(3, create_node_op(2), ts(12)),
            framed(4, create_edge_op(1, 1, 2), ts(13)),
            framed(
                5,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 3,
                    commit_timestamp: ts(100),
                },
                ts(14),
            ),
        ];

        let applied = apply_replica_batch(&handles, &mut pending).expect("apply must succeed");
        assert!(applied.is_some(), "the complete frame must be applied");
        assert!(pending.is_empty(), "nothing should remain buffered");

        let node_a = crate::core::NodeId::new(1).unwrap();
        assert!(handles.current.get_node(node_a).is_ok());
        assert!(
            handles
                .current
                .get_node(crate::core::NodeId::new(2).unwrap())
                .is_ok()
        );

        let edges = handles.current.get_outgoing_edges(node_a);
        assert_eq!(
            edges.len(),
            1,
            "adjacency must reflect the replayed edge alongside its endpoint nodes"
        );
    }
}
