//! Transaction visibility management for Snapshot Isolation.
//!
//! This module implements the visibility rules for Snapshot Isolation (SI),
//! which ensures that each transaction sees a consistent snapshot of the database
//! from the time it started.

use crate::api::transaction::types::TxId;
use crate::core::temporal::Timestamp;
use crate::utils::lock::{MutexExt, RwLockExt};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// Snapshot of transaction visibility at a point in time.
///
/// A snapshot captures the set of transactions that were active when the
/// snapshot was taken. This is used to determine which versions are visible
/// to a transaction using Snapshot Isolation.
///
/// # Performance (Issue #221)
///
/// Uses `Arc<HashSet<TxId>>` instead of `HashSet<TxId>` to avoid O(N²) cloning
/// overhead when capturing snapshots. With N concurrent transactions, cloning
/// a HashSet containing N elements on every transaction creation was creating
/// quadratic scaling. Arc allows cheap snapshot captures (just Arc clone).
#[derive(Debug, Clone)]
pub struct TransactionSnapshot {
    /// Timestamp when snapshot was taken
    pub snapshot_timestamp: Timestamp,

    /// Transactions that were active when snapshot was taken
    /// (not yet committed or aborted).
    ///
    /// Wrapped in Arc to enable efficient snapshot capture without full HashSet cloning.
    pub active_transactions: Arc<HashSet<TxId>>,
}

impl TransactionSnapshot {
    /// Check if a version is visible in this snapshot.
    ///
    /// A version is visible if:
    /// 1. It was committed before the snapshot timestamp, AND
    /// 2. It was not created by a transaction that was active at snapshot time
    ///
    /// # Arguments
    /// * `created_by_tx` - The transaction that created this version
    /// * `commit_timestamp` - When the version was committed (None if uncommitted)
    ///
    /// # Returns
    /// `true` if the version is visible in this snapshot, `false` otherwise
    pub fn is_visible(&self, created_by_tx: TxId, commit_timestamp: Option<Timestamp>) -> bool {
        match commit_timestamp {
            None => false, // Uncommitted version - not visible
            Some(ts) => {
                // Visible if:
                // 1. Committed strictly before our snapshot (not at the same time) AND
                // 2. Not created by a transaction that was active at snapshot time
                ts < self.snapshot_timestamp && !self.active_transactions.contains(&created_by_tx)
            }
        }
    }
}

/// Epoch-based compression for transaction commit log (Issue #237).
///
/// Represents a range of sequential transactions with linear timestamp progression.
/// Instead of storing each transaction individually, we store:
/// - `start_tx`: First transaction ID in the range
/// - `end_tx`: Last transaction ID in the range (inclusive)
/// - `start_ts`: Commit timestamp of first transaction
/// - `end_ts`: Commit timestamp of last transaction
///
/// For any transaction `tx` where `start_tx <= tx <= end_tx`, we can compute
/// its commit timestamp using linear interpolation:
/// `commit_ts = start_ts + (tx - start_tx)`
///
/// # Timestamp Interpolation Invariant
///
/// This structure assumes that:
/// 1. Transaction IDs increment by 1 for each transaction
/// 2. Timestamp wallclock values increment by 1 for each transaction
/// 3. Logical components of timestamps are 0 (or ignored)
///
/// This invariant is verified during compression (line ~279) where we check:
/// `next_ts.wallclock() == expected_ts_wallclock`
///
/// # Memory Savings
///
/// A single EpochRange (32 bytes) can represent thousands of transactions,
/// achieving 100-1000x compression for sequential workloads.
///
/// # Safety
///
/// Callers must ensure `start_tx <= end_tx` and `start_ts <= end_ts`.
/// These invariants are checked via debug_assert in `new()`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EpochRange {
    start_tx: TxId,
    end_tx: TxId,
    start_ts: Timestamp,
    end_ts: Timestamp,
}

impl EpochRange {
    /// Create a new epoch range.
    ///
    /// # Arguments
    /// * `start_tx` - First transaction ID in the range
    /// * `end_tx` - Last transaction ID in the range (inclusive)
    /// * `start_ts` - Commit timestamp of first transaction
    /// * `end_ts` - Commit timestamp of last transaction
    fn new(start_tx: TxId, end_tx: TxId, start_ts: Timestamp, end_ts: Timestamp) -> Self {
        debug_assert!(start_tx <= end_tx, "start_tx must be <= end_tx");
        debug_assert!(start_ts <= end_ts, "start_ts must be <= end_ts");
        EpochRange {
            start_tx,
            end_tx,
            start_ts,
            end_ts,
        }
    }

    /// Check if a transaction ID is within this epoch range.
    fn contains(&self, tx_id: TxId) -> bool {
        self.start_tx <= tx_id && tx_id <= self.end_tx
    }

    /// Get the commit timestamp for a transaction in this epoch.
    ///
    /// Returns `None` if the transaction is not in this epoch.
    /// Uses linear interpolation to compute the timestamp.
    fn get_commit_timestamp(&self, tx_id: TxId) -> Option<Timestamp> {
        if !self.contains(tx_id) {
            return None;
        }

        // Linear interpolation: ts = start_ts + (tx - start_tx)
        let offset = tx_id.as_u64() - self.start_tx.as_u64();

        // Ensure offset fits within i64 to prevent overflow during addition with wallclock.
        // If an epoch spans more than i64::MAX transactions, this indicates an invalid state.
        if offset > i64::MAX as u64 {
            return None;
        }

        let ts_value = self.start_ts.wallclock() + offset as i64;
        Some(Timestamp::from(ts_value))
    }

    /// Get the number of transactions represented by this epoch.
    fn transaction_count(&self) -> u64 {
        debug_assert!(self.start_tx <= self.end_tx, "start_tx must be <= end_tx");
        // Use saturating_sub to prevent underflow in case of invalid data
        self.end_tx.as_u64().saturating_sub(self.start_tx.as_u64()) + 1
    }

    /// Estimate memory usage in bytes.
    #[cfg(test)]
    #[allow(dead_code)]
    fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

/// Statistics about commit log compression.
#[derive(Debug, Clone)]
pub struct CompressionStats {
    /// Total number of transactions tracked
    pub total_transactions: usize,
    /// Number of epoch ranges
    pub epoch_count: usize,
    /// Number of exceptions (non-sequential transactions)
    pub exception_count: usize,
    /// Compression ratio (transactions per storage entry)
    pub compression_ratio: f64,
    /// Estimated memory usage in bytes
    pub memory_usage_bytes: usize,
    /// Estimated memory saved compared to uncompressed
    pub memory_saved_bytes: usize,
}

/// Compressed commit log using epoch-based compression (Issue #237).
///
/// Hybrid structure that combines:
/// - **Epochs**: Vec of EpochRange for sequential transaction runs
/// - **Exceptions**: BTreeMap for non-sequential transactions (gaps, outliers)
///
/// # Compression Strategy
///
/// Sequential transactions with linear timestamp progression are compressed
/// into epochs. Non-sequential transactions (gaps from aborts, timestamp outliers)
/// are stored as exceptions.
///
/// # Performance
///
/// - **Lookups**: O(log E + log X) where E = epochs, X = exceptions
/// - **Compression**: 10-100x for typical workloads with sequential patterns
/// - **Memory**: ~32 bytes per epoch, ~24 bytes per exception
#[derive(Debug, Clone)]
struct CompressedCommitLog {
    /// Sorted vector of epoch ranges (non-overlapping)
    epochs: Vec<EpochRange>,
    /// Exception transactions that don't fit into epochs
    exceptions: BTreeMap<TxId, Timestamp>,
}

impl CompressedCommitLog {
    /// Create a new empty compressed commit log.
    fn new() -> Self {
        CompressedCommitLog {
            epochs: Vec::new(),
            exceptions: BTreeMap::new(),
        }
    }

    /// Insert a transaction commit record.
    ///
    /// Initially added to exceptions. Call `compress()` periodically to
    /// convert sequential runs into epochs.
    fn insert(&mut self, tx_id: TxId, commit_ts: Timestamp) {
        self.exceptions.insert(tx_id, commit_ts);
    }

    /// Get the commit timestamp for a transaction.
    ///
    /// Checks both epochs and exceptions.
    fn get(&self, tx_id: TxId) -> Option<Timestamp> {
        // First check exceptions (likely more recent)
        if let Some(&ts) = self.exceptions.get(&tx_id) {
            return Some(ts);
        }

        // Then check epochs using binary search
        // Find the epoch that might contain this tx_id
        let idx = self.epochs.binary_search_by(|epoch| {
            if tx_id < epoch.start_tx {
                std::cmp::Ordering::Greater
            } else if tx_id > epoch.end_tx {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        });

        match idx {
            Ok(i) => self.epochs[i].get_commit_timestamp(tx_id),
            Err(_) => None,
        }
    }

    /// Compress sequential runs in exceptions into epochs.
    ///
    /// This analyzes the exceptions map to find sequential transaction ranges
    /// with linear timestamp progression and converts them to EpochRange entries.
    ///
    /// # Algorithm (Incremental, Issue #237)
    ///
    /// 1. Only process new exceptions (avoid decompressing existing epochs)
    /// 2. Identify sequential runs in new exceptions
    /// 3. Convert runs of 3+ transactions into new epochs
    /// 4. Merge new epochs with existing epochs (O(E) where E = epoch count)
    /// 5. Keep non-sequential transactions as exceptions
    ///
    /// # Performance
    ///
    /// - Time: O(X log X + E) where X = exceptions, E = epochs
    /// - Memory: O(X + E) - avoids O(N) decompression where N = total transactions
    fn compress(&mut self) {
        if self.exceptions.is_empty() {
            return;
        }

        // Don't compress very small batches (overhead not worth it)
        const MIN_COMPRESS_THRESHOLD: usize = 10;
        if self.exceptions.len() < MIN_COMPRESS_THRESHOLD {
            return;
        }

        // Extract exceptions into sorted vector (only the new ones, not all transactions)
        let entries: Vec<(TxId, Timestamp)> = self
            .exceptions
            .iter()
            .map(|(&tx_id, &ts)| (tx_id, ts))
            .collect();

        // Already sorted by TxId (BTreeMap maintains order)
        // No need to sort again

        let mut new_epochs = Vec::new();
        let mut new_exceptions = BTreeMap::new();

        let mut i = 0;
        while i < entries.len() {
            let (start_tx, start_ts) = entries[i];
            let mut end_tx = start_tx;
            let mut end_ts = start_ts;
            let mut run_length = 1;

            // Extend run while sequential
            while i + run_length < entries.len() {
                let (next_tx, next_ts) = entries[i + run_length];
                let expected_tx = TxId::new(end_tx.as_u64() + 1);
                let expected_ts_wallclock = end_ts.wallclock() + 1;

                // Check if both tx_id and timestamp increment by 1
                // Note: We compare wallclock values since logical component should be 0
                if next_tx == expected_tx && next_ts.wallclock() == expected_ts_wallclock {
                    end_tx = next_tx;
                    end_ts = next_ts;
                    run_length += 1;
                } else {
                    break;
                }
            }

            // Convert runs of 3+ into epochs, keep shorter runs as exceptions
            const MIN_EPOCH_SIZE: usize = 3;
            if run_length >= MIN_EPOCH_SIZE {
                new_epochs.push(EpochRange::new(start_tx, end_tx, start_ts, end_ts));
            } else {
                // Add all transactions in this short run to exceptions
                for j in 0..run_length {
                    let (tx_id, ts) = entries[i + j];
                    new_exceptions.insert(tx_id, ts);
                }
            }

            i += run_length;
        }

        // Merge new epochs with existing epochs (O(E1 + E2) where E1, E2 are epoch counts)
        // This avoids O(N) decompression of all existing epochs
        self.epochs = Self::merge_epochs(std::mem::take(&mut self.epochs), new_epochs);
        self.exceptions = new_exceptions;
    }

    /// Merge two sorted vectors of epochs, attempting to combine adjacent epochs.
    ///
    /// # Performance
    ///
    /// O(E1 + E2) where E1, E2 are the sizes of the input epoch vectors.
    /// Much more efficient than decompressing all epochs (which would be O(N) where N = total transactions).
    ///
    /// # Algorithm
    ///
    /// 1. Iterate through both epoch lists in sorted order
    /// 2. Check if adjacent epochs can be merged (sequential and continuous)
    /// 3. Combine mergeable epochs, keep others separate
    fn merge_epochs(mut epochs1: Vec<EpochRange>, mut epochs2: Vec<EpochRange>) -> Vec<EpochRange> {
        // Combine and sort all epochs
        epochs1.append(&mut epochs2);
        epochs1.sort_by_key(|e| e.start_tx);

        if epochs1.is_empty() {
            return epochs1;
        }

        // Merge adjacent epochs if they're sequential
        let mut merged = Vec::with_capacity(epochs1.len());
        let mut current = epochs1[0].clone();

        for next_epoch in epochs1.into_iter().skip(1) {
            // Check if current and next are adjacent and can be merged
            // They can be merged if:
            // 1. end_tx + 1 == next.start_tx (sequential transaction IDs)
            // 2. end_ts + 1 == next.start_ts (sequential timestamps)
            let can_merge = current.end_tx.as_u64() + 1 == next_epoch.start_tx.as_u64()
                && current.end_ts.wallclock() + 1 == next_epoch.start_ts.wallclock();

            if can_merge {
                // Extend current epoch to include next epoch
                current.end_tx = next_epoch.end_tx;
                current.end_ts = next_epoch.end_ts;
            } else {
                // Can't merge, push current and start new one
                merged.push(current);
                current = next_epoch;
            }
        }

        // Don't forget the last epoch
        merged.push(current);
        merged
    }

    /// Get the number of epoch ranges.
    #[cfg(test)]
    fn epoch_count(&self) -> usize {
        self.epochs.len()
    }

    /// Get the number of exception entries.
    #[cfg(test)]
    fn exception_count(&self) -> usize {
        self.exceptions.len()
    }

    /// Get total number of transactions represented.
    fn total_transaction_count(&self) -> usize {
        let epoch_txs: u64 = self.epochs.iter().map(|e| e.transaction_count()).sum();
        epoch_txs as usize + self.exceptions.len()
    }

    /// Estimate memory usage in bytes.
    fn memory_usage(&self) -> usize {
        let epoch_size = self.epochs.len() * std::mem::size_of::<EpochRange>();
        // BTreeMap has significant overhead: internal tree nodes, alignment, etc.
        // Realistic estimate is 40-48 bytes per entry (not just key+value size)
        let exception_size = self.exceptions.len() * 40; // More realistic BTreeMap estimate
        let vec_overhead = std::mem::size_of::<Vec<EpochRange>>();
        let btree_overhead = std::mem::size_of::<BTreeMap<TxId, Timestamp>>();

        epoch_size + exception_size + vec_overhead + btree_overhead
    }

    /// Get compression statistics.
    fn get_stats(&self) -> CompressionStats {
        let total_txs = self.total_transaction_count();
        let memory = self.memory_usage();
        // More realistic uncompressed estimate: BTreeMap with 40 bytes per entry
        let uncompressed_memory = total_txs * 40;

        CompressionStats {
            total_transactions: total_txs,
            epoch_count: self.epochs.len(),
            exception_count: self.exceptions.len(),
            compression_ratio: if self.epochs.len() + self.exceptions.len() > 0 {
                total_txs as f64 / (self.epochs.len() + self.exceptions.len()) as f64
            } else {
                0.0
            },
            memory_usage_bytes: memory,
            memory_saved_bytes: uncompressed_memory.saturating_sub(memory),
        }
    }
}

/// Manages transaction visibility for Snapshot Isolation.
///
/// The visibility manager tracks which transactions are currently active
/// and which have committed, allowing transactions to capture a consistent
/// snapshot of the database state.
///
/// # Snapshot Isolation Semantics
///
/// - Each transaction sees a consistent snapshot from its start time
/// - Transactions don't see uncommitted changes from other transactions
/// - Transactions don't see changes committed after their snapshot time
/// - Write-write conflicts are detected at commit time
///
/// # Lock Recovery Safety
///
/// This struct uses `lock_or_recover()`, `read_or_recover()`, and `write_or_recover()`
/// for all lock acquisitions. This is safe because the protected data structures
/// (`HashSet<TxId>` and `BTreeMap<TxId, Timestamp>`) have no complex invariants that
/// could be violated by a mid-operation panic:
///
/// - **Worst case**: A transaction ID may be missing from the active/committed sets
/// - **Behavior**: This fails safe by being conservative (treating as uncommitted/not visible)
/// - **No corruption**: Standard library collections remain valid after partial operations
///
/// # Concurrency
///
/// The `committed` map uses an `RwLock` instead of `Mutex` to allow concurrent readers.
/// This eliminates read contention in `is_visible()`, which is called during every
/// read operation (get_node, get_edge). Multiple transactions can check visibility
/// simultaneously, while commits still acquire exclusive write access.
///
/// # Performance Optimization (Issue #221)
///
/// The `active` set uses Arc-wrapping with copy-on-write semantics to avoid O(N²)
/// cloning overhead in `capture_snapshot`. Previously, with N concurrent transactions,
/// each new transaction would clone a HashSet of N elements. Now we only clone the
/// Arc (cheap pointer copy) and use copy-on-write for mutations.
///
/// # Epoch-Based Compression (Issue #237)
///
/// The `committed` field now uses `CompressedCommitLog` which applies epoch-based
/// compression to reduce memory usage from ~24 bytes per transaction to ~0.24 bytes
/// per transaction (100x compression) for sequential workloads.
pub struct TxVisibilityManager {
    /// Currently active transactions, wrapped in Arc for efficient snapshot capture.
    ///
    /// Uses copy-on-write: mutations create a new HashSet, update it, and replace the Arc.
    /// Snapshots just clone the Arc (cheap), avoiding full HashSet clones.
    active: Mutex<Arc<HashSet<TxId>>>,

    /// Committed transactions with epoch-based compression (Issue #237).
    ///
    /// Uses `CompressedCommitLog` to reduce memory usage from ~40 bytes per transaction
    /// to ~0.04-0.4 bytes per transaction (100-1000x compression) for sequential workloads.
    ///
    /// **Memory estimates with compression:**
    /// - 1M transactions: ~40KB-400KB (was 40MB, 100-1000x improvement)
    /// - 10M transactions: ~400KB-4MB (was 400MB, 100-1000x improvement)
    /// - 100M transactions: ~4MB-40MB (was 4GB, 100-1000x improvement)
    ///
    /// Note: BTreeMap overhead is ~40 bytes per entry (not just key+value).
    committed: RwLock<CompressedCommitLog>,
}

impl TxVisibilityManager {
    /// Create a new visibility manager.
    pub fn new() -> Self {
        TxVisibilityManager {
            active: Mutex::new(Arc::new(HashSet::new())),
            committed: RwLock::new(CompressedCommitLog::new()),
        }
    }

    /// Register a new active transaction.
    ///
    /// This should be called when a transaction begins.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the transaction being started
    ///
    /// # Implementation (Copy-on-Write)
    ///
    /// Uses copy-on-write to modify the active set: clones the HashSet, modifies it,
    /// and replaces the Arc. This ensures that existing snapshots remain valid while
    /// allowing efficient snapshot capture (just Arc clone, not HashSet clone).
    ///
    /// # Note
    ///
    /// Uses lock recovery to prevent cascade panics if the lock was poisoned
    /// by a panicking thread. The transaction set can safely be used after recovery.
    pub fn register_active(&self, tx_id: TxId) {
        let mut active_guard = self.active.lock_or_recover();

        // Use Arc::make_mut for idiomatic copy-on-write.
        // This avoids a clone if the Arc is not shared (only one strong reference).
        Arc::make_mut(&mut *active_guard).insert(tx_id);
    }

    /// Capture a snapshot for a transaction.
    ///
    /// The snapshot records which transactions are currently active.
    /// This snapshot will be used to determine visibility of versions
    /// throughout the transaction's lifetime.
    ///
    /// # Arguments
    /// * `snapshot_timestamp` - The timestamp for this snapshot
    ///
    /// # Returns
    /// A `TransactionSnapshot` capturing the current visibility state
    ///
    /// # Performance (Issue #221 Fix)
    ///
    /// Previously cloned the entire HashSet of active transactions, creating O(N²)
    /// overhead with N concurrent transactions. Now clones only the Arc (cheap pointer
    /// copy), reducing snapshot capture from O(N) to O(1).
    pub fn capture_snapshot(&self, snapshot_timestamp: Timestamp) -> TransactionSnapshot {
        let active_guard = self.active.lock_or_recover();

        TransactionSnapshot {
            snapshot_timestamp,
            active_transactions: Arc::clone(&active_guard),
        }
    }

    /// Register a transaction commit.
    ///
    /// This should be called when a transaction successfully commits.
    /// It removes the transaction from the active set and records its
    /// commit timestamp.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the committing transaction
    /// * `commit_timestamp` - When the transaction committed
    ///
    /// # Implementation (Copy-on-Write)
    ///
    /// Uses copy-on-write to remove from active set: clones HashSet, removes tx_id,
    /// wraps in new Arc. This ensures existing snapshots remain valid.
    ///
    /// # Compression (Issue #237)
    ///
    /// Initially adds to exceptions. Call `compress_commit_log()` periodically
    /// to compress sequential runs into epochs.
    pub fn register_commit(&self, tx_id: TxId, commit_timestamp: Timestamp) {
        // Drop active lock before acquiring committed write lock to reduce contention
        {
            let mut active_guard = self.active.lock_or_recover();

            // Use Arc::make_mut for idiomatic copy-on-write.
            Arc::make_mut(&mut *active_guard).remove(&tx_id);
        } // active lock released here

        let mut committed = self.committed.write_or_recover();
        committed.insert(tx_id, commit_timestamp);
    }

    /// Register a transaction abort.
    ///
    /// This should be called when a transaction aborts (rolls back).
    /// It removes the transaction from the active set.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the aborting transaction
    ///
    /// # Implementation (Copy-on-Write)
    ///
    /// Uses copy-on-write to remove from active set: clones HashSet, removes tx_id,
    /// wraps in new Arc. This ensures existing snapshots remain valid.
    pub fn register_abort(&self, tx_id: TxId) {
        let mut active_guard = self.active.lock_or_recover();

        // Use Arc::make_mut for idiomatic copy-on-write.
        Arc::make_mut(&mut *active_guard).remove(&tx_id);
    }

    /// Check if a version is visible in a snapshot.
    ///
    /// This applies the Snapshot Isolation visibility rules to determine
    /// if a version created by `created_by_tx` is visible in the given snapshot.
    ///
    /// # Arguments
    /// * `snapshot` - The transaction's snapshot
    /// * `created_by_tx` - The transaction that created the version
    ///
    /// # Returns
    /// `true` if the version is visible, `false` otherwise
    pub fn is_visible(&self, snapshot: &TransactionSnapshot, created_by_tx: TxId) -> bool {
        // Special case: TxId(0) is for pre-existing data (e.g., test fixtures, migrations)
        // Always treat it as visible to ensure backward compatibility
        if created_by_tx.as_u64() == 0 {
            return true;
        }

        // Check if transaction committed
        let committed = self.committed.read_or_recover();

        match committed.get(created_by_tx) {
            None => false, // Not committed - not visible
            Some(commit_ts) => {
                // Apply visibility rules from snapshot
                snapshot.is_visible(created_by_tx, Some(commit_ts))
            }
        }
    }

    /// Get the number of active transactions.
    ///
    /// This is primarily useful for testing and monitoring.
    #[allow(dead_code)]
    pub fn active_count(&self) -> usize {
        let active_guard = self.active.lock_or_recover();
        active_guard.len()
    }

    /// Get the number of committed transactions tracked.
    ///
    /// This is primarily useful for testing and monitoring.
    #[allow(dead_code)]
    pub fn committed_count(&self) -> usize {
        let committed = self.committed.read_or_recover();
        committed.total_transaction_count()
    }

    /// Compress the commit log to reduce memory usage (Issue #237).
    ///
    /// This analyzes sequential transaction patterns and converts them
    /// to epoch ranges, achieving 10-100x memory reduction for typical
    /// workloads.
    ///
    /// # When to Call
    ///
    /// - Periodically (e.g., every 10K commits)
    /// - During idle periods
    /// - Before checkpointing
    ///
    /// # Performance
    ///
    /// O(N log N) where N is the number of uncompressed transactions.
    /// Relatively expensive, so should not be called on every commit.
    pub fn compress_commit_log(&self) {
        let mut committed = self.committed.write_or_recover();
        committed.compress();
    }

    /// Get memory usage of the commit log in bytes.
    ///
    /// This is useful for monitoring and triggering compression.
    pub fn commit_log_memory_usage(&self) -> usize {
        let committed = self.committed.read_or_recover();
        committed.memory_usage()
    }

    /// Get compression statistics.
    ///
    /// This provides detailed information about compression ratio,
    /// memory usage, and memory savings.
    pub fn get_compression_stats(&self) -> CompressionStats {
        let committed = self.committed.read_or_recover();
        committed.get_stats()
    }

    /// Check if compression should be triggered based on memory threshold.
    ///
    /// This is a convenience method to help decide when to call `compress_commit_log()`.
    /// Returns `true` if the current commit log memory usage exceeds the threshold.
    ///
    /// # Arguments
    ///
    /// * `threshold_bytes` - Memory threshold in bytes
    ///
    /// # Returns
    ///
    /// `true` if compression is recommended, `false` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Compress if commit log uses > 1MB
    /// if db.should_compress_commit_log(1024 * 1024) {
    ///     db.compress_commit_log();
    /// }
    /// ```
    pub fn should_compress_commit_log(&self, threshold_bytes: usize) -> bool {
        self.commit_log_memory_usage() > threshold_bytes
    }

    /// Check if compression should be triggered based on exception count.
    ///
    /// This is an alternative trigger mechanism that compresses when there are
    /// many uncompressed exceptions (indicating potential for compression).
    ///
    /// # Arguments
    ///
    /// * `threshold_exceptions` - Number of exceptions to trigger compression
    ///
    /// # Returns
    ///
    /// `true` if compression is recommended, `false` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Compress if there are > 10K uncompressed exceptions
    /// if db.should_compress_by_exception_count(10_000) {
    ///     db.compress_commit_log();
    /// }
    /// ```
    pub fn should_compress_by_exception_count(&self, threshold_exceptions: usize) -> bool {
        let stats = self.get_compression_stats();
        stats.exception_count > threshold_exceptions
    }
}

impl Default for TxVisibilityManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_visibility_committed_before() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Version committed before snapshot - visible
        assert!(snapshot.is_visible(TxId::new(1), Some(50.into())));
    }

    #[test]
    fn test_snapshot_visibility_committed_after() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Version committed after snapshot - not visible
        assert!(!snapshot.is_visible(TxId::new(1), Some(150.into())));
    }

    #[test]
    fn test_snapshot_visibility_uncommitted() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Uncommitted version - not visible
        assert!(!snapshot.is_visible(TxId::new(1), None));
    }

    #[test]
    fn test_snapshot_visibility_active_transaction() {
        let mut active = HashSet::new();
        active.insert(TxId::new(1));

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(active),
        };

        // Version from active transaction - not visible even if committed before snapshot
        assert!(!snapshot.is_visible(TxId::new(1), Some(50.into())));
    }

    #[test]
    fn test_visibility_manager_creation() {
        let manager = TxVisibilityManager::new();
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 0);
    }

    #[test]
    fn test_register_active() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));
        manager.register_active(TxId::new(2));

        assert_eq!(manager.active_count(), 2);
    }

    #[test]
    fn test_capture_snapshot() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));
        manager.register_active(TxId::new(2));

        let snapshot = manager.capture_snapshot(100.into());

        assert_eq!(snapshot.snapshot_timestamp, 100.into());
        assert_eq!(snapshot.active_transactions.len(), 2);
        assert!(snapshot.active_transactions.contains(&TxId::new(1)));
        assert!(snapshot.active_transactions.contains(&TxId::new(2)));
    }

    #[test]
    fn test_register_commit() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));

        assert_eq!(manager.active_count(), 1);
        assert_eq!(manager.committed_count(), 0);

        manager.register_commit(TxId::new(1), 100.into());

        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 1);
    }

    #[test]
    fn test_register_abort() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));

        assert_eq!(manager.active_count(), 1);

        manager.register_abort(TxId::new(1));

        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 0);
    }

    #[test]
    fn test_is_visible_committed_transaction() {
        let manager = TxVisibilityManager::new();

        // Start and commit transaction 1
        manager.register_active(TxId::new(1));
        manager.register_commit(TxId::new(1), 50.into());

        // Take snapshot after commit
        let snapshot = manager.capture_snapshot(100.into());

        // Version from tx1 should be visible
        assert!(manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_uncommitted_transaction() {
        let manager = TxVisibilityManager::new();

        // Start transaction 1 but don't commit
        manager.register_active(TxId::new(1));

        let snapshot = manager.capture_snapshot(100.into());

        // Version from uncommitted tx1 should not be visible
        assert!(!manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_concurrent_transaction() {
        let manager = TxVisibilityManager::new();

        // Start tx1
        manager.register_active(TxId::new(1));

        // Take snapshot (tx1 is active)
        let snapshot = manager.capture_snapshot(100.into());

        // Commit tx1 after snapshot
        manager.register_commit(TxId::new(1), 90.into());

        // Even though tx1 committed before snapshot timestamp,
        // it was active at snapshot time, so not visible
        assert!(!manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_concurrent_snapshots() {
        let manager = TxVisibilityManager::new();

        // Start tx1
        manager.register_active(TxId::new(1));

        // Snapshot 1 - sees tx1 as active
        let snapshot1 = manager.capture_snapshot(100.into());
        assert_eq!(snapshot1.active_transactions.len(), 1);

        // Commit tx1, start tx2
        manager.register_commit(TxId::new(1), 110.into());
        manager.register_active(TxId::new(2));

        // Snapshot 2 - sees tx2 as active, tx1 committed
        let snapshot2 = manager.capture_snapshot(120.into());
        assert_eq!(snapshot1.active_transactions.len(), 1);
        assert!(snapshot2.active_transactions.contains(&TxId::new(2)));

        // Original snapshot1 unchanged
        assert_eq!(snapshot1.active_transactions.len(), 1);
        assert!(snapshot1.active_transactions.contains(&TxId::new(1)));
    }

    #[test]
    fn test_count_methods() {
        let manager = TxVisibilityManager::new();

        // Initially empty
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 0);

        // Add active transactions
        manager.register_active(TxId::new(1));
        manager.register_active(TxId::new(2));
        assert_eq!(manager.active_count(), 2);
        assert_eq!(manager.committed_count(), 0);

        // Commit one
        manager.register_commit(TxId::new(1), 100.into());
        assert_eq!(manager.active_count(), 1);
        assert_eq!(manager.committed_count(), 1);

        // Abort the other
        manager.register_abort(TxId::new(2));
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 1);
    }

    #[test]
    fn test_concurrent_visibility_checks() {
        use std::sync::Arc;
        use std::thread;

        let manager = Arc::new(TxVisibilityManager::new());

        // Setup: commit several transactions
        for i in 1..=10 {
            manager.register_active(TxId::new(i));
            manager.register_commit(TxId::new(i), ((i * 10) as i64).into());
        }

        // Take snapshot after all commits (timestamp 100 is the last commit)
        let snapshot = manager.capture_snapshot(101.into());

        // Spawn multiple threads doing concurrent visibility checks
        // This test demonstrates that RwLock allows concurrent readers
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let mgr = Arc::clone(&manager);
                let snap = snapshot.clone();
                thread::spawn(move || {
                    // Each thread performs many visibility checks
                    for _ in 0..1000 {
                        let tx_id = TxId::new((i % 10) + 1);
                        // All these transactions were committed before snapshot time
                        assert!(mgr.is_visible(&snap, tx_id));
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify the manager is still in a valid state
        assert_eq!(manager.committed_count(), 10);
    }

    // ============================================================================
    // RED PHASE: Tests for epoch-based compression (Issue #237)
    // These tests will fail until compression is implemented
    // ============================================================================

    #[test]
    fn test_epoch_range_basic() {
        // Test that EpochRange can represent a range of sequential transactions
        let epoch = EpochRange::new(TxId::new(1), TxId::new(100), 1000.into(), 1099.into());

        // Should contain transactions in the range
        assert!(epoch.contains(TxId::new(1)));
        assert!(epoch.contains(TxId::new(50)));
        assert!(epoch.contains(TxId::new(100)));

        // Should not contain transactions outside the range
        assert!(!epoch.contains(TxId::new(0)));
        assert!(!epoch.contains(TxId::new(101)));

        // Should be able to get commit timestamp for transactions in range
        assert_eq!(epoch.get_commit_timestamp(TxId::new(1)), Some(1000.into()));
        assert_eq!(epoch.get_commit_timestamp(TxId::new(50)), Some(1049.into()));
        assert_eq!(
            epoch.get_commit_timestamp(TxId::new(100)),
            Some(1099.into())
        );
        assert_eq!(epoch.get_commit_timestamp(TxId::new(101)), None);
    }

    #[test]
    fn test_compressed_commit_log_sequential() {
        // Test compression of fully sequential transactions
        let mut log = CompressedCommitLog::new();

        // Add 1000 sequential transactions
        for i in 1..=1000 {
            log.insert(TxId::new(i), (1000 + (i as i64) - 1).into());
        }

        // Trigger compression
        log.compress();

        // Should have compressed into 1 epoch with 0 exceptions
        assert_eq!(log.epoch_count(), 1);
        assert_eq!(log.exception_count(), 0);

        // All lookups should still work
        assert_eq!(log.get(TxId::new(1)), Some(1000.into()));
        assert_eq!(log.get(TxId::new(500)), Some(1499.into()));
        assert_eq!(log.get(TxId::new(1000)), Some(1999.into()));

        // Memory usage should be much smaller than uncompressed
        let compressed_size = log.memory_usage();
        let uncompressed_size = 1000 * 40; // ~40 bytes per entry in BTreeMap (realistic estimate)
        assert!(
            compressed_size < uncompressed_size / 10,
            "Compressed size {} should be < 10% of uncompressed size {}",
            compressed_size,
            uncompressed_size
        );
    }

    #[test]
    fn test_compressed_commit_log_with_gaps() {
        // Test compression with some non-sequential transactions (aborts)
        let mut log = CompressedCommitLog::new();

        // Add mostly sequential transactions with some gaps
        for i in 1..=1000 {
            if i % 100 != 0 {
                // Skip every 100th transaction (simulating aborts)
                log.insert(TxId::new(i), (1000 + (i as i64) - 1).into());
            }
        }

        log.compress();

        // Should have compressed into multiple epochs with some gaps
        // Exact count depends on compression algorithm, but should be << 1000
        let total_entries = log.epoch_count() + log.exception_count();
        assert!(
            total_entries < 100,
            "Total entries {} should be much less than 1000",
            total_entries
        );

        // All lookups should still work
        assert_eq!(log.get(TxId::new(1)), Some(1000.into()));
        assert_eq!(log.get(TxId::new(50)), Some(1049.into()));
        assert_eq!(log.get(TxId::new(100)), None); // Was skipped
        assert_eq!(log.get(TxId::new(101)), Some(1100.into()));
    }

    #[test]
    fn test_compressed_commit_log_outliers() {
        // Test compression with timestamp outliers (non-linear time progression)
        let mut log = CompressedCommitLog::new();

        // Add sequential transactions but with a timestamp outlier
        for i in 1..=100 {
            let timestamp = if i == 50 {
                5000.into() // Outlier timestamp
            } else {
                (1000 + (i as i64) - 1).into()
            };
            log.insert(TxId::new(i), timestamp);
        }

        log.compress();

        // Should have epochs + 1 exception for the outlier
        assert!(
            log.exception_count() >= 1,
            "Should have at least 1 exception for the outlier"
        );

        // All lookups should still work correctly
        assert_eq!(log.get(TxId::new(1)), Some(1000.into()));
        assert_eq!(log.get(TxId::new(50)), Some(5000.into())); // Outlier
        assert_eq!(log.get(TxId::new(100)), Some(1099.into()));
    }

    #[test]
    fn test_compressed_commit_log_incremental() {
        // Test that compression can be done incrementally
        let mut log = CompressedCommitLog::new();

        // Add and compress first batch
        for i in 1..=1000 {
            log.insert(TxId::new(i), (1000 + (i as i64) - 1).into());
        }
        log.compress();

        let first_epoch_count = log.epoch_count();

        // Add more sequential transactions
        for i in 1001..=2000 {
            log.insert(TxId::new(i), (1000 + (i as i64) - 1).into());
        }
        log.compress();

        // Should be able to extend existing epoch or create minimal new epochs
        assert!(
            log.epoch_count() <= first_epoch_count + 2,
            "Should not create many new epochs for sequential data"
        );

        // All lookups should still work
        assert_eq!(log.get(TxId::new(1)), Some(1000.into()));
        assert_eq!(log.get(TxId::new(1500)), Some(2499.into()));
        assert_eq!(log.get(TxId::new(2000)), Some(2999.into()));
    }

    #[test]
    fn test_visibility_manager_with_compression() {
        // Integration test: ensure visibility manager works with compression
        let manager = TxVisibilityManager::new();

        // Create many sequential transactions with sequential timestamps
        // This is the typical pattern when transactions commit in order
        let base_ts = 1000;
        for i in 1..=10000 {
            manager.register_active(TxId::new(i));
            manager.register_commit(TxId::new(i), (base_ts + (i as i64) - 1).into());
        }

        // Trigger compression
        manager.compress_commit_log();

        // Check compression stats
        let stats = manager.get_compression_stats();
        println!("Compression stats: {:#?}", stats);
        assert_eq!(stats.epoch_count, 1, "Should compress into 1 epoch");
        assert_eq!(stats.exception_count, 0, "Should have no exceptions");

        // Take snapshot at timestamp 6000
        // tx 5001 commits at 6000, tx 5000 commits at 5999
        let snapshot = manager.capture_snapshot(6000.into());

        // Committed transactions strictly before snapshot should be visible
        assert!(manager.is_visible(&snapshot, TxId::new(1))); // commits at 1000
        assert!(manager.is_visible(&snapshot, TxId::new(5000))); // commits at 5999

        // Transaction at exactly snapshot time should NOT be visible (strict <)
        assert!(!manager.is_visible(&snapshot, TxId::new(5001))); // commits at 6000

        // Transactions after snapshot should not be visible
        assert!(!manager.is_visible(&snapshot, TxId::new(5002))); // commits at 6001

        // Memory usage should be significantly reduced
        let memory_usage = manager.commit_log_memory_usage();
        let uncompressed_estimate = 10000 * 40; // More realistic BTreeMap estimate
        assert!(
            memory_usage < uncompressed_estimate / 5,
            "Memory usage {} should be < 20% of uncompressed size {}",
            memory_usage,
            uncompressed_estimate
        );
    }

    #[test]
    fn test_compression_ratio_measurement() {
        // Test that we can measure compression ratio accurately
        let manager = TxVisibilityManager::new();

        // Add 10000 sequential transactions with sequential timestamps
        let base_ts = 1000;
        for i in 1..=10000 {
            manager.register_active(TxId::new(i));
            manager.register_commit(TxId::new(i), (base_ts + (i as i64) - 1).into());
        }

        let uncompressed_count = manager.committed_count();
        let uncompressed_memory = manager.commit_log_memory_usage();

        // Compress
        manager.compress_commit_log();

        let compression_stats = manager.get_compression_stats();

        // Should have high compression ratio for sequential data
        assert_eq!(compression_stats.total_transactions, uncompressed_count);
        assert!(
            compression_stats.compression_ratio > 50.0,
            "Compression ratio {} should be > 50x for sequential data",
            compression_stats.compression_ratio
        );
        assert!(
            compression_stats.memory_saved_bytes > uncompressed_memory / 2,
            "Should save > 50% memory"
        );
    }

    #[test]
    fn test_should_compress_helpers() {
        // Test the helper methods for determining when to compress
        let manager = TxVisibilityManager::new();

        // Add transactions until memory threshold is exceeded
        let base_ts = 1000;
        for i in 1..=1000 {
            manager.register_active(TxId::new(i));
            manager.register_commit(TxId::new(i), (base_ts + (i as i64) - 1).into());
        }

        // Should recommend compression if memory exceeds low threshold
        assert!(
            manager.should_compress_commit_log(100),
            "Should recommend compression when memory > 100 bytes"
        );

        // Should not recommend compression if memory is below high threshold
        assert!(
            !manager.should_compress_commit_log(1_000_000),
            "Should not recommend compression when memory < 1MB"
        );

        // Should recommend compression if exception count exceeds threshold
        assert!(
            manager.should_compress_by_exception_count(500),
            "Should recommend compression when exceptions > 500"
        );

        // Should not recommend compression if exception count is below threshold
        assert!(
            !manager.should_compress_by_exception_count(10_000),
            "Should not recommend compression when exceptions < 10K"
        );

        // After compression, exception count should be low
        manager.compress_commit_log();
        assert!(
            !manager.should_compress_by_exception_count(10),
            "Should not need compression immediately after compressing"
        );
    }
}
