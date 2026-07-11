//! WAL recovery logic.
//!
//! This module provides the shared logic for replaying Write-Ahead Log (WAL) entries
//! back into the database's storage engines. It acts as the bridge between durable
//! disk storage and the in-memory/disk-backed data structures.
//!
//! # The "Resurrection" Process
//!
//! When AletheiaDB experiences an ungraceful shutdown (a crash or power failure),
//! the data in memory is lost. To ensure data durability (the 'D' in ACID), every
//! committed transaction is first written to the WAL on disk.
//!
//! Upon restart, this module reads those persisted WAL entries and "replays" them.
//! It reconstructs the exact state of the database by systematically applying every
//! recorded `Create`, `Update`, and `Delete` operation to both the `CurrentStorage`
//! (the hot, current state of the graph) and the `HistoricalStorage` (the temporal,
//! versioned state of the graph).
//!
//! # Why is it needed?
//!
//! Replaying the WAL is necessary because building the database's optimized read
//! structures (like the CSR adjacency matrices or vector indexes) is expensive and
//! typically done in memory. Instead of syncing these complex structures to disk on
//! every write, we sync a simple, sequential log of operations. The recovery process
//! uses this log to rebuild the complex structures exactly as they were.

use crate::core::constraint::ConstraintRegistry;
use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::{TxId, VersionId};
use crate::core::temporal::Timestamp;
use crate::core::version::VersionMetadata;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::storage::wal::{LSN, WalEntry, WalOperation};

/// Replay WAL entries starting from a given LSN into existing storage instances.
///
/// # Arguments
///
/// * `wal` - The concurrent WAL system to read entries from
/// * `current` - Current storage to apply updates to
/// * `historical` - Historical storage to apply updates to
/// * `start_lsn` - The LSN to start replaying from (inclusive)
/// * `next_version_id` - The starting version ID for new versions created during replay
///
/// # Returns
///
/// A tuple containing:
/// - The final LSN reached after replay (current LSN of the WAL)
/// - The maximum node ID observed (if any)
/// - The maximum edge ID observed (if any)
/// - The next version ID to use (updated after replay)
///
/// # Examples
///
/// ```rust,ignore
/// # use tempfile::tempdir;
/// # use std::sync::Arc;
/// # use aletheiadb::WalConfigBuilder;
/// # use aletheiadb::storage::wal::{LSN, WalOperation};
/// # use aletheiadb::storage::wal::concurrent_system::ConcurrentWalSystem;
/// # use aletheiadb::storage::current::CurrentStorage;
/// # use aletheiadb::storage::historical::HistoricalStorage;
/// # // Note: This function is pub(crate) and generally called internally by
/// # // AletheiaDB::new() or CheckpointManager::recover().
/// # use aletheiadb::core::id::{NodeId, VersionId, TxId};
/// # use aletheiadb::core::interning::InternedString;
/// # use aletheiadb::core::property::PropertyMap;
/// # use aletheiadb::core::temporal::Timestamp;
/// # use aletheiadb::core::GLOBAL_INTERNER;
/// # fn main() -> aletheiadb::core::error::Result<()> {
/// // 1. Set up a temporary directory and WAL system
/// let dir = tempdir().unwrap();
/// let mut config = WalConfigBuilder::new().build();
/// config.wal_dir = dir.path().to_path_buf();
/// let wal = ConcurrentWalSystem::new(&config)?;
///
/// // 2. Simulate a crash: Write a raw entry directly to the WAL
/// let node_id = NodeId::new(1).unwrap();
/// let label = GLOBAL_INTERNER.intern("User").unwrap();
/// let ts = Timestamp::from(100);
///
/// let op = WalOperation::CreateNode {
///     node_id,
///     label,
///     properties: PropertyMap::default(),
///     valid_from: ts,
/// };
/// wal.append(op)?; // Append takes just the operation, timestamp is assigned by WAL
/// wal.flush()?;
///
/// // 3. The Resurrection: Create empty storage instances
/// let current = CurrentStorage::new();
/// let mut historical = HistoricalStorage::new();
///
/// // 4. Replay the WAL into storage
/// let start_lsn = LSN::initial();
/// let initial_version_id = 1;
///
/// // Internally, AletheiaDB calls this during initialization
/// let (final_lsn, max_node, _, next_vid) = replay_wal_into_storage(
///     &wal,
///     &current,
///     &mut historical,
///     start_lsn,
///     initial_version_id
/// )?;
///
/// // 5. Verify the state was reconstructed
/// assert_eq!(max_node, Some(1));
/// assert!(current.get_node(node_id).is_ok(), "Node was recovered!");
/// # Ok(())
/// # }
/// ```
pub(crate) fn replay_wal_into_storage(
    wal: &ConcurrentWalSystem,
    current: &CurrentStorage,
    historical: &mut HistoricalStorage,
    start_lsn: LSN,
    next_version_id: u64,
) -> Result<(LSN, Option<u64>, Option<u64>, u64)> {
    replay_wal_into_storage_with_constraints(
        wal,
        current,
        historical,
        start_lsn,
        next_version_id,
        None,
    )
}

pub(crate) fn replay_wal_into_storage_with_constraints(
    wal: &ConcurrentWalSystem,
    current: &CurrentStorage,
    historical: &mut HistoricalStorage,
    start_lsn: LSN,
    next_version_id: u64,
    constraint_registry: Option<&ConstraintRegistry>,
) -> Result<(LSN, Option<u64>, Option<u64>, u64)> {
    // Read the segment directory once, then apply. Callers that have already
    // read the WAL for startup (Issue #3429) skip this read entirely and call
    // `replay_entries_into_storage_with_constraints` directly with the
    // pre-decoded entries, avoiding a redundant full decode of the WAL.
    let wal_entries = wal.read_from(start_lsn)?;
    replay_entries_into_storage_with_constraints(
        wal,
        wal_entries,
        current,
        historical,
        next_version_id,
        constraint_registry,
    )
}

/// Apply an already-decoded WAL entry stream to storage (Issue #3429).
///
/// This is the body of [`replay_wal_into_storage_with_constraints`] with the
/// segment read hoisted out. The startup path decodes the WAL exactly once and
/// feeds the resulting entries here (filtered to `>= start_lsn`) so recovery no
/// longer re-reads the segment directory for the differential replay. `wal` is
/// retained only to report the post-replay `current_lsn()`; every operation
/// applied comes from `wal_entries`, never from a fresh read.
///
/// The caller MUST pass exactly the entries with `LSN >= start_lsn` (in the
/// same LSN-sorted order [`ConcurrentWalSystem::read_from`] produces): the
/// transaction-framing resolver below is order-sensitive and expects the same
/// contiguous suffix a direct `read_from(start_lsn)` would return.
pub(crate) fn replay_entries_into_storage_with_constraints(
    wal: &ConcurrentWalSystem,
    wal_entries: Vec<WalEntry>,
    current: &CurrentStorage,
    historical: &mut HistoricalStorage,
    mut next_version_id: u64,
    constraint_registry: Option<&ConstraintRegistry>,
) -> Result<(LSN, Option<u64>, Option<u64>, u64)> {
    const RECOVERY_TX_ID: u64 = 0;

    let mut max_node_id: Option<u64> = None;
    let mut max_edge_id: Option<u64> = None;

    if !wal_entries.is_empty() {
        #[cfg(feature = "observability")]
        tracing::info!("Replaying {} WAL entries", wal_entries.len());
        #[cfg(not(feature = "observability"))]
        eprintln!("Replaying {} WAL entries", wal_entries.len());
    }

    // Resolve transaction framing (Issue #3413) BEFORE applying: pair every
    // operation with the timestamp it must be recorded at, and drop any
    // uncommitted transaction tail. Framed data ops committed by a durable
    // `CommitTx` are paired with that marker's authoritative commit timestamp
    // (so an atomic batch's versions all share ONE transaction time); legacy
    // (pre-v7) and non-transactional entries keep their own logged timestamp.
    let resolved = resolve_transaction_frames(wal_entries);

    for (operation, commit_timestamp) in resolved {
        match operation {
            WalOperation::CreateNode {
                node_id,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                max_node_id = Some(match max_node_id {
                    Some(current_max) => current_max.max(node_id.as_u64()),
                    None => node_id.as_u64(),
                });

                // Idempotent re-application guard (Issue #3419). Replay starts
                // AT the manifest LSN (inclusive) because the snapshot may or
                // may not contain effects of entries at/after that LSN. An
                // existing node whose historical head is a LIVE (open valid
                // interval) version means this exact create was already
                // applied; re-applying it blindly would append a duplicate
                // version to the node's bi-temporal history. The two stores
                // are guarded independently because background persistence
                // may snapshot them at different LSNs.
                //
                // Review round 2 (#3428): a head with a CLOSED valid interval
                // (delete tombstone or #3230 retraction) does NOT reflect this
                // create — the id was deleted/retracted and this entry
                // RE-CREATES it (a legal in-WAL sequence: create → delete →
                // create with the same id). Treating that head as "already
                // applied" left current pointing at the tombstone's version
                // and lost the new incarnation's history entirely.
                let current_node = current.get_node(node_id).ok();
                let historical_head = historical.get_current_node_version(node_id);
                let live_head = historical_head.filter(|vid| {
                    historical
                        .get_node_version(*vid)
                        .is_some_and(|v| v.temporal.valid_time().is_current())
                });
                if current_node.is_some() && live_head.is_some() {
                    continue;
                }

                let interned_label = label;

                // Transaction time is `commit_timestamp` (Issue #3413): for a
                // framed transaction this is the CommitTx marker's single
                // authoritative commit time (shared by every op in the batch);
                // for legacy/non-transactional entries it is the entry's own
                // logged timestamp. Either way it is supplied by the caller.
                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                // Reuse whichever store already applied this create so that
                // `current.current_version == historical head id` stays
                // aligned; only allocate a fresh recovery version id when
                // neither store has one to align to.
                let version_id = match (live_head, &current_node) {
                    // Historical already holds this create: align current.
                    (Some(vid), _) => vid,
                    // Current already holds this create: align history to its
                    // version id — unless that id already exists in history
                    // (inconsistent pre-fix state), in which case a fresh id
                    // is the only safe choice.
                    (None, Some(node))
                        if historical.get_node_version(node.current_version).is_none() =>
                    {
                        node.current_version
                    }
                    _ => VersionId::new(next_version_id)?,
                };
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                if current_node.is_none() {
                    let node = Node::with_metadata(
                        node_id,
                        interned_label,
                        properties.clone(),
                        version_id,
                        metadata,
                    );
                    current.insert_node_direct(node, commit_timestamp)?;
                }

                if live_head.is_none() {
                    // Re-creation after a tombstone/retraction: the closed head
                    // is superseded in transaction time by
                    // `add_node_version_with_provenance` itself, which routes
                    // through `close_previous_version_intervals` and closes the
                    // current head's tx-time at the new version's tx start
                    // (== `commit_timestamp`) — so no explicit
                    // `close_node_version_transaction_time` is needed here
                    // (Issue #3407; the helper is a superset: same target
                    // version, plus monotonicity guards).
                    historical.add_node_version_with_provenance(
                        node_id,
                        version_id,
                        valid_from,
                        commit_timestamp,
                        interned_label,
                        properties,
                        false, // not a tombstone
                        provenance.map(std::sync::Arc::new),
                    )?;
                }
            }
            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                max_edge_id = Some(match max_edge_id {
                    Some(current_max) => current_max.max(edge_id.as_u64()),
                    None => edge_id.as_u64(),
                });

                // Idempotent re-application guard (Issue #3419) — see the
                // CreateNode arm above. Review round 2 (#3428): a tombstone/
                // retraction head likewise means this entry RE-CREATES the
                // edge id, not that the create was already applied.
                let current_edge = current.get_edge(edge_id).ok();
                let historical_head = historical.get_current_edge_version(edge_id);
                let live_head = historical_head.filter(|vid| {
                    historical
                        .get_edge_version(*vid)
                        .is_some_and(|v| v.temporal.valid_time().is_current())
                });
                if current_edge.is_some() && live_head.is_some() {
                    continue;
                }

                let interned_label = label;

                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                let version_id = match (live_head, &current_edge) {
                    (Some(vid), _) => vid,
                    (None, Some(edge))
                        if historical.get_edge_version(edge.current_version).is_none() =>
                    {
                        edge.current_version
                    }
                    _ => VersionId::new(next_version_id)?,
                };
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                if current_edge.is_none() {
                    let edge = Edge::with_metadata(
                        edge_id,
                        interned_label,
                        source,
                        target,
                        properties.clone(),
                        version_id,
                        metadata,
                    );
                    current.insert_edge_direct(edge)?;
                }

                if live_head.is_none() {
                    // Re-creation after a tombstone/retraction: the head's
                    // tx-time closure happens inside
                    // `add_edge_version_with_provenance` via
                    // `close_previous_version_intervals` (Issue #3407) — see
                    // the CreateNode arm above.
                    historical.add_edge_version_with_provenance(
                        edge_id,
                        version_id,
                        valid_from,
                        commit_timestamp,
                        interned_label,
                        source,
                        target,
                        properties,
                        false, // not a tombstone
                        provenance.map(std::sync::Arc::new),
                    )?;
                }
            }
            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                let interned_label = label;

                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

                let node = Node::with_metadata(
                    node_id,
                    interned_label,
                    properties.clone(),
                    version_id,
                    metadata,
                );

                // Overwriting current state with the logged version is
                // idempotent; always apply so a lagging graph snapshot
                // converges to the logged state.
                current.update_node_direct(node, commit_timestamp)?;

                // Idempotent re-application guard (Issue #3419): updates log
                // their version_id, so "this exact version already exists in
                // history" means the update was already applied. Re-applying
                // it blindly would append the version AGAIN with ITSELF as
                // the previous head — a self-referential delta chain that
                // makes history reconstruction loop forever (observed
                // empirically: get_node_history OOMs after a double replay).
                if historical.get_node_version(version_id).is_none() {
                    // Appending the successor closes the current head's
                    // transaction time in `close_previous_version_intervals`
                    // (reached via `add_node_version_with_provenance`), so an
                    // explicit `close_node_version_transaction_time` on the
                    // prior head would be redundant (Issue #3407): the helper
                    // closes the SAME head (`node_version_heads[node_id]`) at
                    // the same tx timestamp, and adds monotonicity guards.
                    historical.add_node_version_with_provenance(
                        node_id,
                        version_id,
                        valid_from,
                        commit_timestamp,
                        interned_label,
                        properties,
                        false, // not a tombstone
                        provenance.map(std::sync::Arc::new),
                    )?;
                }
            }
            WalOperation::UpdateEdge {
                edge_id,
                version_id,
                label,
                properties,
                valid_from,
                provenance,
            } => {
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                let current_edge = current.get_edge(edge_id)?;

                let interned_label = label;

                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

                let edge = Edge::with_metadata(
                    edge_id,
                    interned_label,
                    current_edge.source,
                    current_edge.target,
                    properties.clone(),
                    version_id,
                    metadata,
                );

                current.update_edge_direct(edge)?;

                // Idempotent re-application guard (Issue #3419) — see the
                // UpdateNode arm above for why an already-present version_id
                // must not be appended to history a second time.
                if historical.get_edge_version(version_id).is_none() {
                    // The prior head's tx-time closure is handled inside
                    // `add_edge_version_with_provenance` via
                    // `close_previous_version_intervals` (Issue #3407) — see
                    // the UpdateNode arm above.
                    historical.add_edge_version_with_provenance(
                        edge_id,
                        version_id,
                        valid_from,
                        commit_timestamp,
                        interned_label,
                        current_edge.source,
                        current_edge.target,
                        properties,
                        false, // not a tombstone
                        provenance.map(std::sync::Arc::new),
                    )?;
                }
            }
            WalOperation::DeleteNode {
                node_id,
                valid_from,
                version_id: logged_tombstone_id,
            } => {
                // Idempotent re-application guard, per store (review round 2,
                // #3428). Background persistence may snapshot current and
                // historical at different LSNs, so a boundary delete can be
                // reflected in one store but not the other:
                //   (a) node still in current + tombstone already in
                //       historical → only the current-side removal is owed;
                //       re-applying the historical side would append a SECOND
                //       tombstone.
                //   (b) node already gone from current + head still live in
                //       historical → only the historical closure/tombstone is
                //       owed; gating it on current presence (the pre-fix
                //       behavior) left the head open forever.
                // The historical side applies iff the head is still live; the
                // current side applies iff the node is present.
                let current_node = current.get_node(node_id).ok();
                let historical_head = historical.get_current_node_version(node_id);
                let live_head = historical_head.filter(|vid| {
                    historical
                        .get_node_version(*vid)
                        .is_some_and(|v| v.temporal.valid_time().is_current())
                });

                // Tombstone payload: prefer the current node, else reconstruct
                // from the live historical head (case (b) above).
                let tombstone_payload = if let Some(node) = &current_node {
                    if live_head.is_some() || historical_head.is_none() {
                        Some((node.label, node.properties.clone()))
                    } else {
                        None // head already tombstoned/retracted: case (a)
                    }
                } else if let Some(vid) = live_head {
                    let label = historical.get_node_version(vid).map(|v| v.label);
                    match label {
                        Some(label) => Some((label, historical.reconstruct_node_properties(vid)?)),
                        None => None,
                    }
                } else {
                    None
                };

                if let Some((label, properties)) = tombstone_payload {
                    // The current head's transaction time is closed when the
                    // tombstone is appended below: `add_node_version` routes
                    // through `close_previous_version_intervals`, which closes
                    // the SAME head (`node_version_heads[node_id]`) at the
                    // tombstone's tx start (== `commit_timestamp`). An explicit
                    // `close_node_version_transaction_time` here would be
                    // redundant (Issue #3407; the helper is a superset — same
                    // target version, plus monotonicity guards).

                    // Honor the LOGGED tombstone version_id (Issue #3406) so the
                    // recovered version chain is bit-identical to the live one
                    // and replay is idempotent; segments predating
                    // WAL_VERSION_DELETE_VERSION_ID carry no id, so synthesize as
                    // before. Either way, advance next_version_id past it to keep
                    // future synthesized ids collision-free.
                    let tombstone_version_id = match logged_tombstone_id {
                        Some(vid) => vid,
                        None => VersionId::new(next_version_id)?,
                    };
                    next_version_id = next_version_id.max(tombstone_version_id.as_u64() + 1);

                    // Honor the LOGGED valid_from (possibly backdated, Issues
                    // #3221/#3400) — mirroring the live path's
                    // `apply_node_delete` — while tx_time comes from the WAL
                    // entry. The is_tombstone=true flag closes the valid_time
                    // immediately at valid_from (empty interval).
                    historical.add_node_version(
                        node_id,
                        tombstone_version_id,
                        valid_from,
                        commit_timestamp,
                        label,
                        properties,
                        true, // is_tombstone
                    )?;
                }

                if current_node.is_some() {
                    current.delete_node_direct(node_id, commit_timestamp)?;
                }
            }
            WalOperation::DeleteEdge {
                edge_id,
                valid_from,
                version_id: logged_tombstone_id,
            } => {
                // Per-store re-application guard — see the DeleteNode arm
                // above (review round 2, #3428).
                let current_edge = current.get_edge(edge_id).ok();
                let historical_head = historical.get_current_edge_version(edge_id);
                let live_head = historical_head.filter(|vid| {
                    historical
                        .get_edge_version(*vid)
                        .is_some_and(|v| v.temporal.valid_time().is_current())
                });

                let tombstone_payload = if let Some(edge) = &current_edge {
                    if live_head.is_some() || historical_head.is_none() {
                        Some((
                            edge.label,
                            edge.source,
                            edge.target,
                            edge.properties.clone(),
                        ))
                    } else {
                        None // head already tombstoned/retracted
                    }
                } else if let Some(vid) = live_head {
                    let head = historical
                        .get_edge_version(vid)
                        .map(|v| (v.label, v.source, v.target));
                    match head {
                        Some((label, source, target)) => Some((
                            label,
                            source,
                            target,
                            historical.reconstruct_edge_properties(vid)?,
                        )),
                        None => None,
                    }
                } else {
                    None
                };

                if let Some((label, source, target, properties)) = tombstone_payload {
                    // The current head's tx-time closure happens inside
                    // `add_edge_version` via `close_previous_version_intervals`
                    // (Issue #3407) — see the DeleteNode arm above.

                    // Honor the LOGGED tombstone version_id (Issue #3406); see
                    // the DeleteNode arm above for rationale and the synthesis
                    // fallback for pre-v9 segments.
                    let tombstone_version_id = match logged_tombstone_id {
                        Some(vid) => vid,
                        None => VersionId::new(next_version_id)?,
                    };
                    next_version_id = next_version_id.max(tombstone_version_id.as_u64() + 1);

                    // Honor the LOGGED valid_from (possibly backdated, Issues
                    // #3221/#3400) — mirroring the live path's
                    // `apply_edge_delete` — while tx_time comes from the WAL
                    // entry. The is_tombstone=true flag closes the valid_time
                    // immediately at valid_from (empty interval).
                    historical.add_edge_version(
                        edge_id,
                        tombstone_version_id,
                        valid_from,
                        commit_timestamp,
                        label,
                        source,
                        target,
                        properties,
                        true, // is_tombstone
                    )?;
                }

                if current_edge.is_some() {
                    current.delete_edge_direct(edge_id)?;
                }
            }
            WalOperation::RetractNode {
                node_id,
                valid_to,
                version_id: logged_retraction_id,
            } => {
                // Valid-time retraction (Issue #3230). Reconstruct exactly what
                // the original commit did:
                //   (a) close the head version's TRANSACTION time (its valid
                //       interval stays untouched — append-only, never rewrite);
                //   (b) append a new version with the same properties whose
                //       valid interval is closed at the logged `valid_to`
                //       (honored faithfully — NOT the replay/commit time);
                //   (c) remove the node from current storage.
                //
                // Guarded per store (review round 2, #3428), like DeleteNode:
                // the historical side applies iff the head is still live (a
                // head already retracted/tombstoned means this retraction's
                // historical effect was applied); the current-side removal
                // applies iff the node is present.
                let current_node = current.get_node(node_id).ok();
                let head_version_id = historical.get_current_node_version(node_id);
                let live_head = head_version_id.filter(|vid| {
                    historical
                        .get_node_version(*vid)
                        .is_some_and(|v| v.temporal.valid_time().is_current())
                });

                // Retraction payload: prefer the current node, else
                // reconstruct from the live historical head.
                let payload = if let Some(node) = &current_node {
                    if live_head.is_some() || head_version_id.is_none() {
                        Some((node.label, node.properties.clone()))
                    } else {
                        None // head already retracted/tombstoned
                    }
                } else if let Some(vid) = live_head {
                    let label = historical.get_node_version(vid).map(|v| v.label);
                    match label {
                        Some(label) => Some((label, historical.reconstruct_node_properties(vid)?)),
                        None => None,
                    }
                } else {
                    None
                };

                if let Some((label, properties)) = payload {
                    // Capture the head's valid_from BEFORE appending, so the
                    // retraction version reproduces the same interval start.
                    let valid_from = head_version_id
                        .and_then(|vid| historical.get_node_version(vid))
                        .map(|v| v.temporal.valid_time().start())
                        .unwrap_or(commit_timestamp);

                    // Step (a) — closing the head version's TRANSACTION time —
                    // is performed by `add_retracted_node_version` below, which
                    // routes through `close_previous_version_intervals`: it
                    // closes the SAME head (`node_version_heads[node_id]`) at
                    // the retraction's tx start (== `commit_timestamp`) while
                    // leaving the head's valid interval untouched (the helper's
                    // valid-time guard is `new start > prev start`, and the
                    // retraction reuses the head's `valid_from`, so it is not
                    // rewritten — append-only preserved). An explicit
                    // `close_node_version_transaction_time` here would be
                    // redundant (Issue #3407).

                    // Honor the LOGGED retraction version_id (Issue #3406); see
                    // the DeleteNode arm for rationale and the pre-v9 synthesis
                    // fallback.
                    let retraction_version_id = match logged_retraction_id {
                        Some(vid) => vid,
                        None => VersionId::new(next_version_id)?,
                    };
                    next_version_id = next_version_id.max(retraction_version_id.as_u64() + 1);

                    historical.add_retracted_node_version(
                        node_id,
                        retraction_version_id,
                        valid_from,
                        valid_to,
                        commit_timestamp,
                        label,
                        properties,
                    )?;
                }

                if current_node.is_some() {
                    current.delete_node_direct(node_id, commit_timestamp)?;
                }
            }
            WalOperation::RetractEdge {
                edge_id,
                valid_to,
                version_id: logged_retraction_id,
            } => {
                // Valid-time retraction of an edge (Issue #3230); mirrors the
                // RetractNode arm above, honoring the logged `valid_to`, with
                // the same per-store re-application guards (#3428).
                let current_edge = current.get_edge(edge_id).ok();
                let head_version_id = historical.get_current_edge_version(edge_id);
                let live_head = head_version_id.filter(|vid| {
                    historical
                        .get_edge_version(*vid)
                        .is_some_and(|v| v.temporal.valid_time().is_current())
                });

                let payload = if let Some(edge) = &current_edge {
                    if live_head.is_some() || head_version_id.is_none() {
                        Some((
                            edge.label,
                            edge.source,
                            edge.target,
                            edge.properties.clone(),
                        ))
                    } else {
                        None // head already retracted/tombstoned
                    }
                } else if let Some(vid) = live_head {
                    let head = historical
                        .get_edge_version(vid)
                        .map(|v| (v.label, v.source, v.target));
                    match head {
                        Some((label, source, target)) => Some((
                            label,
                            source,
                            target,
                            historical.reconstruct_edge_properties(vid)?,
                        )),
                        None => None,
                    }
                } else {
                    None
                };

                if let Some((label, source, target, properties)) = payload {
                    let valid_from = head_version_id
                        .and_then(|vid| historical.get_edge_version(vid))
                        .map(|v| v.temporal.valid_time().start())
                        .unwrap_or(commit_timestamp);

                    // The head version's tx-time closure (step (a)) is handled
                    // inside `add_retracted_edge_version` via
                    // `close_previous_version_intervals` (Issue #3407) — see
                    // the RetractNode arm above.

                    // Honor the LOGGED retraction version_id (Issue #3406); see
                    // the DeleteNode arm for rationale and the pre-v9 synthesis
                    // fallback.
                    let retraction_version_id = match logged_retraction_id {
                        Some(vid) => vid,
                        None => VersionId::new(next_version_id)?,
                    };
                    next_version_id = next_version_id.max(retraction_version_id.as_u64() + 1);

                    historical.add_retracted_edge_version(
                        edge_id,
                        retraction_version_id,
                        valid_from,
                        valid_to,
                        commit_timestamp,
                        label,
                        source,
                        target,
                        properties,
                    )?;
                }

                if current_edge.is_some() {
                    current.delete_edge_direct(edge_id)?;
                }
            }
            WalOperation::Checkpoint { .. } => {
                // Checkpoint markers are informational only during replay
            }
            WalOperation::DeclareUniqueConstraint { label, property } => {
                if let Some(reg) = constraint_registry {
                    reg.declare(label, property);
                }
            }
            WalOperation::DropUniqueConstraint { label, property } => {
                if let Some(reg) = constraint_registry {
                    reg.undeclare(label, property);
                }
            }
            // Transaction framing markers (Issue #3413) are consumed by
            // `resolve_transaction_frames` above and never reach this apply
            // loop; the arm exists only for match exhaustiveness.
            WalOperation::BeginTx { .. } | WalOperation::CommitTx { .. } => {}
        }
    }

    let final_lsn = wal.current_lsn();

    Ok((final_lsn, max_node_id, max_edge_id, next_version_id))
}

/// Warn about a discarded (torn / uncommitted) transaction frame during replay.
fn warn_discarded_frame(reason: &str, dropped: usize) {
    #[cfg(feature = "observability")]
    tracing::warn!(
        "Discarding {} buffered WAL op(s) during recovery: {}",
        dropped,
        reason
    );
    #[cfg(not(feature = "observability"))]
    eprintln!(
        "WARNING: discarding {} buffered WAL op(s) during recovery: {}",
        dropped, reason
    );
}

/// Are the buffered ops' LSNs strictly consecutive (`l, l+1, l+2, …`)?
///
/// A committing transaction owns one contiguous LSN band (single atomic
/// `allocate_batch`), so a gap means the buffer does not correspond to an
/// intact batch and the frame must be rejected (Issue #3413, integrity check
/// R4).
fn lsns_contiguous(pending: &[(LSN, WalOperation)]) -> bool {
    pending
        .windows(2)
        .all(|w| w[1].0.0 == w[0].0.0.wrapping_add(1))
}

/// Resolve transaction framing (Issue #3413), turning the raw, LSN-ordered WAL
/// entry stream into the ordered list of `(operation, transaction_timestamp)`
/// pairs that replay must apply — with uncommitted transaction tails removed.
///
/// # Load-bearing invariant: replay begins at a transaction-frame boundary
///
/// This resolver assumes the entry stream it is handed **never begins in the
/// middle of a `[BeginTx .. CommitTx]` band** — i.e. `start_lsn` is always a
/// band start (or a legacy/unframed boundary), never a data op or `CommitTx`
/// whose `BeginTx` was already skipped. Production guarantees this: startup
/// replays from an inclusive `start_lsn = manifest.lsn` (see `db::config`), and
/// the manifest LSN is the observable next-to-allocate LSN, which
/// `allocate_batch` only ever advances by a WHOLE `[BeginTx .. CommitTx]` band
/// in one atomic step — so it can never fall mid-band. If that invariant were
/// ever violated (see the note on `checkpoint.rs`'s exclusive `.next()`
/// convention, currently test-only and unwired), a `CommitTx` could arrive with
/// no open frame; this resolver treats that as a **benign skip** (its `BeginTx`
/// lies below `start_lsn`, so there are no buffered ops to apply) rather than a
/// "failed validation" corruption warning.
///
/// # Framed-replay invariant
///
/// A committing `WriteTransaction` writes `[BeginTx, ..data ops.., CommitTx]`
/// as one atomic, contiguous LSN band inside a single flush epoch (see
/// `api::transaction::write::wal::log_operations_to_wal`). Because the durable
/// suffix of the WAL is always an LSN-ordered *prefix* of what was appended,
/// the terminal `CommitTx` marker is present on disk **iff** every op of its
/// batch is — so:
///
/// * A `BeginTx … data … CommitTx` frame whose marker validated (matching
///   `tx_id`, `entry_count` equal to the buffered op count, contiguous LSNs)
///   is applied **atomically**, and every one of its ops is stamped with the
///   marker's single `commit_timestamp`. This is what makes an atomic batch's
///   versions share one transaction time so `AS OF SYSTEM_TIME` can never
///   observe a half-batch.
/// * A frame whose marker never reached disk (a crash tail), or that fails the
///   integrity checks, is **discarded whole** — never applied as a torn prefix.
/// * Entries from pre-v7 segments (`!framed`), and any op appended outside a
///   frame (raw `append`/`append_async`, control ops such as checkpoints and
///   constraint declarations), keep the legacy **immediate-apply** path using
///   their own logged timestamp. This preserves the behavior of every existing
///   non-transactional WAL producer unchanged.
fn resolve_transaction_frames(entries: Vec<WalEntry>) -> Vec<(WalOperation, Timestamp)> {
    let mut resolved: Vec<(WalOperation, Timestamp)> = Vec::with_capacity(entries.len());
    // Buffered data ops of the currently-open frame, with their LSNs.
    let mut pending: Vec<(LSN, WalOperation)> = Vec::new();
    // `(tx_id, begin_lsn)` of the currently-open frame, if any. The `BeginTx`
    // LSN is retained so the terminal `CommitTx` can bracket-check the band
    // (Issue #3413 integrity check R5): the whole `[BeginTx .. CommitTx]` band
    // must be gap-free at BOTH ends, not just internally contiguous.
    let mut open_tx: Option<(u64, LSN)> = None;

    for entry in entries {
        // Legacy / non-transactional entries apply immediately with their own
        // logged timestamp (pre-v7 segments never carry framing markers).
        if !entry.framed {
            resolved.push((entry.operation, entry.timestamp));
            continue;
        }

        // Copy the header fields we need before moving `operation` out.
        let entry_lsn = entry.lsn;
        let entry_ts = entry.timestamp;

        match entry.operation {
            WalOperation::BeginTx { tx_id } => {
                if !pending.is_empty() {
                    warn_discarded_frame(
                        "new BeginTx encountered before the previous frame committed",
                        pending.len(),
                    );
                    pending.clear();
                }
                open_tx = Some((tx_id, entry_lsn));
            }
            WalOperation::CommitTx {
                tx_id,
                entry_count,
                commit_timestamp,
            } => {
                match open_tx {
                    // Benign skip (see the load-bearing invariant on this fn): a
                    // `CommitTx` with no open frame is a band whose `BeginTx`
                    // lies below `start_lsn`. Under the production invariant this
                    // never happens; if it ever did, `pending` is empty and there
                    // is nothing to apply — quietly skip rather than emit a
                    // spurious corruption warning.
                    None => {
                        debug_assert!(
                            pending.is_empty(),
                            "a CommitTx with no open frame must have no buffered ops"
                        );
                    }
                    Some((open_id, begin_lsn)) => {
                        // Bracket check (R5): the band must be gap-free at both
                        // ends — the first data op immediately follows BeginTx
                        // and CommitTx immediately follows the last data op — in
                        // addition to being internally contiguous (R4) and
                        // matching the marker's tx_id / entry_count.
                        let brackets_ok = pending.first().map(|(l, _)| l.0)
                            == Some(begin_lsn.0.wrapping_add(1))
                            && pending.last().map(|(l, _)| l.0.wrapping_add(1))
                                == Some(entry_lsn.0);
                        let valid = open_id == tx_id
                            && pending.len() == entry_count as usize
                            && lsns_contiguous(&pending)
                            && brackets_ok;
                        if valid {
                            // Commit the frame atomically: every op takes the
                            // marker's authoritative commit timestamp.
                            for (_lsn, op) in pending.drain(..) {
                                resolved.push((op, commit_timestamp));
                            }
                        } else {
                            warn_discarded_frame(
                                "CommitTx failed validation (tx_id / entry_count / LSN contiguity / band brackets)",
                                pending.len(),
                            );
                            pending.clear();
                        }
                    }
                }
                open_tx = None;
            }
            // Self-committing control ops are never written inside a
            // transaction batch; if a frame is somehow open it is torn, so
            // discard it, then apply the control op immediately.
            op @ (WalOperation::Checkpoint { .. }
            | WalOperation::DeclareUniqueConstraint { .. }
            | WalOperation::DropUniqueConstraint { .. }) => {
                if !pending.is_empty() {
                    warn_discarded_frame(
                        "control op encountered inside an open transaction frame",
                        pending.len(),
                    );
                    pending.clear();
                }
                open_tx = None;
                resolved.push((op, entry_ts));
            }
            // A framed data op.
            op => {
                if open_tx.is_some() {
                    pending.push((entry_lsn, op));
                } else {
                    // ⚠️ LANDMINE (Issue #3413): a framed data op with NO open
                    // frame falls through to immediate-apply with its OWN
                    // per-entry timestamp — exactly the per-op timestamp
                    // bisection transaction framing exists to prevent. This is
                    // deliberately reachable ONLY for raw `append`/`append_async`
                    // into a v7+ segment (test/tooling paths), which carry the
                    // `framed` flag from the segment version yet legitimately
                    // have no `BeginTx`; for them immediate-apply is correct.
                    // The real committing producer (`log_operations_to_wal`)
                    // ALWAYS brackets its data ops in a `[BeginTx .. CommitTx]`
                    // band, so a *transactional* op never lands here. If any
                    // future producer raw-appends framed data ops that were
                    // meant to share a commit timestamp, this branch would
                    // silently re-bisect them — audit this path before adding
                    // one.
                    resolved.push((op, entry_ts));
                }
            }
        }
    }

    // Any buffer still open at end-of-stream is an uncommitted crash tail.
    if !pending.is_empty() {
        warn_discarded_frame(
            "WAL ended with an uncommitted transaction frame (crash tail)",
            pending.len(),
        );
    }

    resolved
}

/// Replay ONLY constraint declaration/drop entries from the full WAL history.
///
/// Because constraint declarations are written to the WAL before the
/// corresponding node data, they may lie at LSNs below the persisted snapshot
/// LSN and therefore be skipped by the normal differential replay. This reads
/// every WAL entry from LSN 0 and replays only the two constraint-related
/// operations, giving the net constraint state.
///
/// As of Issue #3429 the startup path no longer calls this: it decodes the WAL
/// once and folds the constraint net-state out of the already-read entries via
/// [`apply_constraint_declarations`], avoiding a dedicated full read. This
/// wrapper (a thin `read_from` + `apply_constraint_declarations`) is retained
/// for the recovery unit tests that exercise the read-and-apply path directly.
#[cfg(test)]
pub(crate) fn replay_constraint_declarations_from_wal(
    wal: &ConcurrentWalSystem,
    registry: &ConstraintRegistry,
) -> Result<()> {
    let all_entries = wal.read_from(LSN::initial())?;
    apply_constraint_declarations(&all_entries, registry);
    Ok(())
}

/// Fold the constraint declare/drop net-state out of an already-decoded WAL
/// entry stream (Issue #3429).
///
/// Same net effect as [`replay_constraint_declarations_from_wal`] but operates
/// on entries the startup path has *already* read from disk, so recovery does
/// not decode the segment directory a second time just to recover constraint
/// state. `entries` is expected to span the full WAL history (from
/// [`LSN::initial()`]) so declarations that predate the index snapshot LSN --
/// and would be skipped by the differential replay -- are still applied.
///
/// Declares and drops are replayed in stream order; the net set is
/// order-dependent (declare then drop == dropped), matching the prior full-read
/// pass exactly. Borrows the entries (does not consume them) so the same vector
/// can then be filtered and handed to the differential replay.
pub(crate) fn apply_constraint_declarations(entries: &[WalEntry], registry: &ConstraintRegistry) {
    for entry in entries {
        match entry.operation {
            WalOperation::DeclareUniqueConstraint { label, property } => {
                registry.declare(label, property);
            }
            WalOperation::DropUniqueConstraint { label, property } => {
                registry.undeclare(label, property);
            }
            _ => {}
        }
    }
}

/// Unit tests for the `resolve_transaction_frames` framing resolver
/// (Issue #3413). These exercise the buffer-until-marker logic directly on
/// synthetic `WalEntry` streams, independent of disk/replay.
#[cfg(test)]
mod framing_tests {
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
            label: GLOBAL_INTERNER.intern("FramingTest").unwrap(),
            properties: PropertyMap::new(),
            valid_from: ts(1),
            provenance: None,
        }
    }

    /// Build an entry as read from a framed (v7+) segment.
    fn framed(lsn: u64, op: WalOperation, timestamp: Timestamp) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp,
            operation: op,
            checksum: 0,
            framed: true,
        }
    }

    /// Build an entry as read from a legacy (pre-v7) segment.
    fn legacy(lsn: u64, op: WalOperation, timestamp: Timestamp) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp,
            operation: op,
            checksum: 0,
            framed: false,
        }
    }

    fn node_id_of(op: &WalOperation) -> u64 {
        match op {
            WalOperation::CreateNode { node_id, .. } => node_id.as_u64(),
            other => panic!("expected CreateNode, got {other:?}"),
        }
    }

    #[test]
    fn commits_framed_batch_atomically_with_marker_timestamp() {
        let marker_ts = ts(9999);
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 7 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(3, create_node_op(2), ts(12)),
            framed(
                4,
                WalOperation::CommitTx {
                    tx_id: 7,
                    entry_count: 2,
                    commit_timestamp: marker_ts,
                },
                ts(13),
            ),
        ];
        let resolved = resolve_transaction_frames(entries);
        assert_eq!(resolved.len(), 2);
        // Both ops re-stamped with the SINGLE marker timestamp (AC#3).
        for (_op, t) in &resolved {
            assert_eq!(*t, marker_ts);
        }
        assert_eq!(node_id_of(&resolved[0].0), 1);
        assert_eq!(node_id_of(&resolved[1].0), 2);
    }

    #[test]
    fn discards_uncommitted_tail_at_eof() {
        // BeginTx + data ops, marker never arrived (crash tail).
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(3, create_node_op(2), ts(12)),
        ];
        let resolved = resolve_transaction_frames(entries);
        assert!(resolved.is_empty(), "uncommitted frame must be discarded");
    }

    #[test]
    fn keeps_committed_frame_then_discards_following_uncommitted_frame() {
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
            // tx 2's CommitTx lost.
        ];
        let resolved = resolve_transaction_frames(entries);
        assert_eq!(resolved.len(), 1, "only the committed frame survives");
        assert_eq!(node_id_of(&resolved[0].0), 1);
        assert_eq!(resolved[0].1, ts(100));
    }

    #[test]
    fn discards_frame_on_entry_count_mismatch() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 5, // wrong: only 1 data op buffered
                    commit_timestamp: ts(100),
                },
                ts(12),
            ),
        ];
        assert!(resolve_transaction_frames(entries).is_empty());
    }

    #[test]
    fn discards_frame_on_non_contiguous_lsns() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(9, create_node_op(2), ts(12)), // LSN gap
            framed(
                10,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 2,
                    commit_timestamp: ts(100),
                },
                ts(13),
            ),
        ];
        assert!(resolve_transaction_frames(entries).is_empty());
    }

    #[test]
    fn discards_frame_on_tx_id_mismatch() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::CommitTx {
                    tx_id: 2, // does not match the open BeginTx
                    entry_count: 1,
                    commit_timestamp: ts(100),
                },
                ts(12),
            ),
        ];
        assert!(resolve_transaction_frames(entries).is_empty());
    }

    #[test]
    fn legacy_unframed_entries_apply_immediately_with_own_timestamp() {
        // Pre-v7 segment: no markers, each op keeps its own logged timestamp.
        let entries = vec![
            legacy(1, create_node_op(1), ts(50)),
            legacy(2, create_node_op(2), ts(60)),
        ];
        let resolved = resolve_transaction_frames(entries);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].1, ts(50));
        assert_eq!(resolved[1].1, ts(60));
    }

    #[test]
    fn control_op_is_self_committing_and_not_buffered() {
        // A framed Checkpoint applies immediately even though it lives in a
        // v7 segment; it must not be swallowed into a pending buffer.
        let entries = vec![framed(
            1,
            WalOperation::Checkpoint {
                lsn: LSN(1),
                timestamp: ts(5),
            },
            ts(70),
        )];
        let resolved = resolve_transaction_frames(entries);
        assert_eq!(resolved.len(), 1);
        assert!(matches!(resolved[0].0, WalOperation::Checkpoint { .. }));
        assert_eq!(resolved[0].1, ts(70));
    }

    #[test]
    fn control_op_discards_an_open_frame_then_applies() {
        // Defensive: a control op appearing while a frame is open discards the
        // (torn) buffer and still applies the control op.
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(
                3,
                WalOperation::DeclareUniqueConstraint {
                    label: GLOBAL_INTERNER.intern("L").unwrap(),
                    property: GLOBAL_INTERNER.intern("p").unwrap(),
                },
                ts(80),
            ),
        ];
        let resolved = resolve_transaction_frames(entries);
        assert_eq!(resolved.len(), 1);
        assert!(matches!(
            resolved[0].0,
            WalOperation::DeclareUniqueConstraint { .. }
        ));
    }

    /// Fix #3413 (defensive benign skip): a `CommitTx` that arrives with no
    /// open frame (its `BeginTx` lay below `start_lsn`) applies nothing and is
    /// NOT a validation failure. Unreachable in production (replay always
    /// starts at a band boundary) but must degrade gracefully if it ever isn't.
    #[test]
    fn stray_commit_tx_with_no_open_frame_is_benign_skip() {
        let entries = vec![framed(
            5,
            WalOperation::CommitTx {
                tx_id: 9,
                entry_count: 0,
                commit_timestamp: ts(100),
            },
            ts(13),
        )];
        let resolved = resolve_transaction_frames(entries);
        assert!(
            resolved.is_empty(),
            "a stray CommitTx with no open frame must apply nothing"
        );
    }

    /// Fix #3413 (band bracket check, R5): a frame that is internally contiguous
    /// AND matches tx_id/entry_count but has a GAP between `BeginTx` and the
    /// first data op (head gap) must be discarded — the band is not fully
    /// bracketed, so an op may be missing at the head.
    #[test]
    fn discards_frame_on_head_gap_between_begin_and_first_op() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            // Head gap: first data op at LSN 3, not 2 (BeginTx.lsn + 1).
            framed(3, create_node_op(1), ts(11)),
            framed(4, create_node_op(2), ts(12)),
            framed(
                5,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 2,
                    commit_timestamp: ts(100),
                },
                ts(13),
            ),
        ];
        assert!(
            resolve_transaction_frames(entries).is_empty(),
            "a head-gapped band (BeginTx not immediately followed by first data op) must discard"
        );
    }

    /// Fix #3413 (band bracket check, R5): a GAP between the last data op and
    /// the terminal `CommitTx` (tail gap) must also discard the frame, even
    /// though the data ops are internally contiguous.
    #[test]
    fn discards_frame_on_tail_gap_between_last_op_and_commit() {
        let entries = vec![
            framed(1, WalOperation::BeginTx { tx_id: 1 }, ts(10)),
            framed(2, create_node_op(1), ts(11)),
            framed(3, create_node_op(2), ts(12)),
            // Tail gap: CommitTx at LSN 5, not 4 (last data op.lsn + 1).
            framed(
                5,
                WalOperation::CommitTx {
                    tx_id: 1,
                    entry_count: 2,
                    commit_timestamp: ts(100),
                },
                ts(13),
            ),
        ];
        assert!(
            resolve_transaction_frames(entries).is_empty(),
            "a tail-gapped band (last data op not immediately followed by CommitTx) must discard"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::tempdir;

    #[test]
    fn replay_constraint_declarations_declare_and_drop() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let label = GLOBAL_INTERNER.intern("RcvTestLabel").unwrap();
        let prop = GLOBAL_INTERNER.intern("rcvTestProp").unwrap();

        // Declare then drop: net result = not active.
        wal.append(WalOperation::DeclareUniqueConstraint {
            label,
            property: prop,
        })
        .unwrap();
        wal.append(WalOperation::DropUniqueConstraint {
            label,
            property: prop,
        })
        .unwrap();
        wal.flush().unwrap();

        let registry = ConstraintRegistry::new();
        replay_constraint_declarations_from_wal(&wal, &registry).unwrap();

        assert!(
            !registry.is_constrained(label, prop),
            "net declare+drop must leave constraint inactive"
        );
    }

    /// Issue #3230: `RetractNode`/`RetractEdge` replay must reconstruct the
    /// retraction exactly — close the head's transaction time, append a
    /// version whose valid interval honors the logged `valid_to` (NOT the
    /// replay/commit time, unlike the historical DeleteNode behavior), and
    /// remove the entity from current storage.
    #[test]
    fn replay_retract_node_honors_valid_to() {
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let label = GLOBAL_INTERNER.intern("RcvRetractNode").unwrap();
        let now = time::now().wallclock();
        let valid_from = crate::core::hlc::HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
        let valid_to = crate::core::hlc::HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

        wal.append(WalOperation::CreateNode {
            node_id,
            label,
            properties: PropertyMap::new(),
            valid_from,
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::RetractNode {
            node_id,
            valid_to,
            version_id: None,
        })
        .unwrap();
        wal.flush().unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 1).unwrap();

        // Removed from current state.
        assert!(current.get_node(node_id).is_err());

        // The head version carries the closed valid interval
        // [valid_from, valid_to) — honoring the logged valid_to.
        let head_id = historical.get_current_node_version(node_id).unwrap();
        let head = historical.get_node_version(head_id).unwrap();
        assert_eq!(head.temporal.valid_time().start(), valid_from);
        assert_eq!(
            head.temporal.valid_time().end(),
            valid_to,
            "replay must honor the logged valid_to, not the replay time"
        );

        // Bi-temporal reads: visible strictly before valid_to, not at/after.
        let probe_before = crate::core::hlc::HybridTimestamp::new(now - 2_700_000_000, 0).unwrap();
        assert!(
            historical
                .get_node_at_time(node_id, probe_before, time::now())
                .is_ok()
        );
        assert!(
            historical
                .get_node_at_time(node_id, valid_to, time::now())
                .is_err()
        );
        assert!(
            historical
                .get_node_at_time(node_id, time::now(), time::now())
                .is_err()
        );

        // Append-only AS OF SYSTEM_TIME contract, post-replay (fix-round
        // #8): the PRE-retraction head must still carry an OPEN valid
        // interval with its transaction time closed at the LOGGED
        // RetractNode entry timestamp — replay must not substitute the
        // replay time, and must never rewrite the pre-retraction record.
        let entries = wal.read_from(LSN::initial()).unwrap();
        let logged_retract_ts = entries
            .iter()
            .find(|e| matches!(e.operation, WalOperation::RetractNode { .. }))
            .expect("RetractNode entry must be in the WAL")
            .timestamp;
        let logged_create_ts = entries
            .iter()
            .find(|e| matches!(e.operation, WalOperation::CreateNode { .. }))
            .expect("CreateNode entry must be in the WAL")
            .timestamp;

        let history = historical.get_node_history(node_id).unwrap();
        assert_eq!(history.version_count(), 2, "create + retraction versions");
        let pre_retraction_head = &history.versions[0];
        assert!(
            pre_retraction_head.temporal.valid_time().is_current(),
            "pre-retraction head's valid interval must stay open-ended after replay"
        );
        assert!(
            !pre_retraction_head.temporal.transaction_time().is_current(),
            "pre-retraction head's transaction time must be closed after replay"
        );
        assert_eq!(
            pre_retraction_head.temporal.transaction_time().end(),
            logged_retract_ts,
            "transaction time must close at the LOGGED entry timestamp, not the replay time"
        );

        // Anchoring AS OF SYSTEM_TIME before the retraction's logged commit
        // shows the fact open-ended (valid even at valid times >= valid_to);
        // anchoring at the retraction's commit does not.
        assert!(
            historical
                .get_node_at_time(node_id, time::now(), logged_create_ts)
                .is_ok(),
            "AS OF SYSTEM_TIME before the retraction must show the fact open-ended"
        );
        assert!(
            historical
                .get_node_at_time(node_id, time::now(), logged_retract_ts)
                .is_err()
        );
    }

    /// Issue #3230: RetractEdge replay — same contract as RetractNode.
    #[test]
    fn replay_retract_edge_honors_valid_to() {
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let source = crate::core::NodeId::new(1).unwrap();
        let target = crate::core::NodeId::new(2).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvRetractEdgeNode").unwrap();
        let edge_label = GLOBAL_INTERNER.intern("RCV_RETRACT_EDGE").unwrap();
        let now = time::now().wallclock();
        let valid_from = crate::core::hlc::HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
        let valid_to = crate::core::hlc::HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

        for node_id in [source, target] {
            wal.append(WalOperation::CreateNode {
                node_id,
                label: node_label,
                properties: PropertyMap::new(),
                valid_from,
                provenance: None,
            })
            .unwrap();
        }
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label: edge_label,
            properties: PropertyMap::new(),
            valid_from,
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::RetractEdge {
            edge_id,
            valid_to,
            version_id: None,
        })
        .unwrap();
        wal.flush().unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 1).unwrap();

        assert!(current.get_edge(edge_id).is_err());
        let head_id = historical.get_current_edge_version(edge_id).unwrap();
        let head = historical.get_edge_version(head_id).unwrap();
        assert_eq!(head.temporal.valid_time().start(), valid_from);
        assert_eq!(head.temporal.valid_time().end(), valid_to);

        let probe_before = crate::core::hlc::HybridTimestamp::new(now - 2_700_000_000, 0).unwrap();
        assert!(
            historical
                .get_edge_at_time(edge_id, probe_before, time::now())
                .is_ok()
        );
        assert!(
            historical
                .get_edge_at_time(edge_id, valid_to, time::now())
                .is_err()
        );
    }

    #[test]
    fn replay_constraint_declarations_declare_survives() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let label = GLOBAL_INTERNER.intern("RcvSurviveLabel").unwrap();
        let prop = GLOBAL_INTERNER.intern("rcvSurviveProp").unwrap();

        wal.append(WalOperation::DeclareUniqueConstraint {
            label,
            property: prop,
        })
        .unwrap();
        wal.flush().unwrap();

        let registry = ConstraintRegistry::new();
        replay_constraint_declarations_from_wal(&wal, &registry).unwrap();

        assert!(
            registry.is_constrained(label, prop),
            "declare without drop must leave constraint active after replay"
        );
    }

    /// Issue #3419 boundary semantics pin: WAL replay must be **idempotent**
    /// for entries whose effects are already present in storage.
    ///
    /// Since the manifest LSN is the next-to-allocate LSN captured *before*
    /// the snapshot is read, an entry at LSN >= manifest.lsn can already be
    /// reflected in the snapshot (a write racing a background persist), and
    /// startup replays from the manifest LSN inclusive. Re-applying such an
    /// entry must be a no-op. Replaying the same WAL twice into the same
    /// storage is exactly that boundary case, maximized.
    ///
    /// Empirically observed WITHOUT the guards:
    /// - a re-applied `CreateNode` appended a duplicate version to the node's
    ///   bi-temporal history (1 version became 2 for a node created once);
    /// - a re-applied `UpdateNode` appended its version with ITSELF as the
    ///   previous head, creating a self-referential delta chain that made
    ///   `get_node_history` loop until the process was OOM-killed.
    #[test]
    fn replay_is_idempotent_for_already_applied_entries() {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvIdemNode").unwrap();
        let other_id = crate::core::NodeId::new(2).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let edge_label = GLOBAL_INTERNER.intern("RCV_IDEM_EDGE").unwrap();

        wal.append(WalOperation::CreateNode {
            node_id,
            label: node_label,
            properties: PropertyMapBuilder::new().insert("v", 1i64).build(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::CreateNode {
            node_id: other_id,
            label: node_label,
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source: node_id,
            target: other_id,
            label: edge_label,
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::UpdateNode {
            node_id,
            version_id: crate::core::VersionId::new(50).unwrap(),
            label: node_label,
            properties: PropertyMapBuilder::new().insert("v", 2i64).build(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::UpdateEdge {
            edge_id,
            version_id: crate::core::VersionId::new(51).unwrap(),
            label: edge_label,
            properties: PropertyMapBuilder::new().insert("w", 1i64).build(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        wal.flush().unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        let (_, _, _, next_vid) =
            replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 1).unwrap();

        let node_history_len = historical.get_node_history(node_id).unwrap().versions.len();
        let edge_history_len = historical.get_edge_history(edge_id).unwrap().versions.len();
        assert_eq!(node_history_len, 2, "create + update");
        assert_eq!(edge_history_len, 2, "create + update");

        // Boundary re-application: replay the SAME entries again.
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), next_vid)
            .expect("re-replay of already-applied entries must succeed");

        // History must not grow, and reconstruction must still terminate.
        assert_eq!(
            historical.get_node_history(node_id).unwrap().versions.len(),
            node_history_len,
            "re-applied entries must not duplicate node history"
        );
        assert_eq!(
            historical.get_edge_history(edge_id).unwrap().versions.len(),
            edge_history_len,
            "re-applied entries must not duplicate edge history"
        );

        // Current state converged to the logged updates.
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.current_version,
            crate::core::VersionId::new(50).unwrap()
        );
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.current_version,
            crate::core::VersionId::new(51).unwrap()
        );
    }

    /// PR #3428 review round 2: an in-WAL `create X → delete X → create X`
    /// sequence with the SAME id (exactly what the recovery property suite
    /// generates) is a RE-CREATION, not a boundary duplicate. The pre-fix
    /// guard treated the tombstone head as "create already applied", reused
    /// the TOMBSTONE's version id for the current-storage insert, and
    /// appended no history version — current then pointed at the dead
    /// incarnation's tombstone and the new incarnation had no bi-temporal
    /// history at all (observed as the property-suite failure
    /// "Current storage label differs from latest version").
    #[test]
    fn replay_recreates_node_after_delete_with_same_id() {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let label_first = GLOBAL_INTERNER.intern("RcvRecreateFirst").unwrap();
        let label_second = GLOBAL_INTERNER.intern("RcvRecreateSecond").unwrap();
        let now = time::now().wallclock();
        let vf_first = crate::core::hlc::HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
        let vf_second = crate::core::hlc::HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

        wal.append(WalOperation::CreateNode {
            node_id,
            label: label_first,
            properties: PropertyMapBuilder::new().insert("v", 1i64).build(),
            valid_from: vf_first,
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::DeleteNode {
            node_id,
            valid_from: time::now(),
            version_id: None,
        })
        .unwrap();
        wal.append(WalOperation::CreateNode {
            node_id,
            label: label_second,
            properties: PropertyMapBuilder::new().insert("v", 2i64).build(),
            valid_from: vf_second,
            provenance: None,
        })
        .unwrap();
        wal.flush().unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 1).unwrap();

        // Current state is the SECOND incarnation.
        let node = current.get_node(node_id).unwrap();
        assert_eq!(node.label, label_second, "current must be the re-creation");
        assert_eq!(node.properties.get("v"), Some(&2i64.into()));

        // History holds create + tombstone + create (3 versions).
        let history = historical.get_node_history(node_id).unwrap();
        assert_eq!(
            history.version_count(),
            3,
            "history must be create + tombstone + re-create"
        );
        assert_eq!(history.versions[0].label, "RcvRecreateFirst");
        assert!(
            !history.versions[1].temporal.valid_time().is_current(),
            "middle version must be the delete tombstone (closed valid interval)"
        );
        assert_eq!(history.versions[2].label, "RcvRecreateSecond");
        assert!(
            history.versions[2].temporal.valid_time().is_current(),
            "re-created head must be live"
        );

        // Current points at the re-creation's version, and that version id is
        // NOT the tombstone's (the pre-fix corruption).
        assert_eq!(node.current_version, history.versions[2].version_id);
        assert_ne!(node.current_version, history.versions[1].version_id);

        // Point-in-time reads across all three states.
        let entries = wal.read_from(LSN::initial()).unwrap();
        let first_create_ts = entries[0].timestamp;
        // Anchoring both dimensions before the delete recalls incarnation 1.
        let recalled = historical
            .get_node_at_time(node_id, first_create_ts, first_create_ts)
            .expect("first incarnation must be recallable before the delete");
        assert_eq!(recalled.label, label_first);
        // At (now, now) the second incarnation is visible.
        let now_node = historical
            .get_node_at_time(node_id, time::now(), time::now())
            .expect("re-created node must be visible now");
        assert_eq!(now_node.label, label_second);
        // Before the re-creation's valid_from (tx = now) nothing is visible.
        let probe = crate::core::hlc::HybridTimestamp::new(now - 2_700_000_000, 0).unwrap();
        assert!(
            historical
                .get_node_at_time(node_id, probe, time::now())
                .is_err(),
            "between delete and re-create (as known now) the node must not exist"
        );
    }

    /// PR #3428 review round 2: edge variant of re-creation after delete.
    #[test]
    fn replay_recreates_edge_after_delete_with_same_id() {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let source = crate::core::NodeId::new(1).unwrap();
        let target = crate::core::NodeId::new(2).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvRecreateEdgeNode").unwrap();
        let label_first = GLOBAL_INTERNER.intern("RCV_RECREATE_FIRST").unwrap();
        let label_second = GLOBAL_INTERNER.intern("RCV_RECREATE_SECOND").unwrap();

        for node_id in [source, target] {
            wal.append(WalOperation::CreateNode {
                node_id,
                label: node_label,
                properties: PropertyMapBuilder::new().build(),
                valid_from: time::now(),
                provenance: None,
            })
            .unwrap();
        }
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label: label_first,
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::DeleteEdge {
            edge_id,
            valid_from: time::now(),
            version_id: None,
        })
        .unwrap();
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label: label_second,
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        wal.flush().unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 1).unwrap();

        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(edge.label, label_second);

        let history = historical.get_edge_history(edge_id).unwrap();
        assert_eq!(history.version_count(), 3, "create + tombstone + re-create");
        assert!(!history.versions[1].temporal.valid_time().is_current());
        assert_eq!(edge.current_version, history.versions[2].version_id);
        assert_ne!(edge.current_version, history.versions[1].version_id);
    }

    /// PR #3428 review round 2 (fix 3, case a): background persistence can
    /// snapshot the two stores at different LSNs. If the node is still in the
    /// CURRENT snapshot but the tombstone is ALREADY in the historical
    /// snapshot, replaying the boundary delete must remove the node from
    /// current WITHOUT appending a second tombstone.
    #[test]
    fn replay_delete_skips_historical_side_when_tombstone_already_applied() {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let other_id = crate::core::NodeId::new(2).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvAsymDelNode").unwrap();
        let edge_label = GLOBAL_INTERNER.intern("RCV_ASYM_DEL_EDGE").unwrap();
        let props = PropertyMapBuilder::new().insert("v", 1i64).build();
        let t_create = time::now();

        // Current snapshot: node + edge still present.
        let current = CurrentStorage::new();
        let metadata = VersionMetadata::new(TxId::new(0), t_create);
        current
            .insert_node_direct(
                Node::with_metadata(
                    node_id,
                    node_label,
                    props.clone(),
                    VersionId::new(1).unwrap(),
                    metadata,
                ),
                t_create,
            )
            .unwrap();
        current
            .insert_edge_direct(Edge::with_metadata(
                edge_id,
                edge_label,
                node_id,
                other_id,
                props.clone(),
                VersionId::new(3).unwrap(),
                metadata,
            ))
            .unwrap();

        // Historical snapshot: the delete was ALREADY applied (newer
        // snapshot): create version closed + tombstone appended.
        let mut historical = HistoricalStorage::new();
        let t_delete = time::now();
        historical
            .add_node_version(
                node_id,
                VersionId::new(1).unwrap(),
                t_create,
                t_create,
                node_label,
                props.clone(),
                false,
            )
            .unwrap();
        historical
            .close_node_version_transaction_time(VersionId::new(1).unwrap(), t_delete)
            .unwrap();
        historical
            .add_node_version(
                node_id,
                VersionId::new(2).unwrap(),
                t_delete,
                t_delete,
                node_label,
                props.clone(),
                true, // tombstone already present
            )
            .unwrap();
        historical
            .add_edge_version(
                edge_id,
                VersionId::new(3).unwrap(),
                t_create,
                t_create,
                edge_label,
                node_id,
                other_id,
                props.clone(),
                false,
            )
            .unwrap();
        historical
            .close_edge_version_transaction_time(VersionId::new(3).unwrap(), t_delete)
            .unwrap();
        historical
            .add_edge_version(
                edge_id,
                VersionId::new(4).unwrap(),
                t_delete,
                t_delete,
                edge_label,
                node_id,
                other_id,
                props,
                true, // tombstone already present
            )
            .unwrap();

        wal.append(WalOperation::DeleteEdge {
            edge_id,
            valid_from: time::now(),
            version_id: None,
        })
        .unwrap();
        wal.append(WalOperation::DeleteNode {
            node_id,
            valid_from: time::now(),
            version_id: None,
        })
        .unwrap();
        wal.flush().unwrap();

        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 10).unwrap();

        // Current side applied; historical side untouched (no 2nd tombstone).
        assert!(current.get_node(node_id).is_err(), "node must be removed");
        assert!(current.get_edge(edge_id).is_err(), "edge must be removed");
        assert_eq!(
            historical
                .get_node_history(node_id)
                .unwrap()
                .version_count(),
            2,
            "no duplicate node tombstone may be appended"
        );
        assert_eq!(
            historical
                .get_edge_history(edge_id)
                .unwrap()
                .version_count(),
            2,
            "no duplicate edge tombstone may be appended"
        );
    }

    /// PR #3428 review round 2 (fix 3, case b): node already ABSENT from the
    /// current snapshot but the historical head is still LIVE (older
    /// historical snapshot). The pre-fix code gated the whole delete on
    /// current presence and skipped entirely — leaving the head open forever.
    /// The historical closure + tombstone must be applied from history alone.
    #[test]
    fn replay_delete_closes_historical_head_when_current_already_removed() {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let other_id = crate::core::NodeId::new(2).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvAsymDel2Node").unwrap();
        let edge_label = GLOBAL_INTERNER.intern("RCV_ASYM_DEL2_EDGE").unwrap();
        let props = PropertyMapBuilder::new().insert("v", 7i64).build();
        let t_create = time::now();

        // Current snapshot: entity already gone. Historical: head still live.
        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        historical
            .add_node_version(
                node_id,
                VersionId::new(1).unwrap(),
                t_create,
                t_create,
                node_label,
                props.clone(),
                false,
            )
            .unwrap();
        historical
            .add_edge_version(
                edge_id,
                VersionId::new(2).unwrap(),
                t_create,
                t_create,
                edge_label,
                node_id,
                other_id,
                props,
                false,
            )
            .unwrap();

        wal.append(WalOperation::DeleteEdge {
            edge_id,
            valid_from: time::now(),
            version_id: None,
        })
        .unwrap();
        wal.append(WalOperation::DeleteNode {
            node_id,
            valid_from: time::now(),
            version_id: None,
        })
        .unwrap();
        wal.flush().unwrap();

        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 10).unwrap();

        // Historical side applied even though current had nothing to remove.
        let node_history = historical.get_node_history(node_id).unwrap();
        assert_eq!(node_history.version_count(), 2, "create + tombstone");
        assert!(
            !node_history.versions[0]
                .temporal
                .transaction_time()
                .is_current(),
            "prior node head's transaction time must be closed"
        );
        assert!(
            !node_history.versions[1].temporal.valid_time().is_current(),
            "node head must now be a tombstone"
        );
        let edge_history = historical.get_edge_history(edge_id).unwrap();
        assert_eq!(edge_history.version_count(), 2, "create + tombstone");
        assert!(
            !edge_history.versions[1].temporal.valid_time().is_current(),
            "edge head must now be a tombstone"
        );
        assert!(current.get_node(node_id).is_err());
        assert!(current.get_edge(edge_id).is_err());
    }

    /// PR #3428 review round 2 (fix 4): when the entity is in the CURRENT
    /// snapshot but missing from the historical snapshot, the boundary create
    /// must reuse the current entity's `current_version` as the historical
    /// version id so `current.current_version == historical head id`.
    #[test]
    fn replay_create_aligns_history_with_current_only_snapshot() {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let other_id = crate::core::NodeId::new(2).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvAlignCurNode").unwrap();
        let edge_label = GLOBAL_INTERNER.intern("RCV_ALIGN_CUR_EDGE").unwrap();
        let props = PropertyMapBuilder::new().insert("v", 1i64).build();
        let t_create = time::now();
        let node_vid = VersionId::new(7).unwrap();
        let edge_vid = VersionId::new(8).unwrap();

        // Current snapshot has the entities with their persisted version ids;
        // the historical snapshot lagged and has nothing.
        let current = CurrentStorage::new();
        let metadata = VersionMetadata::new(TxId::new(0), t_create);
        current
            .insert_node_direct(
                Node::with_metadata(node_id, node_label, props.clone(), node_vid, metadata),
                t_create,
            )
            .unwrap();
        current
            .insert_edge_direct(Edge::with_metadata(
                edge_id,
                edge_label,
                node_id,
                other_id,
                props.clone(),
                edge_vid,
                metadata,
            ))
            .unwrap();
        let mut historical = HistoricalStorage::new();

        wal.append(WalOperation::CreateNode {
            node_id,
            label: node_label,
            properties: props.clone(),
            valid_from: t_create,
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source: node_id,
            target: other_id,
            label: edge_label,
            properties: props,
            valid_from: t_create,
            provenance: None,
        })
        .unwrap();
        wal.flush().unwrap();

        // Deliberately pass a next_version_id that would NOT match: the
        // replay must align to current, not allocate 100/101.
        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 100).unwrap();

        let node_history = historical.get_node_history(node_id).unwrap();
        assert_eq!(node_history.version_count(), 1);
        assert_eq!(
            node_history.versions[0].version_id, node_vid,
            "historical version id must reuse current.current_version"
        );
        assert_eq!(current.get_node(node_id).unwrap().current_version, node_vid);

        let edge_history = historical.get_edge_history(edge_id).unwrap();
        assert_eq!(edge_history.version_count(), 1);
        assert_eq!(
            edge_history.versions[0].version_id, edge_vid,
            "historical version id must reuse current.current_version"
        );
        assert_eq!(current.get_edge(edge_id).unwrap().current_version, edge_vid);
    }

    /// PR #3428 review round 2 (fix 4, mirror order): entity in the
    /// HISTORICAL snapshot but missing from the current snapshot — the
    /// current insert must reuse the historical head's version id
    /// (pre-existing behavior, pinned here alongside the new mirror case).
    #[test]
    fn replay_create_aligns_current_with_historical_only_snapshot() {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::time;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let node_id = crate::core::NodeId::new(1).unwrap();
        let edge_id = crate::core::EdgeId::new(1).unwrap();
        let other_id = crate::core::NodeId::new(2).unwrap();
        let node_label = GLOBAL_INTERNER.intern("RcvAlignHistNode").unwrap();
        let edge_label = GLOBAL_INTERNER.intern("RCV_ALIGN_HIST_EDGE").unwrap();
        let props = PropertyMapBuilder::new().insert("v", 1i64).build();
        let t_create = time::now();
        let node_vid = VersionId::new(9).unwrap();
        let edge_vid = VersionId::new(10).unwrap();

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();
        historical
            .add_node_version(
                node_id,
                node_vid,
                t_create,
                t_create,
                node_label,
                props.clone(),
                false,
            )
            .unwrap();
        historical
            .add_edge_version(
                edge_id,
                edge_vid,
                t_create,
                t_create,
                edge_label,
                node_id,
                other_id,
                props.clone(),
                false,
            )
            .unwrap();

        wal.append(WalOperation::CreateNode {
            node_id,
            label: node_label,
            properties: props.clone(),
            valid_from: t_create,
            provenance: None,
        })
        .unwrap();
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source: node_id,
            target: other_id,
            label: edge_label,
            properties: props,
            valid_from: t_create,
            provenance: None,
        })
        .unwrap();
        wal.flush().unwrap();

        replay_wal_into_storage(&wal, &current, &mut historical, LSN::initial(), 100).unwrap();

        assert_eq!(
            current.get_node(node_id).unwrap().current_version,
            node_vid,
            "current must adopt the historical head's version id"
        );
        assert_eq!(
            current.get_edge(edge_id).unwrap().current_version,
            edge_vid,
            "current must adopt the historical head's version id"
        );
        assert_eq!(
            historical
                .get_node_history(node_id)
                .unwrap()
                .version_count(),
            1,
            "no duplicate history version may be appended"
        );
        assert_eq!(
            historical
                .get_edge_history(edge_id)
                .unwrap()
                .version_count(),
            1,
            "no duplicate history version may be appended"
        );
    }
}
