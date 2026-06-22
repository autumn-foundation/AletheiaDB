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
    // Collect all buffered writes into a single batch and append them under one atomic
    // LSN allocation via the WAL `append_batch` path (Issue #219). This is strictly more
    // efficient than the previous per-operation `append_async` loop for multi-operation
    // transactions — e.g. each chunk committed by the bulk importer (Issue #3211) — and is
    // behavior-preserving: `append_batch_async` only buffers, leaving the subsequent
    // `wal.commit()` to perform the `DurabilityMode`-appropriate flush.
    let operations: Vec<WalOperation> = tx
        .buffer
        .operations()
        .iter()
        .map(WalOperation::from)
        .collect();

    if operations.is_empty() {
        return Ok(());
    }

    tx.wal.append_batch_async(operations)?;

    Ok(())
}
