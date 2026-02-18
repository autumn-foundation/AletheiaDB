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

use super::WriteTransaction;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use crate::core::version::VersionMetadata;
use crate::storage::historical::HistoricalStorage;
use crate::core::error::{Result, StorageError};

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
) -> Result<()> {
    // Create node with proper transaction metadata
    let metadata = VersionMetadata::new(tx.tx_id, commit_timestamp);
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
    historical.add_node_version(
        node_id,
        version_id,
        valid_from,
        commit_timestamp,
        label,
        properties,
        false, // not a tombstone
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
) -> Result<()> {
    // Create edge with proper transaction metadata
    let metadata = VersionMetadata::new(tx.tx_id, commit_timestamp);
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
    historical.add_edge_version(
        edge_id,
        version_id,
        valid_from,
        commit_timestamp,
        label,
        source,
        target,
        properties,
        false, // not a tombstone
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
        } => {
            apply_node_write(
                tx,
                true, // is_create
                *node_id,
                *version_id,
                *label,
                properties.clone(),
                *valid_from,
                commit_timestamp,
                historical,
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
        } => {
            apply_edge_write(
                tx,
                true, // is_create
                *edge_id,
                *version_id,
                *source,
                *target,
                *label,
                properties.clone(),
                *valid_from,
                commit_timestamp,
                historical,
            )?;
        }
        crate::api::transaction::BufferedWrite::UpdateNode {
            node_id,
            version_id,
            label,
            properties,
            valid_from,
        } => {
            apply_node_write(
                tx,
                false, // is_create
                *node_id,
                *version_id,
                *label,
                properties.clone(),
                *valid_from,
                commit_timestamp,
                historical,
            )?;
        }
        crate::api::transaction::BufferedWrite::UpdateEdge {
            edge_id,
            version_id,
            source,
            target,
            label,
            properties,
            valid_from,
        } => {
            apply_edge_write(
                tx,
                false, // is_create
                *edge_id,
                *version_id,
                *source,
                *target,
                *label,
                properties.clone(),
                *valid_from,
                commit_timestamp,
                historical,
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
pub(crate) fn apply_changes(tx: &WriteTransaction, commit_timestamp: Timestamp) -> Result<()> {
    // Create temporal interval for all operations in this transaction.
    let _temporal = BiTemporalInterval::current(commit_timestamp);

    // Acquire lock on historical storage once before processing all operations.
    let mut historical = tx.historical.write();

    // Pre-generate all tombstone version IDs at once to reduce lock contention
    let num_deletes = tx
        .buffer
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                crate::api::transaction::BufferedWrite::DeleteNode { .. }
                    | crate::api::transaction::BufferedWrite::DeleteEdge { .. }
            )
        })
        .count();

    let mut tombstone_ids = if num_deletes > 0 {
        let ids: Result<Vec<u64>> = (0..num_deletes)
            .map(|_| tx.version_id_gen.next().map_err(Into::into))
            .collect();
        ids?.into_iter()
    } else {
        Vec::new().into_iter()
    };

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

    // Rebuild adjacency indexes
    if tx.buffer.has_edge_operations() {
        tx.current.compact_adjacency();
    }

    Ok(())
}
