//! Write-Ahead Log (WAL) integration for transactions.
//!
//! This module bridges the high-level `WriteTransaction` system with the low-level
//! `ConcurrentWalSystem`. It ensures durability by logging all buffered operations
//! to the WAL before they are applied to the in-memory storage.
//!
//! # Architecture
//!
//! The transaction commit process follows a "Log-Before-Apply" protocol:
//! 1.  Validate buffered writes.
//! 2.  **Log operations to WAL** (this module).
//! 3.  Wait for durability (fsync), depending on `DurabilityMode`.
//! 4.  Apply changes to `CurrentStorage`.
//!
//! This ensures that if the system crashes after step 2/3 but before step 4,
//! the changes can be recovered from the WAL on restart.
//!
//! # Performance
//!
//! The conversion from `BufferedWrite` (transaction buffer) to `WalOperation` (WAL entry)
//! is designed to be zero-allocation for string data. It leverages `InternedString`
//! IDs (4-byte integers) instead of copying full strings, making the logging process
//! extremely fast.

use super::WriteTransaction;
use crate::core::error::Result;
use crate::core::temporal::Timestamp;
use crate::storage::wal::WalOperation;

/// Log all buffered operations to the Write-Ahead Log (WAL).
///
/// This function translates the transaction's buffered writes into WAL operations
/// and appends them to the concurrent WAL system.
///
/// # Durability & Recovery
///
/// This is the critical step for ACID durability. Once this function returns and
/// the WAL flush completes, the transaction is considered durable. If the system
/// crashes, recovery logic will replay these operations to restore the state.
///
/// # Zero-Copy Optimization
///
/// This function performs a "shallow copy" of the data. Instead of serializing
/// full string labels and keys, it copies their 4-byte `InternedString` IDs.
/// This minimizes memory allocation and bus traffic during the critical commit path.
///
/// # Arguments
///
/// * `tx` - The active write transaction containing the write buffer.
/// * `_commit_timestamp` - The timestamp assigned to this commit (currently unused in WAL op, but required by API).
///
/// # Errors
///
/// Returns an error if the WAL append fails (e.g., disk full, IO error).
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
