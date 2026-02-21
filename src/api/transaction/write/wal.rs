use super::WriteTransaction;
use crate::api::transaction::BufferedWrite;
use crate::core::error::Result;
use crate::core::temporal::Timestamp;
use crate::storage::wal::{DurabilityMode, WalOperation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalLogDisposition {
    /// Operations are buffered and caller must execute `wal.commit()`.
    NeedsCommit,
    /// Operation is already durably flushed by logging path.
    AlreadyDurable,
}

fn to_wal_operation(write: &BufferedWrite) -> WalOperation {
    match write {
        BufferedWrite::CreateNode {
            node_id,
            label,
            properties,
            valid_from,
            ..
        } => {
            // No allocation! Just copy the 4-byte InternedString ID
            WalOperation::CreateNode {
                node_id: *node_id,
                label: *label,
                properties: properties.clone(),
                valid_from: *valid_from,
            }
        }
        BufferedWrite::CreateEdge {
            edge_id,
            source,
            target,
            label,
            properties,
            valid_from,
            ..
        } => {
            // No allocation! Just copy the 4-byte InternedString ID
            WalOperation::CreateEdge {
                edge_id: *edge_id,
                source: *source,
                target: *target,
                label: *label,
                properties: properties.clone(),
                valid_from: *valid_from,
            }
        }
        BufferedWrite::UpdateNode {
            node_id,
            version_id,
            label,
            properties,
            valid_from,
            ..
        } => {
            // No allocation! Just copy the 4-byte InternedString ID
            WalOperation::UpdateNode {
                node_id: *node_id,
                version_id: *version_id,
                label: *label,
                properties: properties.clone(),
                valid_from: *valid_from,
            }
        }
        BufferedWrite::UpdateEdge {
            edge_id,
            version_id,
            label,
            properties,
            valid_from,
            ..
        } => {
            // No allocation! Just copy the 4-byte InternedString ID
            WalOperation::UpdateEdge {
                edge_id: *edge_id,
                version_id: *version_id,
                label: *label,
                properties: properties.clone(),
                valid_from: *valid_from,
            }
        }
        BufferedWrite::DeleteNode {
            node_id,
            valid_from,
        } => WalOperation::DeleteNode {
            node_id: *node_id,
            valid_from: *valid_from,
        },
        BufferedWrite::DeleteEdge {
            edge_id,
            valid_from,
        } => WalOperation::DeleteEdge {
            edge_id: *edge_id,
            valid_from: *valid_from,
        },
    }
}

/// Log all buffered operations to WAL.
///
/// This ensures durability - operations are logged before being applied.
/// Uses lock-free appends to the concurrent WAL system.
pub(crate) fn log_operations_to_wal(
    tx: &WriteTransaction,
    _commit_timestamp: Timestamp,
) -> Result<WalLogDisposition> {
    let writes = tx.buffer.operations();

    // Single-op synchronous transactions can bypass striped buffering.
    if matches!(tx.durability_mode, DurabilityMode::Synchronous) && writes.len() == 1 {
        tx.wal.append_sync_isolated(to_wal_operation(&writes[0]))?;
        return Ok(WalLogDisposition::AlreadyDurable);
    }

    for write in writes {
        let operation = to_wal_operation(write);
        // Append to WAL (lock-free!)
        tx.wal.append_async(operation)?;
    }

    Ok(WalLogDisposition::NeedsCommit)
}
