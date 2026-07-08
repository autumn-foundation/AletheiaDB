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
use crate::core::version::VersionMetadata;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::storage::wal::{LSN, WalOperation};

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
    mut next_version_id: u64,
    constraint_registry: Option<&ConstraintRegistry>,
) -> Result<(LSN, Option<u64>, Option<u64>, u64)> {
    const RECOVERY_TX_ID: u64 = 0;

    let mut max_node_id: Option<u64> = None;
    let mut max_edge_id: Option<u64> = None;

    let wal_entries = wal.read_from(start_lsn)?;

    if !wal_entries.is_empty() {
        #[cfg(feature = "observability")]
        tracing::info!(
            "Replaying {} WAL entries from LSN {}",
            wal_entries.len(),
            start_lsn.0
        );
        #[cfg(not(feature = "observability"))]
        eprintln!(
            "Replaying {} WAL entries from LSN {}",
            wal_entries.len(),
            start_lsn.0
        );
    }

    for entry in wal_entries {
        match entry.operation {
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

                // Transaction time comes from when the WAL entry was logged
                let commit_timestamp = entry.timestamp;
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
                    // Re-creation after a tombstone/retraction: supersede the
                    // closed head in transaction time (mirroring the
                    // UpdateNode arm) before appending the new incarnation.
                    if let Some(prev) = historical_head {
                        historical.close_node_version_transaction_time(prev, commit_timestamp)?;
                    }
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

                let commit_timestamp = entry.timestamp;
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
                    if let Some(prev) = historical_head {
                        historical.close_edge_version_transaction_time(prev, commit_timestamp)?;
                    }
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

                let commit_timestamp = entry.timestamp;
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
                    if let Some(prev_version_id) = historical.get_current_node_version(node_id) {
                        historical.close_node_version_transaction_time(
                            prev_version_id,
                            commit_timestamp,
                        )?;
                    }

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

                let commit_timestamp = entry.timestamp;
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
                    if let Some(prev_version_id) = historical.get_current_edge_version(edge_id) {
                        historical.close_edge_version_transaction_time(
                            prev_version_id,
                            commit_timestamp,
                        )?;
                    }

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
                valid_from: _,
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
                let commit_timestamp = entry.timestamp;
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
                    if let Some(current_version_id) = historical_head {
                        historical.close_node_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let tombstone_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    // Tombstones use commit_timestamp for both valid_from and tx_time
                    // The is_tombstone=true flag closes the valid_time immediately
                    historical.add_node_version(
                        node_id,
                        tombstone_version_id,
                        commit_timestamp,
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
                valid_from: _,
            } => {
                // Per-store re-application guard — see the DeleteNode arm
                // above (review round 2, #3428).
                let commit_timestamp = entry.timestamp;
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
                    if let Some(current_version_id) = historical_head {
                        historical.close_edge_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let tombstone_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    // Tombstones use commit_timestamp for both valid_from and tx_time
                    // The is_tombstone=true flag closes the valid_time immediately
                    historical.add_edge_version(
                        edge_id,
                        tombstone_version_id,
                        commit_timestamp,
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
            WalOperation::RetractNode { node_id, valid_to } => {
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
                let commit_timestamp = entry.timestamp;
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

                    if let Some(current_version_id) = head_version_id {
                        historical.close_node_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let retraction_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

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
            WalOperation::RetractEdge { edge_id, valid_to } => {
                // Valid-time retraction of an edge (Issue #3230); mirrors the
                // RetractNode arm above, honoring the logged `valid_to`, with
                // the same per-store re-application guards (#3428).
                let commit_timestamp = entry.timestamp;
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

                    if let Some(current_version_id) = head_version_id {
                        historical.close_edge_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let retraction_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

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
        }
    }

    let final_lsn = wal.current_lsn();

    Ok((final_lsn, max_node_id, max_edge_id, next_version_id))
}

/// Replay ONLY constraint declaration/drop entries from the full WAL history.
///
/// Called during index-persistence startup BEFORE the regular snapshot-based
/// WAL replay.  Because constraint declarations are written to the WAL before
/// the corresponding node data, they may lie at LSNs below the persisted
/// snapshot LSN and therefore be skipped by the normal differential replay.
/// This pass reads every WAL entry from LSN 0 and replays only the two
/// constraint-related operations, giving us the net constraint state.
pub(crate) fn replay_constraint_declarations_from_wal(
    wal: &ConcurrentWalSystem,
    registry: &ConstraintRegistry,
) -> Result<()> {
    let all_entries = wal.read_from(LSN::initial())?;
    for entry in all_entries {
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
    Ok(())
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
        wal.append(WalOperation::RetractNode { node_id, valid_to })
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
        wal.append(WalOperation::RetractEdge { edge_id, valid_to })
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
        })
        .unwrap();
        wal.append(WalOperation::DeleteNode {
            node_id,
            valid_from: time::now(),
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
        })
        .unwrap();
        wal.append(WalOperation::DeleteNode {
            node_id,
            valid_from: time::now(),
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
