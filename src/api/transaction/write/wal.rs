use super::WriteTransaction;
use crate::core::temporal::Timestamp;
use crate::storage::wal::WalOperation;
use crate::core::error::Result;

/// Log all buffered operations to WAL.
///
/// This ensures durability - operations are logged before being applied.
/// Uses lock-free appends to the concurrent WAL system.
pub(crate) fn log_operations_to_wal(
    tx: &WriteTransaction,
    _commit_timestamp: Timestamp,
) -> Result<()> {
    for write in tx.buffer.operations() {
        let operation = match write {
            crate::api::transaction::BufferedWrite::CreateNode {
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
            crate::api::transaction::BufferedWrite::CreateEdge {
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
            crate::api::transaction::BufferedWrite::UpdateNode {
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
            crate::api::transaction::BufferedWrite::UpdateEdge {
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
            crate::api::transaction::BufferedWrite::DeleteNode {
                node_id,
                valid_from,
            } => WalOperation::DeleteNode {
                node_id: *node_id,
                valid_from: *valid_from,
            },
            crate::api::transaction::BufferedWrite::DeleteEdge {
                edge_id,
                valid_from,
            } => WalOperation::DeleteEdge {
                edge_id: *edge_id,
                valid_from: *valid_from,
            },
        };

        // Append to WAL (lock-free!)
        tx.wal.append_async(operation)?;
    }

    Ok(())
}
