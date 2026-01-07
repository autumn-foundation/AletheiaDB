//! Group commit coordination for batched WAL flushes.
//!
//! This module provides [`GroupCommitCoordinator`], which manages the epoch-based
//! waiting mechanism for [`GroupCommit`](super::DurabilityMode::GroupCommit) mode.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::utils::{Error, StorageError};

/// Coordinates group commit batching and waiting.
///
/// The coordinator uses an epoch-based system where:
/// 1. Each transaction registers and receives the current epoch number
/// 2. Multiple transactions accumulate in the same epoch
/// 3. When the batch is flushed, the epoch advances
/// 4. All waiting transactions for that epoch are notified
///
/// This allows ACID durability with amortized fsync cost across many transactions.
///
/// # Epoch Model
///
/// ```text
/// Epoch 0: [tx1, tx2, tx3] → flush → advance to epoch 1
/// Epoch 1: [tx4, tx5] → flush → advance to epoch 2
/// ...
/// ```
///
/// Each transaction waits on its epoch number. When `mark_flushed()` is called,
/// all transactions waiting on that epoch are released.
///
/// # Error Propagation
///
/// If a flush fails, the error is stored and propagated to all waiting transactions.
/// This ensures no transaction incorrectly believes its data is durable.
pub struct GroupCommitCoordinator {
    /// State protected by mutex
    state: Mutex<GroupCommitState>,
    /// Condition variable for flush completion
    flush_complete: Condvar,
    /// Configuration
    config: GroupCommitConfig,
}

/// Configuration for group commit behavior.
#[derive(Debug, Clone)]
pub struct GroupCommitConfig {
    /// Maximum time to wait for more transactions before flushing.
    pub max_delay_ms: u64,
    /// Maximum transactions to batch before forcing a flush.
    pub max_batch_size: usize,
}

impl Default for GroupCommitConfig {
    fn default() -> Self {
        Self {
            max_delay_ms: 10,
            max_batch_size: 200,
        }
    }
}

struct GroupCommitState {
    /// Current epoch (increments on each flush)
    current_epoch: u64,
    /// Number of transactions in current batch
    batch_count: usize,
    /// Epoch that has been durably flushed
    flushed_epoch: u64,
    /// Error from last flush (for propagation to waiters)
    last_flush_error: Option<String>,
}

impl GroupCommitCoordinator {
    /// Create a new GroupCommitCoordinator with the given configuration.
    pub fn new(max_delay_ms: u64, max_batch_size: usize) -> Self {
        Self {
            state: Mutex::new(GroupCommitState {
                current_epoch: 0,
                batch_count: 0,
                flushed_epoch: 0,
                last_flush_error: None,
            }),
            flush_complete: Condvar::new(),
            config: GroupCommitConfig {
                max_delay_ms,
                max_batch_size,
            },
        }
    }

    /// Create a new GroupCommitCoordinator with default configuration.
    pub fn with_defaults() -> Self {
        let config = GroupCommitConfig::default();
        Self::new(config.max_delay_ms, config.max_batch_size)
    }

    /// Register a transaction for group commit.
    ///
    /// Returns the epoch number that this transaction should wait for.
    /// If the batch is full, returns `true` in the second element to signal
    /// that an immediate flush should be triggered.
    ///
    /// # Returns
    ///
    /// A tuple of (epoch_to_wait_for, should_trigger_flush)
    pub fn register_transaction(&self) -> (u64, bool) {
        let mut state = self.state.lock().expect("group commit lock poisoned");

        state.batch_count += 1;
        let epoch = state.current_epoch;

        // Check if we should trigger immediate flush (batch full)
        let should_flush = state.batch_count >= self.config.max_batch_size;

        (epoch, should_flush)
    }

    /// Wait for the specified epoch to be flushed.
    ///
    /// This blocks until the epoch has been durably flushed. If the flush
    /// failed, the error is propagated to all waiting transactions.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The flush for this epoch failed
    /// - The wait times out (10x max_delay_ms with 2 second minimum for thread startup)
    pub fn wait_for_flush(&self, epoch: u64) -> Result<(), Error> {
        let mut state = self.state.lock().expect("group commit lock poisoned");

        // Use a generous timeout to account for thread startup delays on slow CI systems
        // Minimum 2 seconds for thread startup, or 10x max_delay for longer intervals
        let timeout =
            Duration::from_millis(self.config.max_delay_ms * 10).max(Duration::from_secs(2));

        while state.flushed_epoch <= epoch {
            let (new_state, timeout_result) = self
                .flush_complete
                .wait_timeout(state, timeout)
                .expect("group commit lock poisoned");

            state = new_state;

            if timeout_result.timed_out() && state.flushed_epoch <= epoch {
                return Err(Error::Storage(StorageError::WalError {
                    reason: format!(
                        "Group commit timeout waiting for epoch {} (current flushed: {})",
                        epoch, state.flushed_epoch
                    ),
                }));
            }
        }

        // Check for flush errors
        if let Some(ref error_msg) = state.last_flush_error {
            return Err(Error::Storage(StorageError::WalError {
                reason: format!("Group commit flush failed: {}", error_msg),
            }));
        }

        Ok(())
    }

    /// Mark the current batch as flushed.
    ///
    /// Called by the flush thread after completing a flush. This:
    /// 1. Records any error for propagation
    /// 2. Advances the epoch
    /// 3. Resets the batch counter
    /// 4. Notifies all waiting transactions
    ///
    /// # Arguments
    ///
    /// * `result` - The result of the flush operation. If `Err`, the error
    ///   is stored and propagated to all waiters.
    pub fn mark_flushed(&self, result: Result<(), Error>) {
        let mut state = self.state.lock().expect("group commit lock poisoned");

        // Store any error for propagation
        state.last_flush_error = result.err().map(|e| e.to_string());

        // Advance the flushed epoch (even on error, so waiters wake up)
        state.flushed_epoch = state.current_epoch + 1;
        state.current_epoch += 1;
        state.batch_count = 0;

        // Wake all waiting transactions
        self.flush_complete.notify_all();
    }

    /// Get the current batch size.
    ///
    /// Useful for monitoring and testing.
    pub fn current_batch_size(&self) -> usize {
        self.state
            .lock()
            .expect("group commit lock poisoned")
            .batch_count
    }

    /// Get the current epoch.
    ///
    /// Useful for monitoring and testing.
    pub fn current_epoch(&self) -> u64 {
        self.state
            .lock()
            .expect("group commit lock poisoned")
            .current_epoch
    }

    /// Get the last flushed epoch.
    ///
    /// Useful for monitoring and testing.
    pub fn flushed_epoch(&self) -> u64 {
        self.state
            .lock()
            .expect("group commit lock poisoned")
            .flushed_epoch
    }

    /// Check if a flush should be triggered based on batch size.
    ///
    /// Called by the flush thread to check if the batch is full.
    pub fn should_flush(&self) -> bool {
        let state = self.state.lock().expect("group commit lock poisoned");
        state.batch_count >= self.config.max_batch_size
    }

    /// Get the maximum delay for this coordinator.
    pub fn max_delay(&self) -> Duration {
        Duration::from_millis(self.config.max_delay_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_new_coordinator() {
        let coord = GroupCommitCoordinator::new(10, 200);
        assert_eq!(coord.current_epoch(), 0);
        assert_eq!(coord.flushed_epoch(), 0);
        assert_eq!(coord.current_batch_size(), 0);
    }

    #[test]
    fn test_register_transaction() {
        let coord = GroupCommitCoordinator::new(10, 5);

        // Register transactions
        for i in 0..4 {
            let (epoch, should_flush) = coord.register_transaction();
            assert_eq!(epoch, 0);
            assert!(!should_flush, "should not flush at batch size {}", i + 1);
        }

        // Fifth transaction should trigger flush
        let (epoch, should_flush) = coord.register_transaction();
        assert_eq!(epoch, 0);
        assert!(should_flush, "should flush when batch is full");
    }

    #[test]
    fn test_mark_flushed_advances_epoch() {
        let coord = GroupCommitCoordinator::new(10, 100);

        coord.register_transaction();
        coord.register_transaction();

        assert_eq!(coord.current_epoch(), 0);
        assert_eq!(coord.current_batch_size(), 2);

        coord.mark_flushed(Ok(()));

        assert_eq!(coord.current_epoch(), 1);
        assert_eq!(coord.flushed_epoch(), 1);
        assert_eq!(coord.current_batch_size(), 0);
    }

    #[test]
    fn test_wait_for_flush_success() {
        let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
        let coord_clone = Arc::clone(&coord);

        let (epoch, _) = coord.register_transaction();

        // Spawn a thread to mark flushed after a short delay
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            coord_clone.mark_flushed(Ok(()));
        });

        // Wait should succeed
        let result = coord.wait_for_flush(epoch);
        assert!(result.is_ok());

        handle.join().unwrap();
    }

    #[test]
    fn test_wait_for_flush_error_propagation() {
        let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
        let coord_clone = Arc::clone(&coord);

        let (epoch, _) = coord.register_transaction();

        // Spawn a thread to mark flushed with error
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            coord_clone.mark_flushed(Err(Error::Storage(StorageError::WalError {
                reason: "disk full".to_string(),
            })));
        });

        // Wait should return the error
        let result = coord.wait_for_flush(epoch);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("disk full"));

        handle.join().unwrap();
    }

    #[test]
    fn test_wait_for_flush_timeout() {
        let coord = GroupCommitCoordinator::new(10, 100); // 10ms max delay

        let (epoch, _) = coord.register_transaction();

        // Wait without anyone calling mark_flushed - should timeout
        let result = coord.wait_for_flush(epoch);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("timeout"));
    }

    #[test]
    fn test_multiple_waiters() {
        let coord = Arc::new(GroupCommitCoordinator::new(100, 100));

        // Register multiple transactions
        let mut epochs = Vec::new();
        for _ in 0..5 {
            let (epoch, _) = coord.register_transaction();
            epochs.push(epoch);
        }

        // All should be same epoch
        assert!(epochs.iter().all(|&e| e == 0));

        // Spawn multiple waiting threads
        let mut handles = Vec::new();
        for _ in 0..5 {
            let coord_clone = Arc::clone(&coord);
            handles.push(thread::spawn(move || coord_clone.wait_for_flush(0)));
        }

        // Let them start waiting
        thread::sleep(Duration::from_millis(10));

        // Mark flushed
        coord.mark_flushed(Ok(()));

        // All should succeed
        for handle in handles {
            let result = handle.join().unwrap();
            assert!(result.is_ok());
        }
    }

    #[test]
    fn test_multiple_epochs() {
        let coord = GroupCommitCoordinator::new(10, 100);

        // First batch
        coord.register_transaction();
        coord.register_transaction();
        coord.mark_flushed(Ok(()));

        assert_eq!(coord.current_epoch(), 1);

        // Second batch
        let (epoch, _) = coord.register_transaction();
        assert_eq!(epoch, 1);

        coord.mark_flushed(Ok(()));
        assert_eq!(coord.current_epoch(), 2);
    }

    #[test]
    fn test_should_flush() {
        let coord = GroupCommitCoordinator::new(10, 3);

        assert!(!coord.should_flush());

        coord.register_transaction();
        assert!(!coord.should_flush());

        coord.register_transaction();
        assert!(!coord.should_flush());

        coord.register_transaction();
        assert!(coord.should_flush());
    }

    #[test]
    fn test_max_delay() {
        let coord = GroupCommitCoordinator::new(42, 100);
        assert_eq!(coord.max_delay(), Duration::from_millis(42));
    }

    #[test]
    fn test_with_defaults() {
        let coord = GroupCommitCoordinator::with_defaults();
        assert_eq!(coord.max_delay(), Duration::from_millis(10));
        // Can't easily test max_batch_size without registering 200 transactions
    }
}
