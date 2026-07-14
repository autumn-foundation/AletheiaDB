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
use crate::core::id::VersionId;
use crate::core::temporal::Timestamp;
use crate::storage::wal::{LSN, WalOperation};

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
/// # Transaction framing (Issue #3413)
///
/// The buffered data ops are bracketed by a leading [`WalOperation::BeginTx`]
/// and a terminal [`WalOperation::CommitTx`] carrying the real
/// `commit_timestamp` and the data-op count, and the whole
/// `[BeginTx, ..data.., CommitTx]` sequence is appended in the SAME atomic
/// `append_batch_async` allocation (one LSN band, one flush epoch). This lets
/// crash recovery (a) discard a batch whose commit marker never became durable
/// — instead of replaying a torn prefix as a half-committed transaction — and
/// (b) re-stamp every recovered version of the transaction with the single
/// `commit_timestamp` instead of each entry's own wallclock (which previously
/// let `AS OF SYSTEM_TIME` bisect an atomic batch).
///
/// The markers ride entirely inside the existing `wal` critical section, so the
/// CLAUDE.md lock order (current_timestamp → wal → …) is unchanged.
///
/// # Arguments
///
/// * `tx` - The active write transaction containing the write buffer.
/// * `commit_timestamp` - The authoritative bi-temporal commit timestamp, carried
///   in the terminal `CommitTx` marker and re-stamped onto every replayed version.
///
/// # Returns
///
/// `Some(base_lsn)` — the lowest LSN allocated for this transaction (the
/// `BeginTx` marker's LSN) — for a non-empty transaction, or `None` for an
/// empty transaction (no ops appended). The base LSN is registered as the
/// commit's in-flight watermark by the caller (lost-write persist race fix).
///
/// # Errors
///
/// Returns an error if the WAL append fails (e.g., disk full, IO error).
pub(crate) fn log_operations_to_wal(
    tx: &WriteTransaction,
    commit_timestamp: Timestamp,
    closing_version_ids: &[VersionId],
) -> Result<Option<LSN>> {
    // Collect all buffered writes into a single batch and append them under one atomic
    // LSN allocation via the WAL `append_batch` path (Issue #219). This is strictly more
    // efficient than the previous per-operation `append_async` loop for multi-operation
    // transactions — e.g. each chunk committed by the bulk importer (Issue #3211) — and is
    // behavior-preserving: `append_batch_async` only buffers, leaving the subsequent
    // `wal.commit()` to perform the `DurabilityMode`-appropriate flush.
    let buffered = tx.buffer.operations();

    if buffered.is_empty() {
        // Empty transaction: no data ops, therefore no frame and no marker.
        // Reproduces the prior early-return exactly. No LSN allocated, so no
        // in-flight watermark to register.
        return Ok(None);
    }

    let tx_id = tx.tx_id.as_u64();

    // [BeginTx, ..data ops.., CommitTx]. Begin+Commit bracket the data ops so
    // replay buffers exactly this transaction's ops until the commit marker is
    // seen; raw (non-transactional) appends and legacy segments carry no such
    // markers and stay on the immediate-apply recovery path.
    let mut operations: Vec<WalOperation> = Vec::with_capacity(buffered.len() + 2);
    operations.push(WalOperation::BeginTx { tx_id });

    // Issue #3406: stamp the pre-generated closing (tombstone / retraction)
    // version id onto each delete/retract op, in buffer order, so the WAL
    // records the exact id historical storage will use. `closing_version_ids`
    // was generated with one id per delete/retract op in this same order.
    let mut closing_ids = closing_version_ids.iter().copied();
    for bw in buffered.iter() {
        let mut op = WalOperation::from(bw);
        match &mut op {
            WalOperation::DeleteNode { version_id, .. }
            | WalOperation::DeleteEdge { version_id, .. }
            | WalOperation::RetractNode { version_id, .. }
            | WalOperation::RetractEdge { version_id, .. } => {
                *version_id = closing_ids.next();
            }
            _ => {}
        }
        operations.push(op);
    }

    operations.push(WalOperation::CommitTx {
        tx_id,
        entry_count: buffered.len() as u32,
        commit_timestamp,
    });

    // `append_batch` allocates a single contiguous LSN band and returns the
    // allocated LSNs in operation order, so element 0 (the `BeginTx` marker) is
    // the base — the lowest LSN for this whole transaction.
    let lsns = tx.wal.append_batch_async(operations)?;

    Ok(lsns.first().copied())
}
