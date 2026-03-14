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
    mut next_version_id: u64,
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
            } => {
                max_node_id = Some(match max_node_id {
                    Some(current_max) => current_max.max(node_id.as_u64()),
                    None => node_id.as_u64(),
                });

                // Label is already an InternedString (no allocation needed!)
                let interned_label = label;

                // Transaction time comes from when the WAL entry was logged
                let commit_timestamp = entry.timestamp;
                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                let version_id = VersionId::new(next_version_id)?;
                next_version_id += 1;

                let node = Node::with_metadata(
                    node_id,
                    interned_label,
                    properties.clone(),
                    version_id,
                    metadata,
                );

                current.insert_node_direct(node, commit_timestamp)?;
                historical.add_node_version(
                    node_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    properties,
                    false, // not a tombstone
                )?;
            }
            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                properties,
                valid_from,
            } => {
                max_edge_id = Some(match max_edge_id {
                    Some(current_max) => current_max.max(edge_id.as_u64()),
                    None => edge_id.as_u64(),
                });

                // Label is already an InternedString (no allocation needed!)
                let interned_label = label;

                let commit_timestamp = entry.timestamp;
                let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                let version_id = VersionId::new(next_version_id)?;
                next_version_id += 1;

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
                historical.add_edge_version(
                    edge_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    source,
                    target,
                    properties,
                    false, // not a tombstone
                )?;
            }
            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties,
                valid_from,
            } => {
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                // Label is already an InternedString (no allocation needed!)
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

                current.update_node_direct(node, commit_timestamp)?;

                if let Some(prev_version_id) = historical.get_current_node_version(node_id) {
                    historical
                        .close_node_version_transaction_time(prev_version_id, commit_timestamp)?;
                }

                historical.add_node_version(
                    node_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    properties,
                    false, // not a tombstone
                )?;
            }
            WalOperation::UpdateEdge {
                edge_id,
                version_id,
                label,
                properties,
                valid_from,
            } => {
                next_version_id = next_version_id.max(version_id.as_u64() + 1);

                let current_edge = current.get_edge(edge_id)?;

                // Label is already an InternedString (no allocation needed!)
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

                if let Some(prev_version_id) = historical.get_current_edge_version(edge_id) {
                    historical
                        .close_edge_version_transaction_time(prev_version_id, commit_timestamp)?;
                }

                historical.add_edge_version(
                    edge_id,
                    version_id,
                    valid_from,
                    commit_timestamp,
                    interned_label,
                    current_edge.source,
                    current_edge.target,
                    properties,
                    false, // not a tombstone
                )?;
            }
            WalOperation::DeleteNode {
                node_id,
                valid_from: _,
            } => {
                // If the node doesn't exist in current storage, it might have been deleted already
                // or never existed (if we're replaying a delete for something we missed creation of?).
                // But for linear WAL replay, creation should have happened before.
                // However, `get_node` might fail.
                if let Ok(node) = current.get_node(node_id) {
                    let commit_timestamp = entry.timestamp;

                    if let Some(current_version_id) = historical.get_current_node_version(node_id) {
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
                        node.label,
                        node.properties.clone(),
                        true, // is_tombstone
                    )?;

                    current.delete_node_direct(node_id, commit_timestamp)?;
                }
            }
            WalOperation::DeleteEdge {
                edge_id,
                valid_from: _,
            } => {
                if let Ok(edge) = current.get_edge(edge_id) {
                    let commit_timestamp = entry.timestamp;

                    if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
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
                        edge.label,
                        edge.source,
                        edge.target,
                        edge.properties.clone(),
                        true, // is_tombstone
                    )?;

                    current.delete_edge_direct(edge_id)?;
                }
            }
            WalOperation::Checkpoint { .. } => {
                // Checkpoint markers are informational only during replay
            }
        }
    }

    let final_lsn = wal.current_lsn();

    Ok((final_lsn, max_node_id, max_edge_id, next_version_id))
}
