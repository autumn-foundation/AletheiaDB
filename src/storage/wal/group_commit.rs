//! Group commit coordination for batched WAL flushes.
//!
//! This module provides [`GroupCommitCoordinator`], which manages the epoch-based
//! waiting mechanism for [`GroupCommit`](super::DurabilityMode::GroupCommit) mode.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::utils::lock::MutexExt;
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
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock was poisoned
    /// by a panicking thread. This indicates the coordinator is in an inconsistent
    /// state and cannot safely coordinate commits.
    pub fn register_transaction(&self) -> Result<(u64, bool), Error> {
        let mut state = self.state.lock_or_err()?;

        state.batch_count += 1;
        let epoch = state.current_epoch;

        // Check if we should trigger immediate flush (batch full)
        let should_flush = state.batch_count >= self.config.max_batch_size;

        Ok((epoch, should_flush))
    }

    /// Wait for the specified epoch to be flushed.
    ///
    /// This blocks until the epoch has been durably flushed. If the flush
    /// failed, the error is propagated to all waiting transactions.
    ///
    /// # Timeout Calculation
    ///
    /// The timeout is a **deadlock detection mechanism**, not a performance target.
    /// It's designed to catch stuck flush threads, not to enforce timing.
    ///
    /// Formula: `max(max_delay_ms * 2 + 500ms, 500ms)` with cap at 30s
    /// - 2x multiplier: Allows for one full flush cycle plus margin
    /// - +500ms: Fixed overhead for fsync and thread scheduling
    /// - Minimum 500ms: Handles very fast configs (e.g., 1ms) in slow CI
    /// - Maximum 30s: Prevents indefinite waiting on stuck threads
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The flush for this epoch failed
    /// - The wait times out (indicates stuck flush thread)
    pub fn wait_for_flush(&self, epoch: u64) -> Result<(), Error> {
        let mut state = self.state.lock_or_err()?;

        // Deadlock detection timeout (NOT a performance SLA)
        // Must be longer than max_delay_ms to allow at least one flush cycle
        let base_timeout =
            Duration::from_millis(self.config.max_delay_ms * 2) + Duration::from_millis(500);
        let timeout = base_timeout
            .max(Duration::from_millis(500))
            .min(Duration::from_secs(30));

        // RACE CONDITION SAFETY: If the epoch was already flushed between register_transaction()
        // and this wait (rare but possible on fast systems), this loop exits immediately since
        // flushed_epoch > epoch, and we return Ok(()) without waiting.
        while state.flushed_epoch <= epoch {
            let (new_state, timeout_result) = self
                .flush_complete
                .wait_timeout(state, timeout)
                .map_err(|_| StorageError::LockPoisoned {
                    lock_type: "Condvar",
                })?;

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
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock was poisoned
    /// by a panicking thread.
    pub fn mark_flushed(&self, result: Result<(), Error>) -> Result<(), Error> {
        let mut state = self.state.lock_or_err()?;

        // Store any error for propagation
        state.last_flush_error = result.err().map(|e| e.to_string());

        // NOTE: Observability metrics removed to prevent log spam in CI.
        // See GitHub issue #274 for tracking proper observability implementation
        // with rate limiting and filtering of uninteresting events.

        // Advance the flushed epoch (even on error, so waiters wake up)
        state.flushed_epoch = state.current_epoch + 1;
        state.current_epoch += 1;
        state.batch_count = 0;

        // Wake all waiting transactions
        self.flush_complete.notify_all();

        Ok(())
    }

    /// Get the current batch size.
    ///
    /// Useful for monitoring and testing.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock was poisoned.
    pub fn current_batch_size(&self) -> Result<usize, Error> {
        Ok(self.state.lock_or_err()?.batch_count)
    }

    /// Get the current epoch.
    ///
    /// Useful for monitoring and testing.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock was poisoned.
    pub fn current_epoch(&self) -> Result<u64, Error> {
        Ok(self.state.lock_or_err()?.current_epoch)
    }

    /// Get the last flushed epoch.
    ///
    /// Useful for monitoring and testing.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock was poisoned.
    pub fn flushed_epoch(&self) -> Result<u64, Error> {
        Ok(self.state.lock_or_err()?.flushed_epoch)
    }

    /// Check if a flush should be triggered based on batch size.
    ///
    /// Called by the flush thread to check if the batch is full.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock was poisoned.
    pub fn should_flush(&self) -> Result<bool, Error> {
        let state = self.state.lock_or_err()?;
        Ok(state.batch_count >= self.config.max_batch_size)
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

    // ==================== Test Helpers for Lock Poisoning ====================

    /// Poison the coordinator's internal lock by panicking while holding it.
    /// After this function returns, any lock acquisition will fail with PoisonError.
    fn poison_coordinator_lock(coord: &Arc<GroupCommitCoordinator>) {
        let coord_clone = Arc::clone(coord);
        let handle = thread::spawn(move || {
            let _guard = coord_clone.state.lock().unwrap();
            panic!("intentional panic to poison lock");
        });
        // Thread panicked, so join returns Err - this is expected
        assert!(handle.join().is_err(), "Poisoning thread should panic");
    }

    /// Assert that a Result contains a LockPoisoned error with the expected lock_type.
    fn assert_is_lock_poisoned<T: std::fmt::Debug>(result: Result<T, Error>, expected_type: &str) {
        assert!(result.is_err(), "Expected error, got {:?}", result);
        if let Err(Error::Storage(StorageError::LockPoisoned { lock_type })) = result {
            assert_eq!(lock_type, expected_type);
        } else {
            panic!(
                "Expected StorageError::LockPoisoned {{ lock_type: \"{}\" }}, got {:?}",
                expected_type,
                result.err()
            );
        }
    }

    // ==================== TDD Tests for Lock Poisoning Error Handling ====================
    // These tests verify that methods return proper errors instead of panicking
    // when mutex locks are poisoned.

    #[test]
    fn test_register_transaction_returns_result() {
        let coord = GroupCommitCoordinator::new(10, 200);
        let result = coord.register_transaction();
        assert!(result.is_ok());
        let (epoch, should_flush) = result.unwrap();
        assert_eq!(epoch, 0);
        assert!(!should_flush);
    }

    #[test]
    fn test_register_transaction_with_poisoned_lock() {
        let coord = Arc::new(GroupCommitCoordinator::new(10, 200));
        poison_coordinator_lock(&coord);
        assert_is_lock_poisoned(coord.register_transaction(), "Mutex");
    }

    #[test]
    fn test_mark_flushed_returns_result() {
        let coord = GroupCommitCoordinator::new(10, 100);
        let _ = coord.register_transaction().unwrap();
        let result = coord.mark_flushed(Ok(()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_mark_flushed_with_poisoned_lock() {
        let coord = Arc::new(GroupCommitCoordinator::new(10, 200));
        poison_coordinator_lock(&coord);
        assert_is_lock_poisoned(coord.mark_flushed(Ok(())), "Mutex");
    }

    #[test]
    fn test_current_batch_size_returns_result() {
        let coord = GroupCommitCoordinator::new(10, 200);
        let result = coord.current_batch_size();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_current_batch_size_with_poisoned_lock() {
        let coord = Arc::new(GroupCommitCoordinator::new(10, 200));
        poison_coordinator_lock(&coord);
        assert_is_lock_poisoned(coord.current_batch_size(), "Mutex");
    }

    #[test]
    fn test_current_epoch_returns_result() {
        let coord = GroupCommitCoordinator::new(10, 200);
        let result = coord.current_epoch();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_current_epoch_with_poisoned_lock() {
        let coord = Arc::new(GroupCommitCoordinator::new(10, 200));
        poison_coordinator_lock(&coord);
        assert_is_lock_poisoned(coord.current_epoch(), "Mutex");
    }

    #[test]
    fn test_flushed_epoch_returns_result() {
        let coord = GroupCommitCoordinator::new(10, 200);
        let result = coord.flushed_epoch();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_flushed_epoch_with_poisoned_lock() {
        let coord = Arc::new(GroupCommitCoordinator::new(10, 200));
        poison_coordinator_lock(&coord);
        assert_is_lock_poisoned(coord.flushed_epoch(), "Mutex");
    }

    #[test]
    fn test_should_flush_returns_result() {
        let coord = GroupCommitCoordinator::new(10, 200);
        let result = coord.should_flush();
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_should_flush_with_poisoned_lock() {
        let coord = Arc::new(GroupCommitCoordinator::new(10, 200));
        poison_coordinator_lock(&coord);
        assert_is_lock_poisoned(coord.should_flush(), "Mutex");
    }

    #[test]
    fn test_wait_for_flush_with_poisoned_lock() {
        // Test that wait_for_flush returns an error if the lock is already poisoned on entry.
        // This tests the initial lock_or_err() call, not the condvar wait_timeout path.
        let coord = Arc::new(GroupCommitCoordinator::new(100, 200));

        // Register a transaction first (before poisoning)
        let (epoch, _) = coord.register_transaction().unwrap();

        // Now poison the lock
        poison_coordinator_lock(&coord);

        // wait_for_flush should return error on initial lock acquisition, not panic
        let result = coord.wait_for_flush(epoch);
        assert!(result.is_err());
    }

    // ==================== Original Tests (Updated for Result-based API) ====================

    #[test]
    fn test_new_coordinator() {
        let coord = GroupCommitCoordinator::new(10, 200);
        assert_eq!(coord.current_epoch().unwrap(), 0);
        assert_eq!(coord.flushed_epoch().unwrap(), 0);
        assert_eq!(coord.current_batch_size().unwrap(), 0);
    }

    #[test]
    fn test_register_transaction() {
        let coord = GroupCommitCoordinator::new(10, 5);

        // Register transactions
        for i in 0..4 {
            let (epoch, should_flush) = coord.register_transaction().unwrap();
            assert_eq!(epoch, 0);
            assert!(!should_flush, "should not flush at batch size {}", i + 1);
        }

        // Fifth transaction should trigger flush
        let (epoch, should_flush) = coord.register_transaction().unwrap();
        assert_eq!(epoch, 0);
        assert!(should_flush, "should flush when batch is full");
    }

    #[test]
    fn test_mark_flushed_advances_epoch() {
        let coord = GroupCommitCoordinator::new(10, 100);

        coord.register_transaction().unwrap();
        coord.register_transaction().unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 0);
        assert_eq!(coord.current_batch_size().unwrap(), 2);

        coord.mark_flushed(Ok(())).unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 1);
        assert_eq!(coord.flushed_epoch().unwrap(), 1);
        assert_eq!(coord.current_batch_size().unwrap(), 0);
    }

    #[test]
    fn test_wait_for_flush_success() {
        let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
        let coord_clone = Arc::clone(&coord);

        let (epoch, _) = coord.register_transaction().unwrap();

        // Spawn a thread to mark flushed after a short delay
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            coord_clone.mark_flushed(Ok(())).unwrap();
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

        let (epoch, _) = coord.register_transaction().unwrap();

        // Spawn a thread to mark flushed with error
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            coord_clone
                .mark_flushed(Err(Error::Storage(StorageError::WalError {
                    reason: "disk full".to_string(),
                })))
                .unwrap();
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

        let (epoch, _) = coord.register_transaction().unwrap();

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
            let (epoch, _) = coord.register_transaction().unwrap();
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
        coord.mark_flushed(Ok(())).unwrap();

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
        coord.register_transaction().unwrap();
        coord.register_transaction().unwrap();
        coord.mark_flushed(Ok(())).unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 1);

        // Second batch
        let (epoch, _) = coord.register_transaction().unwrap();
        assert_eq!(epoch, 1);

        coord.mark_flushed(Ok(())).unwrap();
        assert_eq!(coord.current_epoch().unwrap(), 2);
    }

    #[test]
    fn test_should_flush() {
        let coord = GroupCommitCoordinator::new(10, 3);

        assert!(!coord.should_flush().unwrap());

        coord.register_transaction().unwrap();
        assert!(!coord.should_flush().unwrap());

        coord.register_transaction().unwrap();
        assert!(!coord.should_flush().unwrap());

        coord.register_transaction().unwrap();
        assert!(coord.should_flush().unwrap());
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
