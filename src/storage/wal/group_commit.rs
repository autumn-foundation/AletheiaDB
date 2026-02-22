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

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::core::error::{Error, StorageError};

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
    /// Maximum number of recent errors to keep in history.
    pub recent_errors_capacity: usize,
}

impl Default for GroupCommitConfig {
    fn default() -> Self {
        Self {
            max_delay_ms: 10,
            max_batch_size: 200,
            timeout_multiplier: 50,
            timeout_base_ms: 5000,
            timeout_min_ms: 10000,
            timeout_max_ms: 60000,
            recent_errors_capacity: 1024,
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
    /// Recent flush errors, stored as (epoch, error_message).
    /// Used to verify that a specific epoch was successfully flushed.
    recent_errors: VecDeque<(u64, String)>,
    /// The oldest epoch in the recent_errors list (or older if evicted).
    /// Used to detect if we've lost history.
    oldest_error_epoch: u64,
}

impl GroupCommitCoordinator {
    /// Create a new GroupCommitCoordinator with the given configuration.
    pub fn new(max_delay_ms: u64, max_batch_size: usize) -> Self {
        Self::with_config(GroupCommitConfig {
            max_delay_ms,
            max_batch_size,
            recent_errors_capacity: 1024,
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
                recent_errors: VecDeque::new(),
                oldest_error_epoch: 0,
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
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    pub fn register_transaction(&self) -> Result<(u64, bool), Error> {
        let mut state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;

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
        let mut state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;

        // Deadlock detection timeout (NOT a performance SLA)
        let base_timeout =
            Duration::from_millis(self.config.max_delay_ms * self.config.timeout_multiplier as u64)
                + Duration::from_millis(self.config.timeout_base_ms);
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
                .map_err(|_| {
                    Error::Storage(StorageError::LockPoisoned {
                        resource: "group_commit_state".to_string(),
                    })
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

        // Check for flush errors specifically for this epoch
        for (failed_epoch, error_msg) in &state.recent_errors {
            if *failed_epoch == epoch {
                return Err(Error::Storage(StorageError::WalError {
                    reason: format!("Group commit flush failed: {}", error_msg),
                }));
            }
        }

        // Check for history eviction (False Success protection)
        // We only lose certainty about an epoch's status if its error record was EVICTED.
        // state.oldest_error_epoch tracks the threshold of lost history.
        // If epoch < state.oldest_error_epoch, it might have failed and been forgotten.
        //
        // Note: We do NOT check against recent_errors.front() because a sparse error list
        // is valid. If epoch 1 succeeded and epoch 2 failed, recent_errors = [(2, ...)].
        // epoch 1 < 2, but it shouldn't fail unless 1 < oldest_error_epoch.
        // See regression test: tests/regressions/wal_group_commit_epochs.rs

        if epoch < state.oldest_error_epoch {
            return Err(Error::Storage(StorageError::WalError {
                reason: format!(
                    "Group commit status unknown: epoch {} evicted from error history (history starts at {})",
                    epoch, state.oldest_error_epoch
                ),
            }));
        }

        Ok(())
    }

    /// Start a flush operation.
    ///
    /// This method MUST be called before the actual flush begins. It advances
    /// the `current_epoch` so that any new transactions registering during the
    /// flush operation are assigned to the *next* epoch, preventing race conditions
    /// where late-arriving transactions are marked as flushed without actually being written.
    ///
    /// # Returns
    ///
    /// The epoch number that is being flushed.
    pub fn start_flush(&self) -> Result<u64, Error> {
        let mut state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;

        let epoch_to_flush = state.current_epoch;

        // Advance current epoch immediately so new transactions go to the next one
        state.current_epoch += 1;
        state.batch_count = 0;

        Ok(epoch_to_flush)
    }

    /// Finish a flush operation and notify waiters.
    ///
    /// Called by the flush thread after completing a flush.
    ///
    /// # Arguments
    ///
    /// * `epoch` - The epoch that was flushed (returned by `start_flush`).
    /// * `result` - The result of the flush operation.
    pub fn finish_flush(&self, epoch: u64, result: Result<(), Error>) -> Result<(), Error> {
        let mut state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;

        // Store error if any
        if let Err(e) = result {
            state.recent_errors.push_back((epoch, e.to_string()));

            // Keep history limited
            while state.recent_errors.len() > self.config.recent_errors_capacity {
                if let Some((evicted_epoch, _)) = state.recent_errors.pop_front() {
                    // Track the newest evicted epoch to know what we've lost
                    // We set oldest_error_epoch to evicted_epoch + 1 because if
                    // evicted_epoch is gone, any check for it (or older) is invalid.
                    state.oldest_error_epoch = evicted_epoch + 1;
                }
            }
        }

        // Advance flushed_epoch to wake up waiters for this epoch
        // Note: We use max to handle potential out-of-order completions if we ever support that,
        // though currently flushes are serialized.
        if epoch > state.flushed_epoch {
            state.flushed_epoch = epoch;
        }

        // Wake all waiting transactions
        self.flush_complete.notify_all();

        Ok(())
    }

    /// Mark the current batch as flushed (Legacy/Combined Helper).
    ///
    /// WARNING: This method combines `start_flush` and `finish_flush` but IS NOT SAFE
    /// against the race condition described in security review (transactions registering
    /// during the flush).
    ///
    /// It is kept for backward compatibility with existing tests but `start_flush`
    /// and `finish_flush` should be used in production.
    pub fn mark_flushed(&self, result: Result<(), Error>) -> Result<(), Error> {
        let epoch = self.start_flush()?;
        self.finish_flush(epoch, result)
    }

    /// Get the current batch size.
    ///
    /// Useful for monitoring and testing.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    pub fn current_batch_size(&self) -> Result<usize, Error> {
        let state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;
        Ok(state.batch_count)
    }

    /// Get the current epoch.
    ///
    /// Useful for monitoring and testing.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    pub fn current_epoch(&self) -> Result<u64, Error> {
        let state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;
        Ok(state.current_epoch)
    }

    /// Get the last flushed epoch.
    ///
    /// Useful for monitoring and testing.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    pub fn flushed_epoch(&self) -> Result<u64, Error> {
        let state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;
        Ok(state.flushed_epoch)
    }

    /// Check if a flush should be triggered based on batch size.
    ///
    /// Called by the flush thread to check if the batch is full.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    pub fn should_flush(&self) -> Result<bool, Error> {
        let state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;
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
        // Use a config with very short timeout for testing
        let config = GroupCommitConfig {
            max_delay_ms: 10,
            max_batch_size: 100,
            timeout_multiplier: 2, // 10 * 2 = 20ms
            timeout_base_ms: 10,   // + 10 = 30ms
            timeout_min_ms: 20,    // clamp min
            timeout_max_ms: 100,   // clamp max
            recent_errors_capacity: 1024,
        };
        let coord = GroupCommitCoordinator::with_config(config);

        let (epoch, _) = coord.register_transaction().unwrap();

        // Wait without anyone calling mark_flushed - should timeout quickly
        let start = std::time::Instant::now();
        let result = coord.wait_for_flush(epoch);
        let elapsed = start.elapsed();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("timeout"));

        // Verify it didn't take 10 seconds (default timeout)
        assert!(elapsed < Duration::from_millis(500));
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
            recent_errors_capacity: 1024,
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
        // We use a relaxed lower bound (5ms) to handle Windows CI scheduler jitter.
        assert!(elapsed >= Duration::from_millis(5));
        assert!(elapsed < Duration::from_millis(500));
    }

    #[test]
    fn test_error_history_eviction() {
        // Config with very small history
        let config = GroupCommitConfig {
            max_delay_ms: 10,
            max_batch_size: 100,
            timeout_multiplier: 2,
            timeout_base_ms: 10,
            timeout_min_ms: 20,
            timeout_max_ms: 1000,
            recent_errors_capacity: 2, // Only keep 2 recent errors
        };
        let coord = GroupCommitCoordinator::with_config(config);

        // Epoch 1 fails
        let epoch1 = coord.start_flush().unwrap();
        coord
            .finish_flush(
                epoch1,
                Err(Error::Storage(StorageError::WalError {
                    reason: "Fail 1".to_string(),
                })),
            )
            .unwrap();

        // Epoch 2 fails
        let epoch2 = coord.start_flush().unwrap();
        coord
            .finish_flush(
                epoch2,
                Err(Error::Storage(StorageError::WalError {
                    reason: "Fail 2".to_string(),
                })),
            )
            .unwrap();

        // Epoch 3 fails (Evicts Epoch 1)
        let epoch3 = coord.start_flush().unwrap();
        coord
            .finish_flush(
                epoch3,
                Err(Error::Storage(StorageError::WalError {
                    reason: "Fail 3".to_string(),
                })),
            )
            .unwrap();

        // Check Epoch 1 - Should be unknown/evicted
        let result1 = coord.wait_for_flush(epoch1);
        assert!(result1.is_err());
        assert!(
            result1
                .unwrap_err()
                .to_string()
                .contains("evicted from error history")
        );

        // Check Epoch 2 - Should be known error
        let result2 = coord.wait_for_flush(epoch2);
        assert!(result2.is_err());
        assert!(result2.unwrap_err().to_string().contains("Fail 2"));

        // Check Epoch 3 - Should be known error
        let result3 = coord.wait_for_flush(epoch3);
        assert!(result3.is_err());
        assert!(result3.unwrap_err().to_string().contains("Fail 3"));
    }

    #[test]
    fn test_flush_race_condition() {
        let coord = GroupCommitCoordinator::new(100, 100);

        // 1. Transaction A registers (Epoch 1)
        let (epoch_a, _) = coord.register_transaction().unwrap();
        assert_eq!(epoch_a, 1);

        // 2. Flush starts (Epoch 1 is being flushed)
        // This advances current_epoch to 2
        let flushing_epoch = coord.start_flush().unwrap();
        assert_eq!(flushing_epoch, 1);
        assert_eq!(coord.current_epoch().unwrap(), 2);

        // 3. Transaction B registers (Should be Epoch 2)
        // Because start_flush advanced the epoch, B is not part of the current flush
        let (epoch_b, _) = coord.register_transaction().unwrap();
        assert_eq!(epoch_b, 2);

        // 4. Flush finishes successfully
        coord.finish_flush(flushing_epoch, Ok(())).unwrap();

        // 5. Transaction A should be done
        assert!(coord.wait_for_flush(epoch_a).is_ok());

        // 6. Transaction B should NOT be done (it needs Epoch 2 flush)
        // We expect it to timeout if we wait, but we can just check flushed_epoch
        assert_eq!(coord.flushed_epoch().unwrap(), 1);

        // 7. Flush Epoch 2
        let flushing_epoch_2 = coord.start_flush().unwrap();
        assert_eq!(flushing_epoch_2, 2);
        coord.finish_flush(flushing_epoch_2, Ok(())).unwrap();

        // 8. Transaction B should be done
        assert!(coord.wait_for_flush(epoch_b).is_ok());
    }
}
