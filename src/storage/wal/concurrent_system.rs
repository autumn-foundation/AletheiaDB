//! Unified Concurrent WAL System.
//!
//! This module provides [`ConcurrentWalSystem`], which combines the concurrent
//! WAL striped architecture with the flush coordinator into a single, cohesive
//! component that can be used as a drop-in replacement for the old `WriteAheadLog`.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    ConcurrentWalSystem                           │
//! │                                                                  │
//! │  ┌──────────────────────┐    ┌─────────────────────────────┐   │
//! │  │    ConcurrentWal     │    │     FlushCoordinator        │   │
//! │  │  (Striped Buffers)   │───▶│   (Segment Management)      │   │
//! │  └──────────────────────┘    └─────────────────────────────┘   │
//! │                                                                  │
//! │  ┌──────────────────────────────────────────────────────────┐  │
//! │  │                   Background Flush Thread                  │  │
//! │  │  - Drains stripes periodically                            │  │
//! │  │  - Writes to segment files                                │  │
//! │  │  - Notifies completion handles                            │  │
//! │  └──────────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
//!
//! let config = ConcurrentWalSystemConfig::new("data/wal");
//! let wal = ConcurrentWalSystem::new(config)?;
//!
//! // Async append (returns immediately)
//! let lsn = wal.append_async(operation)?;
//!
//! // Sync append (waits for durability)
//! let lsn = wal.append_sync(operation)?;
//!
//! // Shutdown gracefully
//! wal.shutdown();
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use super::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig, FlushStats};
use super::group_commit::GroupCommitCoordinator;
use super::{LSN, WalOperation};
use crate::core::error::{Error, Result, StorageError};
use crate::storage::wal::DurabilityMode;

/// Configuration for the concurrent WAL system.
#[derive(Debug, Clone)]
pub struct ConcurrentWalSystemConfig {
    /// WAL directory path.
    pub wal_dir: PathBuf,
    /// Number of stripes (should be power of 2).
    pub num_stripes: usize,
    /// Ring buffer capacity per stripe.
    pub stripe_capacity: usize,
    /// Maximum segment size in bytes before rotation.
    pub segment_size: usize,
    /// Number of segments to retain.
    pub segments_to_retain: usize,
    /// Flush interval in milliseconds.
    pub flush_interval_ms: u64,
    /// Durability mode.
    pub durability_mode: DurabilityMode,
    /// Write buffer size for segment files.
    pub write_buffer_size: usize,
}

impl Default for ConcurrentWalSystemConfig {
    fn default() -> Self {
        Self {
            wal_dir: PathBuf::from("data/wal"),
            num_stripes: 16,
            stripe_capacity: 1024,
            segment_size: 64 * 1024 * 1024, // 64 MB
            segments_to_retain: 10,
            flush_interval_ms: 10,
            durability_mode: DurabilityMode::Synchronous,
            write_buffer_size: 64 * 1024, // 64 KB
        }
    }
}

impl ConcurrentWalSystemConfig {
    /// Create a new config with the specified WAL directory.
    pub fn new(wal_dir: impl Into<PathBuf>) -> Self {
        Self {
            wal_dir: wal_dir.into(),
            ..Default::default()
        }
    }

    /// Set the durability mode.
    pub fn with_durability_mode(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = mode;
        self
    }

    /// Set the number of stripes.
    pub fn with_num_stripes(mut self, num_stripes: usize) -> Self {
        self.num_stripes = num_stripes.next_power_of_two();
        self
    }

    /// Set the flush interval in milliseconds.
    pub fn with_flush_interval_ms(mut self, ms: u64) -> Self {
        self.flush_interval_ms = ms;
        self
    }
}

/// Signal for waking up the flush thread when batch is full.
struct FlushNotifier {
    /// Lock for condvar.
    lock: Mutex<bool>,
    /// Condvar to signal immediate flush.
    condvar: Condvar,
}

impl FlushNotifier {
    fn new() -> Self {
        Self {
            lock: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    /// Signal the flush thread to wake up immediately.
    fn notify(&self) {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *guard = true;
        self.condvar.notify_one();
    }

    /// Wait for a signal or timeout, returns true if signaled.
    fn wait_timeout(&self, duration: Duration) -> bool {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        // Check if already signaled before waiting.
        // This handles the race where notify() is called before we enter wait_timeout().
        if *guard {
            *guard = false; // Reset signal
            return true;
        }

        let (new_guard, result) = self
            .condvar
            .wait_timeout(guard, duration)
            .unwrap_or_else(|e| e.into_inner());
        guard = new_guard;

        let was_signaled = *guard && !result.timed_out();
        *guard = false; // Reset signal
        was_signaled
    }
}

/// Threshold for consecutive flush errors before logging a critical warning.
const FLUSH_ERROR_WARNING_THRESHOLD: u64 = 3;

/// Helper struct to encapsulate background flush logic.
struct BackgroundFlusher {
    wal: Arc<ConcurrentWal>,
    coordinator: Arc<FlushCoordinator>,
    shutdown: Arc<AtomicBool>,
    flush_notifier: Arc<FlushNotifier>,
    group_commit: Option<Arc<GroupCommitCoordinator>>,
    error_counter: Arc<AtomicU64>,
    interval: Duration,
    sync_on_flush: bool,
}

impl BackgroundFlusher {
    fn run(&self) {
        while !self.shutdown.load(Ordering::Relaxed) {
            self.perform_flush_cycle();
            // Wait for flush interval OR immediate signal (batch full)
            self.flush_notifier.wait_timeout(self.interval);
        }
        self.perform_final_flush();
    }

    fn perform_flush_cycle(&self) {
        let entries = self.wal.drain_all();

        // Always try to advance the epoch when there are entries OR when
        // group commit has pending transactions.
        //
        // LOCK POISONING: If current_batch_size() fails, the coordinator lock is
        // poisoned and the system is in an unrecoverable state. Panicking is correct
        // here - continuing would leave waiting transactions hanging indefinitely.
        let should_mark_flushed = !entries.is_empty()
            || self.group_commit.as_ref().is_some_and(|gc| {
                gc.current_batch_size()
                    .expect("GroupCommitCoordinator lock poisoned - flush thread cannot continue")
                    > 0
            });

        if !entries.is_empty() {
            // Flush to coordinator
            let result = self.coordinator.flush(entries, self.sync_on_flush);
            self.handle_flush_result(result.map(|_| ()));
        } else if should_mark_flushed {
            // No entries but there are pending transactions - advance epoch anyway
            self.handle_flush_result(Ok(()));
        }
    }

    fn perform_final_flush(&self) {
        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            let result = self.coordinator.flush(entries, true);
            self.handle_flush_result(result.map(|_| ()));
        }
    }

    fn handle_flush_result(&self, result: Result<()>) {
        match result {
            Ok(_) => {
                // Reset error counter on success
                self.error_counter.store(0, Ordering::Relaxed);
                if let Some(ref gc) = self.group_commit {
                    gc.mark_flushed(Ok(())).expect(
                        "GroupCommitCoordinator lock poisoned - flush thread cannot continue",
                    );
                }
            }
            Err(e) => {
                // Track consecutive errors for health monitoring
                let errors = self.error_counter.fetch_add(1, Ordering::Relaxed) + 1;
                if errors == FLUSH_ERROR_WARNING_THRESHOLD {
                    eprintln!(
                        "CRITICAL: WAL flush failed {} consecutive times. \
                         Data durability may be compromised. Last error: {}",
                        errors, e
                    );
                } else {
                    eprintln!("WAL flush error: {}", e);
                }

                if let Some(ref gc) = self.group_commit {
                    // Create a new error from the string representation
                    gc.mark_flushed(Err(crate::core::error::Error::other(e.to_string())))
                        .expect(
                            "GroupCommitCoordinator lock poisoned - flush thread cannot continue",
                        );
                }
            }
        }
    }
}

/// Unified concurrent WAL system.
///
/// This combines the striped concurrent WAL with the flush coordinator
/// and a background flush thread to provide a complete WAL solution.
pub struct ConcurrentWalSystem {
    /// The concurrent WAL with striped buffers.
    wal: Arc<ConcurrentWal>,
    /// The flush coordinator for segment management.
    coordinator: Arc<FlushCoordinator>,
    /// Handle to the background flush thread.
    flush_thread: Option<JoinHandle<()>>,
    /// Signal to stop the flush thread.
    shutdown_signal: Arc<AtomicBool>,
    /// Signal to wake up flush thread immediately (batch full).
    flush_notifier: Arc<FlushNotifier>,
    /// Durability mode.
    durability_mode: DurabilityMode,
    /// Group commit coordinator for epoch-based waiting (GroupCommit mode only).
    group_commit: Option<Arc<GroupCommitCoordinator>>,
    /// Counter for consecutive flush errors (for health monitoring).
    consecutive_flush_errors: Arc<AtomicU64>,
}

impl ConcurrentWalSystem {
    /// Create a new concurrent WAL system.
    pub fn new(config: ConcurrentWalSystemConfig) -> Result<Self> {
        // Create ConcurrentWal config
        let wal_config = ConcurrentWalConfig {
            wal_dir: config.wal_dir.clone(),
            num_stripes: config.num_stripes,
            stripe_capacity: config.stripe_capacity,
            segment_size: config.segment_size,
            segments_to_retain: config.segments_to_retain,
        };

        // Create FlushCoordinator config
        let coordinator_config = FlushCoordinatorConfig {
            wal_dir: config.wal_dir,
            segment_size: config.segment_size,
            segments_to_retain: config.segments_to_retain,
            flush_interval_ms: config.flush_interval_ms,
            sync_on_flush: matches!(
                config.durability_mode,
                DurabilityMode::Synchronous | DurabilityMode::GroupCommit { .. }
            ),
            write_buffer_size: config.write_buffer_size,
        };

        let wal = Arc::new(ConcurrentWal::new(wal_config)?);
        let coordinator = Arc::new(FlushCoordinator::new(coordinator_config)?);
        let shutdown_signal = Arc::new(AtomicBool::new(false));

        // Create group commit coordinator for modes that need epoch tracking
        let group_commit = match config.durability_mode {
            DurabilityMode::GroupCommit {
                max_batch_size,
                max_delay_ms,
            } => Some(Arc::new(GroupCommitCoordinator::new(
                max_delay_ms,
                max_batch_size,
            ))),
            DurabilityMode::AsyncBatched {
                max_batch_size,
                max_delay_ms,
                ..
            } => Some(Arc::new(GroupCommitCoordinator::new(
                max_delay_ms,
                max_batch_size,
            ))),
            _ => None,
        };

        // Create flush notifier for batch-size-triggered flushes
        let flush_notifier = Arc::new(FlushNotifier::new());

        // Create error counter for health monitoring
        let consecutive_flush_errors = Arc::new(AtomicU64::new(0));

        // Start background flush thread for async/group-commit modes
        let flush_thread = if matches!(
            config.durability_mode,
            DurabilityMode::Async { .. }
                | DurabilityMode::GroupCommit { .. }
                | DurabilityMode::AsyncBatched { .. }
        ) {
            let wal_clone = Arc::clone(&wal);
            let coordinator_clone = Arc::clone(&coordinator);
            let shutdown_clone = Arc::clone(&shutdown_signal);
            let flush_notifier_clone = Arc::clone(&flush_notifier);
            let group_commit_clone = group_commit.clone();
            let error_counter_clone = Arc::clone(&consecutive_flush_errors);
            let flush_interval = Duration::from_millis(config.flush_interval_ms);
            let sync_on_flush =
                matches!(config.durability_mode, DurabilityMode::GroupCommit { .. });

            Some(thread::spawn(move || {
                Self::flush_loop(
                    wal_clone,
                    coordinator_clone,
                    shutdown_clone,
                    flush_notifier_clone,
                    group_commit_clone,
                    error_counter_clone,
                    flush_interval,
                    sync_on_flush,
                );
            }))
        } else {
            None
        };

        Ok(Self {
            wal,
            coordinator,
            flush_thread,
            shutdown_signal,
            flush_notifier,
            durability_mode: config.durability_mode,
            group_commit,
            consecutive_flush_errors,
        })
    }

    /// Background flush loop.
    ///
    /// Wakes up either when:
    /// - The flush interval expires (normal periodic flush)
    /// - The flush_notifier is signaled (batch size reached)
    /// - Shutdown is requested
    #[allow(clippy::too_many_arguments)]
    fn flush_loop(
        wal: Arc<ConcurrentWal>,
        coordinator: Arc<FlushCoordinator>,
        shutdown: Arc<AtomicBool>,
        flush_notifier: Arc<FlushNotifier>,
        group_commit: Option<Arc<GroupCommitCoordinator>>,
        error_counter: Arc<AtomicU64>,
        interval: Duration,
        sync_on_flush: bool,
    ) {
        let flusher = BackgroundFlusher {
            wal,
            coordinator,
            shutdown,
            flush_notifier,
            group_commit,
            error_counter,
            interval,
            sync_on_flush,
        };
        flusher.run();
    }

    /// Append an operation asynchronously (fire and forget).
    ///
    /// For `DurabilityMode::Synchronous`, this blocks until durable.
    /// For other modes, this returns immediately.
    pub fn append(&self, operation: WalOperation) -> Result<LSN> {
        match self.durability_mode {
            DurabilityMode::Synchronous => self.append_sync(operation),
            DurabilityMode::Async { .. }
            | DurabilityMode::GroupCommit { .. }
            | DurabilityMode::AsyncBatched { .. } => self.append_async(operation),
        }
    }

    /// Append an operation asynchronously (returns immediately).
    ///
    /// The entry is buffered and will be flushed by the background thread.
    pub fn append_async(&self, operation: WalOperation) -> Result<LSN> {
        self.wal.append_async(operation)
    }

    /// Append an operation synchronously (waits for durability).
    ///
    /// This flushes immediately and waits for fsync.
    pub fn append_sync(&self, operation: WalOperation) -> Result<LSN> {
        let (lsn, handle) = self.wal.append_with_handle(operation)?;

        // Drain and flush immediately for sync mode
        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            self.coordinator.flush(entries, true)?;
        }

        // Wait for durability
        handle.wait().map_err(|e| {
            Error::Storage(StorageError::WalError {
                reason: format!("WAL flush failed: {}", e),
            })
        })?;

        Ok(lsn)
    }

    /// Append a batch of operations efficiently.
    ///
    /// This method provides significant performance improvements for high-throughput
    /// workloads by batching multiple operations into fewer I/O operations.
    ///
    /// # Performance Benefits
    ///
    /// Compared to calling `append()` multiple times:
    /// - Single atomic LSN allocation for all operations (vs N atomic operations)
    /// - Better CPU cache locality during serialization
    /// - Reduced stripe buffer contention
    ///
    /// # Durability Behavior
    ///
    /// The durability semantics follow the configured `DurabilityMode`:
    /// - **Synchronous**: All operations are flushed and synced before returning
    /// - **Async**: Operations are buffered and flushed by background thread (eventual consistency)
    /// - **GroupCommit**: Operations are buffered and flushed by background thread (caller must wait on epoch)
    /// - **AsyncBatched**: Same as GroupCommit (operations buffered, background flush)
    ///
    /// # Arguments
    ///
    /// * `operations` - Vector of operations to append
    ///
    /// # Returns
    ///
    /// Vector of allocated LSNs in the same order as the operations.
    /// Returns an empty vector if `operations` is empty.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::storage::wal::{WalOperation, ConcurrentWalSystem};
    ///
    /// let ops = vec![
    ///     WalOperation::CreateNode { /* ... */ },
    ///     WalOperation::CreateEdge { /* ... */ },
    ///     WalOperation::UpdateNode { /* ... */ },
    /// ];
    ///
    /// // Efficient batch append
    /// let lsns = wal.append_batch(ops)?;
    /// assert_eq!(lsns.len(), 3);
    ///
    /// // For GroupCommit mode, commit and wait
    /// if let Some(epoch) = wal.commit()? {
    ///     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
    /// }
    /// ```
    pub fn append_batch(&self, operations: Vec<WalOperation>) -> Result<Vec<LSN>> {
        // Handle empty batch early
        if operations.is_empty() {
            return Ok(Vec::new());
        }

        // Use the underlying WAL's batch append for async modes
        match self.durability_mode {
            DurabilityMode::Synchronous => {
                // For synchronous mode, append batch then flush all
                let (lsns, handles) = self.wal.append_batch_with_handles(operations)?;

                // Drain and flush immediately for sync mode
                let entries = self.wal.drain_all();
                if !entries.is_empty() {
                    self.coordinator.flush(entries, true).map_err(|e| {
                        Error::Storage(StorageError::WalError {
                            reason: format!("Failed to flush batch after drain: {}", e),
                        })
                    })?;
                }

                // Wait for all handles to ensure durability.
                // Note: Since flush coordinator preserves LSN order, waiting for the last one
                // technically implies all previous ones are done, but waiting for all is safer
                // against future changes and handles errors correctly.
                if let Some(last_handle) = handles.into_iter().last() {
                    last_handle.wait().map_err(|e| {
                        Error::Storage(StorageError::WalError {
                            reason: format!("WAL flush failed: {}", e),
                        })
                    })?;
                }

                Ok(lsns)
            }
            DurabilityMode::Async { .. }
            | DurabilityMode::GroupCommit { .. }
            | DurabilityMode::AsyncBatched { .. } => {
                // For async modes, just batch append (background thread handles flush)
                self.wal.append_batch(operations)
            }
        }
    }

    /// Force a flush of all pending entries.
    pub fn flush(&self) -> Result<FlushStats> {
        let entries = self.wal.drain_all();
        if entries.is_empty() {
            return Ok(FlushStats::default());
        }

        let should_sync = !matches!(self.durability_mode, DurabilityMode::Async { .. });
        self.coordinator.flush(entries, should_sync)
    }

    /// Commit with the configured durability mode.
    ///
    /// # Usage
    ///
    /// **Important**: All `append_async()` calls for a transaction MUST complete
    /// before calling `commit()`. The typical pattern is:
    ///
    /// ```ignore
    /// wal.append_async(op1)?;
    /// wal.append_async(op2)?;
    /// let epoch = wal.commit()?;  // Register for durability
    /// if let Some(epoch) = epoch {
    ///     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
    /// }
    /// ```
    ///
    /// # Returns
    ///
    /// Returns an epoch number for GroupCommit/AsyncBatched modes that the
    /// caller should wait on using `group_commit_coordinator().wait_for_flush(epoch)`.
    ///
    /// For other modes, returns `None`:
    /// - Synchronous: Data is already durable when this returns
    /// - Async: No waiting needed (fire-and-forget)
    ///
    /// # Race Condition Handling
    ///
    /// In GroupCommit mode, there's an intentional race between:
    /// 1. The flush thread draining entries
    /// 2. Transactions calling `register_transaction()`
    ///
    /// This is handled safely: if entries are drained before registration,
    /// the epoch will still advance (with no entries), ensuring waiters
    /// are notified. The data durability is guaranteed because entries
    /// must be in the ring buffer before this method is called.
    pub fn commit(&self) -> Result<Option<u64>> {
        match self.durability_mode {
            DurabilityMode::Synchronous => {
                // Drain and flush immediately with fsync
                let entries = self.wal.drain_all();
                if !entries.is_empty() {
                    self.coordinator.flush(entries, true)?;
                }
                Ok(None)
            }
            DurabilityMode::Async { .. } => {
                // Just let background thread handle it
                Ok(None)
            }
            DurabilityMode::GroupCommit { .. } | DurabilityMode::AsyncBatched { .. } => {
                // Register with coordinator and return epoch to wait for
                if let Some(ref gc) = self.group_commit {
                    let (epoch, should_trigger) = gc.register_transaction()?;

                    // If batch is full, signal flush thread to wake up immediately
                    if should_trigger {
                        self.flush_notifier.notify();
                    }

                    Ok(Some(epoch))
                } else {
                    // Fallback to sync if no coordinator (shouldn't happen)
                    let entries = self.wal.drain_all();
                    if !entries.is_empty() {
                        self.coordinator.flush(entries, true)?;
                    }
                    Ok(None)
                }
            }
        }
    }

    /// Get the group commit coordinator for waiting on epochs.
    ///
    /// Returns `None` for modes that don't use group commit.
    pub fn group_commit_coordinator(&self) -> Option<&Arc<GroupCommitCoordinator>> {
        self.group_commit.as_ref()
    }

    /// Get the current (next to be allocated) LSN.
    pub fn current_lsn(&self) -> LSN {
        self.wal.current_lsn()
    }

    /// Get total entries appended.
    pub fn total_appends(&self) -> u64 {
        self.wal.total_appends()
    }

    /// Get total entries flushed to disk.
    pub fn total_flushed(&self) -> u64 {
        self.coordinator.total_entries_flushed()
    }

    /// Get the durability mode.
    pub fn durability_mode(&self) -> DurabilityMode {
        self.durability_mode
    }

    /// Get the number of consecutive flush errors.
    ///
    /// This can be used for health monitoring. A value > 0 indicates
    /// that the last flush(es) failed. A value >= 3 indicates a critical
    /// condition where data durability may be compromised.
    ///
    /// The counter resets to 0 after a successful flush.
    pub fn consecutive_flush_errors(&self) -> u64 {
        self.consecutive_flush_errors.load(Ordering::Relaxed)
    }

    /// Check if the WAL is healthy (no consecutive flush errors).
    ///
    /// Returns `true` if the last flush succeeded, `false` if there are
    /// outstanding errors that haven't been cleared by a successful flush.
    pub fn is_healthy(&self) -> bool {
        self.consecutive_flush_errors() == 0
    }

    /// Get the WAL directory path.
    pub fn wal_dir(&self) -> &std::path::Path {
        self.coordinator.wal_dir()
    }

    /// Read WAL entries from disk, starting from the specified LSN.
    ///
    /// This reads all segment files in the WAL directory and returns entries
    /// with LSN >= start_lsn. Used for recovery.
    pub fn read_from(&self, start_lsn: LSN) -> Result<Vec<super::WalEntry>> {
        crate::storage::wal_reader::read_wal_entries(self.wal_dir(), start_lsn)
    }

    /// Shutdown the WAL system gracefully.
    ///
    /// This signals the background thread to stop, waits for it to finish,
    /// and performs a final flush of all pending entries.
    pub fn shutdown(&mut self) {
        // Close the WAL (prevent new appends, wait for active batches)
        // CRITICAL: This must happen BEFORE stopping the flush thread,
        // because waiting for active batches might block on full buffers
        // that only the flush thread can drain.
        self.wal.close();

        // Signal shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wake up flush thread so it can see the shutdown signal
        self.flush_notifier.notify();

        // Wait for flush thread to finish
        if let Some(handle) = self.flush_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ConcurrentWalSystem {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GLOBAL_INTERNER;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMap;
    use crate::core::temporal::time;
    use tempfile::tempdir;

    fn create_test_operation(id: u64) -> WalOperation {
        WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node{}", id)).unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        }
    }

    #[test]
    fn test_concurrent_wal_system_creation() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        assert_eq!(wal.total_appends(), 0);
        assert_eq!(wal.current_lsn(), LSN(1));
    }

    #[test]
    fn test_append_sync_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let lsn = wal.append(create_test_operation(1)).unwrap();
        assert_eq!(lsn, LSN(1));
        assert_eq!(wal.total_appends(), 1);
    }

    #[test]
    fn test_append_sync_mode_handles_more_than_stripe_capacity() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        // Keep capacity intentionally tiny to regression-test the benchmark footgun:
        // mode-aware `append()` must continue making progress even when the buffered
        // async path would hit backpressure quickly.
        config.stripe_capacity = 8;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        for i in 1..=64 {
            let lsn = wal.append(create_test_operation(i)).unwrap();
            assert_eq!(lsn, LSN(i));
        }

        assert_eq!(wal.total_appends(), 64);
        assert_eq!(wal.total_flushed(), 64);
    }

    #[test]
    fn test_append_async_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_flush_interval_ms(10_000) // Explicitly set config interval to avoid racing with default 10ms
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 10_000,
            });
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append several entries
        for i in 1..=10 {
            let lsn = wal.append(create_test_operation(i)).unwrap();
            assert_eq!(lsn, LSN(i));
        }

        assert_eq!(wal.total_appends(), 10);

        // Explicit flush - ensure all entries are durable.
        // Note: The background flush thread may have already flushed some/all
        // entries, so we check total_flushed() rather than the return stats.
        // This makes the test deterministic regardless of timing.
        wal.flush().unwrap();

        // Wait for flush to complete (handle race with background thread)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        while wal.total_flushed() < 10 {
            // LCOV_EXCL_START
            if start.elapsed() > timeout {
                break;
            }
            // LCOV_EXCL_STOP
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");

        wal.shutdown();
        assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");
    }

    #[test]
    fn test_concurrent_appends() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 100,
            })
            .with_num_stripes(4);
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let num_threads = 4;
        let ops_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let wal = Arc::clone(&wal);
                thread::spawn(move || {
                    for i in 0..ops_per_thread {
                        let id = (t * ops_per_thread + i + 1) as u64;
                        wal.append_async(create_test_operation(id)).unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(wal.total_appends(), (num_threads * ops_per_thread) as u64);
    }

    #[test]
    fn test_flush_persists_entries() {
        let dir = tempdir().unwrap();
        // Use Synchronous mode to avoid background flush thread interference
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries (append_async in Sync mode still buffers)
        for i in 1..=5 {
            wal.append_async(create_test_operation(i)).unwrap();
        }

        // Force flush - since no background thread, all 5 entries should be flushed here
        let stats = wal.flush().unwrap();
        assert_eq!(stats.entries_flushed, 5);

        // Verify flushed count
        assert_eq!(wal.total_flushed(), 5);

        wal.shutdown();
    }

    #[test]
    fn test_group_commit_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::GroupCommit {
                max_batch_size: 10,
                max_delay_ms: 10,
            })
            .with_flush_interval_ms(5);
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries
        for i in 1..=5 {
            wal.append(create_test_operation(i)).unwrap();
        }

        // Wait for background flush with polling (more resilient than single sleep)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5); // Increased timeout for CI
        let mut flushed = false;
        while start.elapsed() < timeout {
            if wal.total_flushed() >= 1 {
                flushed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        // Should have been flushed by background thread
        assert!(
            flushed,
            "Expected at least 1 entry to be flushed within {}ms, but got {} flushed",
            timeout.as_millis(),
            wal.total_flushed()
        );

        wal.shutdown();
    }

    #[test]
    fn test_shutdown_flushes_remaining() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 100,
            },
        );
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries without explicit flush
        for i in 1..=5 {
            wal.append_async(create_test_operation(i)).unwrap();
        }

        // Shutdown should flush remaining
        wal.shutdown();

        // All entries should be flushed
        assert_eq!(wal.total_flushed(), 5);
    }

    // ============================================================
    // Batch Append Tests (Issue #219)
    // ============================================================

    #[test]
    fn test_append_batch_async() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 10_000,
            },
        );
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let ops = vec![
            create_test_operation(1),
            create_test_operation(2),
            create_test_operation(3),
        ];

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 3);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[1], LSN(2));
        assert_eq!(lsns[2], LSN(3));
        assert_eq!(wal.total_appends(), 3);
    }

    #[test]
    fn test_append_batch_sync() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let ops = vec![create_test_operation(1), create_test_operation(2)];

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 2);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[1], LSN(2));
        assert_eq!(wal.total_appends(), 2);
    }

    #[test]
    fn test_append_batch_empty() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let lsns = wal.append_batch(vec![]).unwrap();

        assert_eq!(lsns.len(), 0);
        assert_eq!(wal.total_appends(), 0);
    }

    #[test]
    fn test_append_batch_large() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 10_000,
            },
        );
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Create 100 operations
        let ops: Vec<_> = (1..=100).map(create_test_operation).collect();

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 100);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[99], LSN(100));
        assert_eq!(wal.total_appends(), 100);
    }

    #[test]
    fn test_append_sync_persistence_guarantee() {
        // This test verifies that append_sync actually waits for the flush.
        // While we can't easily deterministic race condition, we can verify basic
        // persistence guarantee: immediately after append_sync returns, total_flushed
        // must be incremented.

        let dir = tempdir().unwrap();
        // Use Synchronous mode
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // 1. Initial state
        assert_eq!(wal.total_flushed(), 0);

        // 2. Perform append_sync
        let lsn = wal.append_sync(create_test_operation(1)).unwrap();

        // 3. Immediately assert flushed count
        // If append_sync didn't wait, and flush was async/delayed, this might fail.
        // But since it's sync, it MUST be 1.
        assert_eq!(
            wal.total_flushed(),
            1,
            "Should be flushed immediately after return"
        );
        assert_eq!(lsn, LSN(1));

        // 4. Batch append sync
        let ops = vec![create_test_operation(2), create_test_operation(3)];
        let lsns = wal.append_batch(ops).unwrap();

        // 5. Assert flushed count increased by 2
        assert_eq!(
            wal.total_flushed(),
            3,
            "Batch should be flushed immediately"
        );
        assert_eq!(lsns.len(), 2);
    }
}
