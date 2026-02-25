//! WAL recovery logic.
//!
//! This module provides shared logic for replaying WAL entries into storage.
//! It is used by both the CheckpointManager (for recovering from checkpoints)
//! and AletheiaDB startup (for recovering from persisted indexes).

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
///
/// # Returns
///
/// The final LSN reached after replay (current LSN of the WAL).
pub(crate) fn replay_wal_into_storage(
    wal: &ConcurrentWalSystem,
    current: &CurrentStorage,
    historical: &mut HistoricalStorage,
    start_lsn: LSN,
) -> Result<LSN> {
    const RECOVERY_TX_ID: u64 = 0;

    let mut max_node_id: Option<u64> = None;
    let mut max_edge_id: Option<u64> = None;
    let mut max_version_id: Option<u64> = None;
    let mut next_version_id: u64 = 1;

    let wal_entries = wal.read_from(start_lsn)?;

    if !wal_entries.is_empty() {
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

                max_version_id = Some(match max_version_id {
                    Some(current_max) => current_max.max(next_version_id - 1),
                    None => next_version_id - 1,
                });
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

                max_version_id = Some(match max_version_id {
                    Some(current_max) => current_max.max(next_version_id - 1),
                    None => next_version_id - 1,
                });
            }
            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties,
                valid_from,
            } => {
                max_version_id = Some(match max_version_id {
                    Some(current_max) => current_max.max(version_id.as_u64()),
                    None => version_id.as_u64(),
                });
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
                max_version_id = Some(match max_version_id {
                    Some(current_max) => current_max.max(version_id.as_u64()),
                    None => version_id.as_u64(),
                });
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
                    max_version_id = Some(match max_version_id {
                        Some(current_max) => current_max.max(next_version_id - 1),
                        None => next_version_id - 1,
                    });
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
                    max_version_id = Some(match max_version_id {
                        Some(current_max) => current_max.max(next_version_id - 1),
                        None => next_version_id - 1,
                    });
                }
            }
            WalOperation::Checkpoint { .. } => {
                // Checkpoint markers are informational only during replay
            }
        }
    }

    let final_lsn = wal.current_lsn();

    // Only update ID generators when replay observed IDs for that type.
    // This preserves values from load_current_storage when no WAL replay happened.
    if let Some(max_node_id) = max_node_id {
        current.init_node_id_generator(max_node_id + 1);
    }
    if let Some(max_edge_id) = max_edge_id {
        current.init_edge_id_generator(max_edge_id + 1);
    }
    if let Some(max_version_id) = max_version_id {
        current.init_version_id_generator(max_version_id + 1);
    }

    Ok(final_lsn)
}
