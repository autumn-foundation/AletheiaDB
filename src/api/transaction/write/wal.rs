//! WAL Logging for Write Transactions
//!
//! This module handles the translation of high-level transaction operations
//! into the low-level Write-Ahead Log (WAL) format.
//!
//! It acts as a bridge between the transaction buffer (`BufferedWrite`) and
//! the durable storage layer (`WalOperation`).
//!
//! # Performance
//!
//! The translation is designed to be zero-allocation for string data where possible:
//! - `InternedString` IDs are copied directly (4 bytes).
//! - Property maps are cloned (which involves Arc increments for keys/values).
//! - No string data is copied or allocated during logging.

use super::WriteTransaction;
use crate::core::error::Result;
use crate::core::temporal::Timestamp;
use crate::storage::wal::WalOperation;

/// Log all buffered operations to WAL.
///
/// This ensures durability - operations are logged before being applied to the in-memory state.
/// Uses lock-free appends to the concurrent WAL system.
///
/// # Mapping
///
/// Maps `BufferedWrite` operations to `WalOperation` variants:
/// - `CreateNode` -> `WalOperation::CreateNode`
/// - `CreateEdge` -> `WalOperation::CreateEdge`
/// - `UpdateNode` -> `WalOperation::UpdateNode`
/// - `UpdateEdge` -> `WalOperation::UpdateEdge`
/// - `DeleteNode` -> `WalOperation::DeleteNode`
/// - `DeleteEdge` -> `WalOperation::DeleteEdge`
///
/// # Zero-Allocation Optimization
///
/// The `InternedString` types used in `BufferedWrite` store only a 32-bit integer ID.
/// When converting to `WalOperation`, we simply copy this integer. The actual string
/// content is stored in the global interner and is persisted separately (or recovered
/// via the interner snapshot).
///
/// # Example (Internal)
///
/// ```rust,ignore
/// // Inside a transaction commit:
/// log_operations_to_wal(&tx, commit_timestamp)?;
/// // If successful, operations are durable.
/// ```
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
