//! Write conflict detection for Snapshot Isolation.
//!
//! This module implements the "First-Committer-Wins" rule, ensuring that two
//! concurrent transactions cannot modify the same entity.
//!
//! # Snapshot Isolation
//!
//! In Snapshot Isolation, a transaction T1 must abort if it attempts to modify
//! an entity that was modified by another transaction T2 that committed after
//! T1 started.
//!
//! # Mechanism
//!
//! We iterate through all entities modified in the write buffer and check their
//! current version in storage. If the current version's `commit_timestamp` is
//! greater than our `snapshot_timestamp`, a conflict has occurred.

use super::WriteTransaction;
use crate::utils::error::{Result, TransactionError};

/// Detect write-write conflicts for Snapshot Isolation.
///
/// Checks if any entity modified by this transaction has been committed
/// by another transaction after our snapshot was taken. This implements
/// the First-Committer-Wins rule of Snapshot Isolation.
///
/// # Algorithm
///
/// For each operation in the write buffer:
/// 1.  Look up the entity in `CurrentStorage`.
/// 2.  If the entity exists, check its `commit_timestamp`.
/// 3.  If `commit_timestamp > snapshot_timestamp`, conflict!
/// 4.  If the entity doesn't exist but we are updating it, it implies deletion by another transaction. Conflict!
///
/// # Errors
///
/// Returns `TransactionError::SerializationFailure` if a write-write conflict is detected.
pub(crate) fn detect_conflicts(tx: &WriteTransaction) -> Result<()> {
    for write in tx.buffer.operations() {
        match write {
            // UpdateNode: check if node was modified or deleted after our snapshot
            crate::api::transaction::BufferedWrite::UpdateNode { node_id, .. } => {
                match tx.current.get_node(*node_id) {
                    Ok(current_node) => {
                        // Node exists - check if it was modified after our snapshot
                        if let Some(commit_ts) = current_node.metadata.commit_timestamp
                            && commit_ts > tx.snapshot.snapshot_timestamp
                        {
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", node_id),
                                reason: format!(
                                    "Version committed at {} after snapshot at {}",
                                    commit_ts, tx.snapshot.snapshot_timestamp
                                ),
                            }
                            .into());
                        }
                    }
                    Err(_) => {
                        // Node doesn't exist in current storage. Since we successfully
                        // called update_node() earlier (which reads from current storage),
                        // the node must have been deleted by another transaction.
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", node_id),
                            reason: "Node was deleted by another transaction".to_string(),
                        }
                        .into());
                    }
                }
            }

            // UpdateEdge: check if edge was modified or deleted after our snapshot
            crate::api::transaction::BufferedWrite::UpdateEdge { edge_id, .. } => {
                match tx.current.get_edge(*edge_id) {
                    Ok(current_edge) => {
                        // Edge exists - check if it was modified after our snapshot
                        if let Some(commit_ts) = current_edge.metadata.commit_timestamp
                            && commit_ts > tx.snapshot.snapshot_timestamp
                        {
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", edge_id),
                                reason: format!(
                                    "Version committed at {} after snapshot at {}",
                                    commit_ts, tx.snapshot.snapshot_timestamp
                                ),
                            }
                            .into());
                        }
                    }
                    Err(_) => {
                        // Edge doesn't exist - it was deleted by another transaction
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", edge_id),
                            reason: "Edge was deleted by another transaction".to_string(),
                        }
                        .into());
                    }
                }
            }

            // DeleteNode: check if node was modified after our snapshot
            crate::api::transaction::BufferedWrite::DeleteNode { node_id, .. } => {
                // Get current version from storage
                if let Ok(current_node) = tx.current.get_node(*node_id)
                    && let Some(commit_ts) = current_node.metadata.commit_timestamp
                    && commit_ts > tx.snapshot.snapshot_timestamp
                {
                    return Err(TransactionError::SerializationFailure {
                        entity: format!("{:?}", node_id),
                        reason: format!(
                            "Version committed at {} after snapshot at {}",
                            commit_ts, tx.snapshot.snapshot_timestamp
                        ),
                    }
                    .into());
                }
            }

            // DeleteEdge: check if edge was modified after our snapshot
            crate::api::transaction::BufferedWrite::DeleteEdge { edge_id, .. } => {
                // Get current version from storage
                if let Ok(current_edge) = tx.current.get_edge(*edge_id)
                    && let Some(commit_ts) = current_edge.metadata.commit_timestamp
                    && commit_ts > tx.snapshot.snapshot_timestamp
                {
                    return Err(TransactionError::SerializationFailure {
                        entity: format!("{:?}", edge_id),
                        reason: format!(
                            "Version committed at {} after snapshot at {}",
                            commit_ts, tx.snapshot.snapshot_timestamp
                        ),
                    }
                    .into());
                }
            }

            // CreateNode and CreateEdge don't need conflict detection
            // since they're creating new entities that didn't exist before
            _ => {}
        }
    }

    Ok(())
}
