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
use crate::core::error::{Result, TransactionError};
use crate::core::hlc::HybridTimestamp;

fn check_commit_timestamp<F>(
    entity_fn: F,
    commit_ts: Option<HybridTimestamp>,
    snapshot_ts: HybridTimestamp,
) -> Result<()>
where
    F: FnOnce() -> String,
{
    if let Some(commit_ts) = commit_ts
        && commit_ts > snapshot_ts
    {
        return Err(TransactionError::SerializationFailure {
            entity: entity_fn(),
            reason: format!(
                "Version committed at {} after snapshot at {}",
                commit_ts, snapshot_ts
            ),
        }
        .into());
    }
    Ok(())
}

fn check_node_conflict(
    tx: &WriteTransaction,
    node_id: crate::core::id::NodeId,
    is_update: bool,
) -> Result<()> {
    match tx.current.get_node(node_id) {
        Ok(current_node) => check_commit_timestamp(
            || format!("{:?}", node_id),
            current_node.metadata.commit_timestamp,
            tx.snapshot.snapshot_timestamp,
        ),
        Err(_) if is_update => {
            // Node doesn't exist in current storage. Since we successfully
            // called update_node() earlier (which reads from current storage),
            // the node must have been deleted by another transaction.
            Err(TransactionError::SerializationFailure {
                entity: format!("{:?}", node_id),
                reason: "Node was deleted by another transaction".to_string(),
            }
            .into())
        }
        Err(_) => Ok(()),
    }
}

fn check_edge_conflict(
    tx: &WriteTransaction,
    edge_id: crate::core::id::EdgeId,
    is_update: bool,
) -> Result<()> {
    match tx.current.get_edge(edge_id) {
        Ok(current_edge) => check_commit_timestamp(
            || format!("{:?}", edge_id),
            current_edge.metadata.commit_timestamp,
            tx.snapshot.snapshot_timestamp,
        ),
        Err(_) if is_update => {
            // Edge doesn't exist - it was deleted by another transaction
            Err(TransactionError::SerializationFailure {
                entity: format!("{:?}", edge_id),
                reason: "Edge was deleted by another transaction".to_string(),
            }
            .into())
        }
        Err(_) => Ok(()),
    }
}

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
            crate::api::transaction::BufferedWrite::UpdateNode { node_id, .. } => {
                check_node_conflict(tx, *node_id, true)?;
            }
            crate::api::transaction::BufferedWrite::UpdateEdge { edge_id, .. } => {
                check_edge_conflict(tx, *edge_id, true)?;
            }
            crate::api::transaction::BufferedWrite::DeleteNode { node_id, .. } => {
                check_node_conflict(tx, *node_id, false)?;
            }
            crate::api::transaction::BufferedWrite::DeleteEdge { edge_id, .. } => {
                check_edge_conflict(tx, *edge_id, false)?;
            }
            _ => {}
        }
    }

    Ok(())
}
