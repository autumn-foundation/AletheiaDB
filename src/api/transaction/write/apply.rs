//! Apply buffered writes to storage.
//!
//! This module handles the "Apply" phase of the commit protocol:
//! 1.  Update current state (fast path)
//! 2.  Archive previous state to historical storage (temporal path)
//! 3.  Update temporal indexes (time travel path)
//!
//! # Atomicity
//!
//! Operations in this module are executed after validation and conflict detection
//! have passed. They must be applied atomically (all or nothing). Since we are
//! in the commit phase, we assume success unless a catastrophic storage failure
//! occurs.
//!
//! # Temporal Semantics
//!
//! - **Valid Time**: Controlled by the user (or defaults to commit time).
//! - **Transaction Time**: Controlled by the system (always commit time).
//! - **Tombstones**: Deletions create "tombstone" versions that close the valid time interval.
//!
//! # Lock Acquisition Order
//!
//! Write commits acquire synchronization in this order:
//! 1. `current_timestamp`
//! 2. `wal`
//! 3. `historical`
//! 4. `temporal_indexes`
//! 5. `id generators`
//! 6. `outgoing`
//! 7. `incoming`
//!
//! `commit()` handles `current_timestamp` and `wal` before entering this module.
//! `apply_changes()` then acquires `historical` once for the whole batch. The
//! current `temporal_indexes`, `id generators`, `outgoing`, and `incoming`
//! adjacency storage use atomic or fine-grained internal synchronization, but the
//! ordering is still the contract for future explicit locks: do not acquire an
//! earlier entry while holding a later one.

use super::WriteTransaction;
use crate::core::error::{Result, StorageError};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::provenance::Provenance;
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use crate::core::version::VersionMetadata;
use crate::storage::historical::HistoricalStorage;
use std::sync::Arc;

/// Helper function to create a bi-temporal interval with proper closing logic.
///
/// For standard versions, the interval is open-ended `[start, infinity)`.
/// For tombstones (deletions), the valid-time interval is closed immediately `[start, start)`,
/// representing that the entity is no longer valid from that point onward.
#[inline]
pub(crate) fn create_temporal_interval(
    valid_from: Timestamp,
    tx_time: Timestamp,
    is_tombstone: bool,
) -> Result<BiTemporalInterval> {
    let mut temporal = BiTemporalInterval::with_valid_time(valid_from, tx_time);
    if is_tombstone {
        temporal = temporal.close_valid_time(valid_from)?;
    }
    Ok(temporal)
}

/// Apply a node creation or update to storage.
///
/// This performs three actions:
/// 1.  Updates `CurrentStorage` with the new node state.
/// 2.  Adds a new version to `HistoricalStorage`.
/// 3.  Updates `TemporalIndexes` for time-travel queries.
///
/// If this is an update, it also closes the transaction time of the previous version
/// in historical storage to maintain history continuity.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_node_write(
    tx: &WriteTransaction,
    is_create: bool,
    node_id: NodeId,
    version_id: VersionId,
    label: InternedString,
    properties: PropertyMap,
    valid_from: Timestamp,
    commit_timestamp: Timestamp,
    historical: &mut HistoricalStorage,
    provenance: Option<Arc<Provenance>>,
) -> Result<()> {
    // Create node with pending metadata (commit_timestamp finalized after full apply_changes).
    // Using uncommitted here prevents phantom visibility if apply_changes fails partway through.
    let metadata = VersionMetadata::uncommitted(tx.tx_id);
    let node = Node::with_metadata(node_id, label, properties.clone(), version_id, metadata);

    // Insert or update in current storage
    if is_create {
        tx.current.insert_node_direct(node, commit_timestamp)?;
    } else {
        tx.current.update_node_direct(node, commit_timestamp)?;

        if let Some(current_version_id) = historical.get_current_node_version(node_id) {
            historical.close_node_version_transaction_time(current_version_id, commit_timestamp)?;
        }
    }

    // Store in historical storage (consume properties, avoiding second clone)
    historical.add_node_version_with_provenance(
        node_id,
        version_id,
        valid_from,
        commit_timestamp,
        label,
        properties,
        false, // not a tombstone
        provenance,
    )?;

    // Index in temporal indexes with bi-temporal interval
    let temporal = create_temporal_interval(valid_from, commit_timestamp, false)?;
    tx.temporal_indexes
        .insert_node_version(node_id, version_id, temporal)?;

    Ok(())
}

/// Apply an edge creation or update to storage.
///
/// Similar to `apply_node_write`, this updates current storage, historical storage,
/// and temporal indexes.
///
/// # Referential Integrity
///
/// This function assumes that `source` and `target` nodes exist. Referential integrity
/// checks should be performed during the validation phase (see `validation::validate`),
/// not during application.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_edge_write(
    tx: &WriteTransaction,
    is_create: bool,
    edge_id: EdgeId,
    version_id: VersionId,
    source: NodeId,
    target: NodeId,
    label: InternedString,
    properties: PropertyMap,
    valid_from: Timestamp,
    commit_timestamp: Timestamp,
    historical: &mut HistoricalStorage,
    provenance: Option<Arc<Provenance>>,
) -> Result<()> {
    // Create edge with pending metadata (commit_timestamp finalized after full apply_changes).
    let metadata = VersionMetadata::uncommitted(tx.tx_id);
    let edge = Edge::with_metadata(
        edge_id,
        label,
        source,
        target,
        properties.clone(),
        version_id,
        metadata,
    );

    // Insert or update in current storage
    if is_create {
        tx.current.insert_edge_direct(edge)?;
    } else {
        tx.current.update_edge_direct(edge)?;

        if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
            historical.close_edge_version_transaction_time(current_version_id, commit_timestamp)?;
        }
    }

    // Store in historical storage (consume properties, avoiding second clone)
    historical.add_edge_version_with_provenance(
        edge_id,
        version_id,
        valid_from,
        commit_timestamp,
        label,
        source,
        target,
        properties,
        false, // not a tombstone
        provenance,
    )?;

    // Index in temporal indexes with bi-temporal interval
    let temporal = create_temporal_interval(valid_from, commit_timestamp, false)?;
    tx.temporal_indexes
        .insert_edge_version(edge_id, version_id, temporal)?;

    Ok(())
}

/// Apply a node deletion.
///
/// Deletion involves:
/// 1.  Closing the current version in historical storage.
/// 2.  Creating a "tombstone" version in historical storage to mark the end of validity.
/// 3.  Removing the node from current storage.
///
/// # Tombstones
///
/// A tombstone is a version with `is_tombstone = true`. Its valid-time interval
/// is effectively empty `[valid_from, valid_from)`, indicating that the entity
/// ceases to exist at that point.
pub(crate) fn apply_node_delete(
    tx: &WriteTransaction,
    node_id: NodeId,
    valid_from: Timestamp,
    commit_timestamp: Timestamp,
    tombstone_id: VersionId,
    historical: &mut HistoricalStorage,
) -> Result<()> {
    // Get the node before deleting
    let node = tx.current.get_node(node_id)?;

    // Close the current version's transaction_time in historical storage
    if let Some(current_version_id) = historical.get_current_node_version(node_id) {
        historical.close_node_version_transaction_time(current_version_id, commit_timestamp)?;
    }

    // Create tombstone interval using centralized logic
    let tombstone_temporal = create_temporal_interval(valid_from, commit_timestamp, true)?;

    // Add tombstone version to historical storage
    historical.add_node_version(
        node_id,
        tombstone_id,
        valid_from,
        commit_timestamp,
        node.label,
        node.properties.clone(),
        true, // is_tombstone
    )?;

    // Index the tombstone version with the same interval
    tx.temporal_indexes
        .insert_node_version(node_id, tombstone_id, tombstone_temporal)?;

    // Delete from current storage
    tx.current.delete_node_direct(node_id, commit_timestamp)?;

    Ok(())
}

/// Apply an edge deletion.
///
/// Similar to `apply_node_delete`, creates a tombstone and removes from current storage.
pub(crate) fn apply_edge_delete(
    tx: &WriteTransaction,
    edge_id: EdgeId,
    valid_from: Timestamp,
    commit_timestamp: Timestamp,
    tombstone_id: VersionId,
    historical: &mut HistoricalStorage,
) -> Result<()> {
    // Get the edge before deleting
    let edge = tx.current.get_edge(edge_id)?;

    // Close the current version's transaction_time in historical storage
    if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
        historical.close_edge_version_transaction_time(current_version_id, commit_timestamp)?;
    }

    // Create tombstone interval using centralized logic
    let tombstone_temporal = create_temporal_interval(valid_from, commit_timestamp, true)?;

    // Add tombstone version to historical storage
    historical.add_edge_version(
        edge_id,
        tombstone_id,
        valid_from,
        commit_timestamp,
        edge.label,
        edge.source,
        edge.target,
        edge.properties.clone(),
        true, // is_tombstone
    )?;

    // Index the tombstone version with the same interval
    tx.temporal_indexes
        .insert_edge_version(edge_id, tombstone_id, tombstone_temporal)?;

    // Delete from current storage
    tx.current.delete_edge_direct(edge_id)?;

    Ok(())
}

/// Apply a node retraction (Issue #3230).
///
/// Retraction closes the node's valid-time interval at `valid_to` without
/// deleting its history:
/// 1.  Closes the head version's TRANSACTION time at the commit timestamp,
///     leaving its valid interval untouched (still open-ended) — this is
///     what makes `AS OF SYSTEM_TIME` before the retraction show the fact
///     open-ended (append-only, never rewrite).
/// 2.  Appends a NEW version with the same properties and the closed valid
///     interval `[valid_from, valid_to)` (tx time `[commit, infinity)`).
/// 3.  Removes the node from current storage and updates temporal indexes.
///
/// After retraction the node is absent from current-state queries, like a
/// delete, while the full history remains queryable.
pub(crate) fn apply_node_retract(
    tx: &WriteTransaction,
    node_id: NodeId,
    valid_to: Timestamp,
    commit_timestamp: Timestamp,
    retraction_version_id: VersionId,
    historical: &mut HistoricalStorage,
) -> Result<()> {
    // Get the node before removing it from current storage
    let node = tx.current.get_node(node_id)?;

    // Capture the head's valid_from BEFORE mutating, so the retraction
    // version reproduces the same interval start.
    let head_version_id = historical.get_current_node_version(node_id);
    let valid_from = head_version_id
        .and_then(|vid| historical.get_node_version(vid))
        .map(|v| v.temporal.valid_time().start())
        .unwrap_or(commit_timestamp);

    // (1) Close the head version's transaction time only.
    if let Some(current_version_id) = head_version_id {
        historical.close_node_version_transaction_time(current_version_id, commit_timestamp)?;
    }

    // (2) Append the retraction version with the closed valid interval.
    historical.add_retracted_node_version(
        node_id,
        retraction_version_id,
        valid_from,
        valid_to,
        commit_timestamp,
        node.label,
        node.properties.clone(),
    )?;

    // Index the retraction version with the same closed interval.
    let temporal = BiTemporalInterval::with_valid_time(valid_from, commit_timestamp)
        .close_valid_time(valid_to)?;
    tx.temporal_indexes
        .insert_node_version(node_id, retraction_version_id, temporal)?;

    // (3) Remove from current storage.
    tx.current.delete_node_direct(node_id, commit_timestamp)?;

    Ok(())
}

/// Apply an edge retraction (Issue #3230).
///
/// See [`apply_node_retract`] for the bi-temporal contract.
pub(crate) fn apply_edge_retract(
    tx: &WriteTransaction,
    edge_id: EdgeId,
    valid_to: Timestamp,
    commit_timestamp: Timestamp,
    retraction_version_id: VersionId,
    historical: &mut HistoricalStorage,
) -> Result<()> {
    // Get the edge before removing it from current storage
    let edge = tx.current.get_edge(edge_id)?;

    let head_version_id = historical.get_current_edge_version(edge_id);
    let valid_from = head_version_id
        .and_then(|vid| historical.get_edge_version(vid))
        .map(|v| v.temporal.valid_time().start())
        .unwrap_or(commit_timestamp);

    // (1) Close the head version's transaction time only.
    if let Some(current_version_id) = head_version_id {
        historical.close_edge_version_transaction_time(current_version_id, commit_timestamp)?;
    }

    // (2) Append the retraction version with the closed valid interval.
    historical.add_retracted_edge_version(
        edge_id,
        retraction_version_id,
        valid_from,
        valid_to,
        commit_timestamp,
        edge.label,
        edge.source,
        edge.target,
        edge.properties.clone(),
    )?;

    let temporal = BiTemporalInterval::with_valid_time(valid_from, commit_timestamp)
        .close_valid_time(valid_to)?;
    tx.temporal_indexes
        .insert_edge_version(edge_id, retraction_version_id, temporal)?;

    // (3) Remove from current storage.
    tx.current.delete_edge_direct(edge_id)?;

    Ok(())
}

/// Apply a single buffered write operation.
///
/// Dispatches to the appropriate `apply_*` function based on the operation type.
///
/// # Arguments
///
/// * `tombstone_ids` - Iterator of pre-generated Version IDs for tombstones. This optimization
///   avoids acquiring the ID generator lock for every delete operation.
pub(crate) fn apply_single_write(
    tx: &WriteTransaction,
    write: &crate::api::transaction::BufferedWrite,
    commit_timestamp: Timestamp,
    historical: &mut HistoricalStorage,
    tombstone_ids: &mut std::vec::IntoIter<u64>,
    num_deletes: usize,
) -> Result<()> {
    match write {
        crate::api::transaction::BufferedWrite::CreateNode {
            node_id,
            version_id,
            label,
            properties,
            valid_from,
            provenance,
        }
        | crate::api::transaction::BufferedWrite::UpdateNode {
            node_id,
            version_id,
            label,
            properties,
            valid_from,
            provenance,
        } => {
            let is_create = matches!(
                write,
                crate::api::transaction::BufferedWrite::CreateNode { .. }
            );
            apply_node_write(
                tx,
                is_create,
                *node_id,
                *version_id,
                *label,
                properties.clone(),
                *valid_from,
                commit_timestamp,
                historical,
                provenance.clone(),
            )?;
        }
        crate::api::transaction::BufferedWrite::CreateEdge {
            edge_id,
            version_id,
            source,
            target,
            label,
            properties,
            valid_from,
            provenance,
        }
        | crate::api::transaction::BufferedWrite::UpdateEdge {
            edge_id,
            version_id,
            source,
            target,
            label,
            properties,
            valid_from,
            provenance,
        } => {
            let is_create = matches!(
                write,
                crate::api::transaction::BufferedWrite::CreateEdge { .. }
            );
            apply_edge_write(
                tx,
                is_create,
                *edge_id,
                *version_id,
                *source,
                *target,
                *label,
                properties.clone(),
                *valid_from,
                commit_timestamp,
                historical,
                provenance.clone(),
            )?;
        }
        crate::api::transaction::BufferedWrite::DeleteNode {
            node_id,
            valid_from,
        } => {
            // Use pre-generated tombstone version ID (no lock needed)
            let tombstone_version_id = VersionId::new_unchecked(tombstone_ids.next().ok_or_else(|| {
                StorageError::InconsistentState {
                    reason: format!(
                        "Tombstone ID exhaustion for DeleteNode: expected {} deletes, iterator depleted at node_id {:?}",
                        num_deletes, node_id
                    ),
                }
            })?);

            apply_node_delete(
                tx,
                *node_id,
                *valid_from,
                commit_timestamp,
                tombstone_version_id,
                historical,
            )?;
        }
        crate::api::transaction::BufferedWrite::DeleteEdge {
            edge_id,
            valid_from,
        } => {
            // Use pre-generated tombstone version ID (no lock needed)
            let tombstone_version_id = VersionId::new_unchecked(tombstone_ids.next().ok_or_else(|| {
                StorageError::InconsistentState {
                    reason: format!(
                        "Tombstone ID exhaustion for DeleteEdge: expected {} deletes, iterator depleted at edge_id {:?}",
                        num_deletes, edge_id
                    ),
                }
            })?);

            apply_edge_delete(
                tx,
                *edge_id,
                *valid_from,
                commit_timestamp,
                tombstone_version_id,
                historical,
            )?;
        }
        crate::api::transaction::BufferedWrite::RetractNode { node_id, valid_to } => {
            // Retractions draw from the same pre-generated version ID pool
            // as deletes (both append exactly one closing version).
            let retraction_version_id = VersionId::new_unchecked(tombstone_ids.next().ok_or_else(|| {
                StorageError::InconsistentState {
                    reason: format!(
                        "Version ID exhaustion for RetractNode: expected {} closing versions, iterator depleted at node_id {:?}",
                        num_deletes, node_id
                    ),
                }
            })?);

            apply_node_retract(
                tx,
                *node_id,
                *valid_to,
                commit_timestamp,
                retraction_version_id,
                historical,
            )?;
        }
        crate::api::transaction::BufferedWrite::RetractEdge { edge_id, valid_to } => {
            let retraction_version_id = VersionId::new_unchecked(tombstone_ids.next().ok_or_else(|| {
                StorageError::InconsistentState {
                    reason: format!(
                        "Version ID exhaustion for RetractEdge: expected {} closing versions, iterator depleted at edge_id {:?}",
                        num_deletes, edge_id
                    ),
                }
            })?);

            apply_edge_retract(
                tx,
                *edge_id,
                *valid_to,
                commit_timestamp,
                retraction_version_id,
                historical,
            )?;
        }
    }
    Ok(())
}

/// Apply all buffered changes in the transaction.
///
/// This is the main entry point for the "Apply" phase. It iterates through the
/// write buffer and applies each operation to the storage engine.
///
/// # Locking Strategy
///
/// This function acquires the write lock on `HistoricalStorage` *once* and holds it
/// for the duration of all operations. This avoids lock thrashing (acquiring/releasing
/// for every single write) and ensures atomicity of the historical updates.
///
/// # Performance
///
/// - **Locking**: Single lock acquisition for historical storage.
/// - **ID Generation**: Tombstone IDs are pre-generated in a batch to minimize lock contention.
/// - **Adjacency**: Adjacency indexes are compacted *once* after all edge operations are complete.
pub(crate) fn apply_changes(
    tx: &WriteTransaction,
    commit_timestamp: Timestamp,
    closing_version_ids: &[VersionId],
) -> Result<()> {
    // Create temporal interval for all operations in this transaction.
    let _temporal = BiTemporalInterval::current(commit_timestamp);

    // Acquire lock on historical storage once before processing all operations.
    let mut historical = tx.historical.write();

    // Issue #3406: the closing-version IDs (delete tombstones + retraction
    // versions) are pre-generated once per commit — BEFORE the WAL log phase —
    // so the identical ids are recorded in the WAL and applied here. We simply
    // replay them in the same buffer order they were generated and logged.
    let num_deletes = closing_version_ids.len();
    let mut tombstone_ids = closing_version_ids
        .iter()
        .map(|v| v.as_u64())
        .collect::<Vec<u64>>()
        .into_iter();

    for write in tx.buffer.operations() {
        apply_single_write(
            tx,
            write,
            commit_timestamp,
            &mut historical,
            &mut tombstone_ids,
            num_deletes,
        )?;
    }

    // Safety check
    debug_assert!(
        tombstone_ids.next().is_none(),
        "Tombstone ID surplus: expected {} deletes, but iterator has remaining IDs",
        num_deletes
    );

    drop(historical);

    // With incremental adjacency indexes, edge updates are immediately visible
    // via merged reads. Background compaction handles delta->frozen promotion.

    Ok(())
}

/// Finalize commit timestamps in current storage after a successful `apply_changes`.
///
/// During `apply_changes`, nodes and edges are written to current storage with
/// `commit_timestamp: None` (via `VersionMetadata::uncommitted`).  This prevents
/// phantom visibility if `apply_changes` fails partway through: an aborted
/// transaction's partial writes stay invisible because `is_visible_with_embedded_ts`
/// treats `None` as uncommitted.
///
/// This function is called only on the success path, just before `register_commit`.
/// It sets `commit_timestamp: Some(T)` on every node/edge written in this
/// transaction, making them visible to snapshots taken after the commit.
pub(crate) fn finalize_current_commit_timestamps(
    tx: &WriteTransaction,
    commit_timestamp: Timestamp,
) {
    for write in tx.buffer.operations() {
        match write {
            crate::api::transaction::BufferedWrite::CreateNode { node_id, .. }
            | crate::api::transaction::BufferedWrite::UpdateNode { node_id, .. } => {
                tx.current
                    .set_node_commit_timestamp(*node_id, commit_timestamp);
            }
            crate::api::transaction::BufferedWrite::CreateEdge { edge_id, .. }
            | crate::api::transaction::BufferedWrite::UpdateEdge { edge_id, .. } => {
                tx.current
                    .set_edge_commit_timestamp(*edge_id, commit_timestamp);
            }
            // Deletes remove the node/edge from current storage; no finalization needed.
            _ => {}
        }
    }
}
