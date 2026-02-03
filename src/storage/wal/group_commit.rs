//! Group commit coordination for batched WAL flushes.
//!
//! This module provides [`GroupCommitCoordinator`], which manages the epoch-based
//! waiting mechanism for [`GroupCommit`](super::DurabilityMode::GroupCommit) mode.
//!
//! # Error Handling Strategy
//!
//! All public methods return `Result` types to handle lock poisoning gracefully.
//! Lock poisoning occurs when a thread panics while holding the coordinator's mutex.
//!
//! ## For Callers
//!
//! When a `StorageError::LockPoisoned` error is returned:
//! - **Flush thread**: Should panic immediately. Continuing would leave waiting
//!   transactions hanging indefinitely. This is an unrecoverable state.
//! - **Transaction threads**: Should propagate the error to the caller. The
//!   transaction cannot complete and must be rolled back.
//!
//! ## Rationale
//!
//! Lock poisoning in the coordinator indicates severe corruption. The alternatives
//! are:
//! 1. **Panic everywhere** (too aggressive for transaction threads)
//! 2. **Continue silently** (leaves transactions hanging - worse than panicking)
//! 3. **Return Result** (chosen approach - lets callers decide appropriate action)
//!
//! The flush thread uses `.expect()` to panic on lock poisoning because silent
//! degradation is worse than fail-fast behavior for background infrastructure.

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

    /// Multiplier for max_delay_ms to calculate base timeout.
    pub timeout_multiplier: u32,
    /// Fixed overhead added to base timeout in milliseconds.
    pub timeout_base_ms: u64,
    /// Minimum timeout in milliseconds.
    pub timeout_min_ms: u64,
    /// Maximum timeout in milliseconds.
    pub timeout_max_ms: u64,
}

impl Default for GroupCommitConfig {
    fn default() -> Self {
        Self {
            max_delay_ms: 10,
            max_batch_size: 200,
            timeout_multiplier: 10,
            timeout_base_ms: 200,
            timeout_min_ms: 500,
            timeout_max_ms: 5000,
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
        Self::with_config(GroupCommitConfig {
            max_delay_ms,
            max_batch_size,
            ..GroupCommitConfig::default()
        })
    }

    /// Create a new GroupCommitCoordinator with the given full configuration.
    pub fn with_config(config: GroupCommitConfig) -> Self {
        Self {
            state: Mutex::new(GroupCommitState {
                current_epoch: 1, // Start at 1 so flushed_epoch=0 means "nothing flushed yet"
                batch_count: 0,
                flushed_epoch: 0,
                last_flush_error: None,
            }),
            flush_complete: Condvar::new(),
            config,
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
    /// Formula: `clamp(max_delay_ms * multiplier + base, min, max)`
    /// - multiplier: Allows for thread scheduling overhead
    /// - base: Fixed overhead for thread startup
    /// - min: Handles very fast configs in slow CI
    /// - max: Prevents indefinite waiting on stuck threads
    ///
    /// The timeout accounts for:
    /// - Environments with unpredictable thread scheduling (e.g., CI runners)
    /// - Systems under heavy load causing thread starvation
    /// - Variable I/O latency for disk flushes
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The flush for this epoch failed
    /// - The wait times out (indicates a stuck flush thread or excessive system load)
    pub fn wait_for_flush(&self, epoch: u64) -> Result<(), Error> {
        let mut state = self.state.lock_or_err()?;

        // Deadlock detection timeout (NOT a performance SLA)
        let base_timeout = Duration::from_millis(
            self.config.max_delay_ms * self.config.timeout_multiplier as u64,
        ) + Duration::from_millis(self.config.timeout_base_ms);
        let timeout = base_timeout
            .max(Duration::from_millis(self.config.timeout_min_ms))
            .min(Duration::from_millis(self.config.timeout_max_ms));

        // RACE CONDITION SAFETY: If the epoch was already flushed between register_transaction()
        // and this wait (rare but possible on fast systems), this loop exits immediately since
        // flushed_epoch >= epoch, and we return Ok(()) without waiting.
        //
        // EPOCH SEMANTICS: flushed_epoch = N means "epoch N has been flushed".
        // Transaction at epoch E waits while flushed_epoch < E (i.e., E has not been flushed yet).
        while state.flushed_epoch < epoch {
            let (new_state, timeout_result) = self
                .flush_complete
                .wait_timeout(state, timeout)
                .map_err(|_| StorageError::LockPoisoned {
                    // Note: Condvar::wait_timeout() returns PoisonError when the MUTEX
                    // is poisoned, not the Condvar. Condvars cannot be poisoned.
                    lock_type: "Mutex",
                })?;

            state = new_state;

            if timeout_result.timed_out() && state.flushed_epoch < epoch {
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
        // EPOCH SEMANTICS: flushed_epoch = N means "epoch N has been flushed".
        // We flush the current epoch, then advance to the next epoch for new transactions.
        state.flushed_epoch = state.current_epoch;
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
    //
    // TEST GAP: The wait_timeout() path in wait_for_flush() (line ~164) cannot be
    // reliably tested for poisoning during the actual condvar wait. This would
    // require poisoning the lock WHILE another thread is blocked in wait_timeout(),
    // which is inherently racy. The current test (test_wait_for_flush_with_poisoned_lock)
    // only tests the initial lock acquisition path. The implementation does handle
    // the condvar poisoning case, but it's not covered by tests.

    #[test]
    fn test_register_transaction_returns_result() {
        let coord = GroupCommitCoordinator::new(10, 200);
        let result = coord.register_transaction();
        assert!(result.is_ok());
        let (epoch, should_flush) = result.unwrap();
        assert_eq!(epoch, 1); // Start at epoch 1
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
        assert_eq!(result.unwrap(), 1); // Start at epoch 1
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
        assert_eq!(coord.current_epoch().unwrap(), 1); // Start at 1 (flushed_epoch=0 means nothing flushed)
        assert_eq!(coord.flushed_epoch().unwrap(), 0);
        assert_eq!(coord.current_batch_size().unwrap(), 0);
    }

    #[test]
    fn test_register_transaction() {
        let coord = GroupCommitCoordinator::new(10, 5);

        // Register transactions
        for i in 0..4 {
            let (epoch, should_flush) = coord.register_transaction().unwrap();
            assert_eq!(epoch, 1); // All in epoch 1
            assert!(!should_flush, "should not flush at batch size {}", i + 1);
        }

        // Fifth transaction should trigger flush
        let (epoch, should_flush) = coord.register_transaction().unwrap();
        assert_eq!(epoch, 1); // Still in epoch 1
        assert!(should_flush, "should flush when batch is full");
    }

    #[test]
    fn test_mark_flushed_advances_epoch() {
        let coord = GroupCommitCoordinator::new(10, 100);

        coord.register_transaction().unwrap();
        coord.register_transaction().unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 1); // Start at epoch 1
        assert_eq!(coord.current_batch_size().unwrap(), 2);

        coord.mark_flushed(Ok(())).unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 2); // Advances to epoch 2
        assert_eq!(coord.flushed_epoch().unwrap(), 1); // Epoch 1 has been flushed
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

        // All should be same epoch (epoch 1)
        assert!(epochs.iter().all(|&e| e == 1));

        // Spawn multiple waiting threads
        let mut handles = Vec::new();
        for _ in 0..5 {
            let coord_clone = Arc::clone(&coord);
            handles.push(thread::spawn(move || coord_clone.wait_for_flush(1))); // Wait for epoch 1
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

        // First batch at epoch 1
        coord.register_transaction().unwrap();
        coord.register_transaction().unwrap();
        coord.mark_flushed(Ok(())).unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 2); // Advanced to epoch 2

        // Second batch at epoch 2
        let (epoch, _) = coord.register_transaction().unwrap();
        assert_eq!(epoch, 2);

        coord.mark_flushed(Ok(())).unwrap();
        assert_eq!(coord.current_epoch().unwrap(), 3); // Advanced to epoch 3
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

    #[test]
    fn test_custom_timeout_config() {
        let config = GroupCommitConfig {
            max_delay_ms: 1,
            max_batch_size: 100,
            timeout_multiplier: 2,
            timeout_base_ms: 10,
            timeout_min_ms: 20,
            timeout_max_ms: 100,
        };
        let coord = GroupCommitCoordinator::with_config(config);

        let (epoch, _) = coord.register_transaction().unwrap();

        // Formula: clamp(1 * 2 + 10, 20, 100) = 20ms
        // Wait without anyone calling mark_flushed - should timeout quickly
        let start = std::time::Instant::now();
        let result = coord.wait_for_flush(epoch);
        let elapsed = start.elapsed();

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
        // Should be around 20ms. Let's check it's within a reasonable range.
        // We use a generous upper bound for slow CI.
        assert!(elapsed >= Duration::from_millis(20));
        assert!(elapsed < Duration::from_millis(500));
    }
}
