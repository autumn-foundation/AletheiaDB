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

use std::collections::{BTreeSet, VecDeque};
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
/// # Why?
///
/// Group commit is a crucial performance optimization for a database's Write-Ahead Log.
/// Fsync operations on modern NVMe drives can still take ~1-2ms. If every transaction
/// synchronously blocks on an fsync, throughput is capped at ~500-1000 tx/sec. By
/// batching multiple concurrent transactions into a single "epoch" and performing one
/// fsync for all of them, throughput can scale to hundreds of thousands of tx/sec while
/// still maintaining strict ACID durability.
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
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
/// use std::sync::Arc;
/// use std::thread;
///
/// let coordinator = Arc::new(GroupCommitCoordinator::new(10, 100));
///
/// // Transaction 1
/// let (epoch1, _) = coordinator.register_transaction().unwrap();
///
/// // Background Flush Thread
/// let flush_coord = Arc::clone(&coordinator);
/// thread::spawn(move || {
///     let epoch = flush_coord.start_flush().unwrap();
///     // Flush writes to disk here...
///     // Then notify waiting transactions
///     flush_coord.finish_flush(epoch, Ok(())).unwrap();
/// });
///
/// // Transaction 1 waits for flush
/// coordinator.wait_for_flush(epoch1).unwrap();
/// ```
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
    /// Set of completed epochs that are ahead of flushed_epoch
    completed_epochs: BTreeSet<u64>,
}

impl GroupCommitCoordinator {
    /// Create a new GroupCommitCoordinator with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `max_delay_ms` - Maximum time to wait for more transactions before flushing.
    /// * `max_batch_size` - Maximum transactions to batch before forcing a flush.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// // Batches up to 100 transactions, waiting at most 10ms
    /// let coordinator = GroupCommitCoordinator::new(10, 100);
    /// ```
    pub fn new(max_delay_ms: u64, max_batch_size: usize) -> Self {
        Self::with_config(GroupCommitConfig {
            max_delay_ms,
            max_batch_size,
            recent_errors_capacity: 1024,
            ..GroupCommitConfig::default()
        })
    }

    /// Create a new GroupCommitCoordinator with the given full configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::{GroupCommitCoordinator, GroupCommitConfig};
    ///
    /// let config = GroupCommitConfig {
    ///     max_delay_ms: 10,
    ///     max_batch_size: 100,
    ///     ..GroupCommitConfig::default()
    /// };
    /// let coordinator = GroupCommitCoordinator::with_config(config);
    /// ```
    pub fn with_config(config: GroupCommitConfig) -> Self {
        Self {
            state: Mutex::new(GroupCommitState {
                current_epoch: 1, // Start at 1 so flushed_epoch=0 means "nothing flushed yet"
                batch_count: 0,
                flushed_epoch: 0,
                recent_errors: VecDeque::new(),
                oldest_error_epoch: 0,
                completed_epochs: BTreeSet::new(),
            }),
            flush_complete: Condvar::new(),
            config,
        }
    }

    /// Create a new GroupCommitCoordinator with default configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    /// # let coordinator = GroupCommitCoordinator::new(10, 100);
    /// # let (epoch, _) = coordinator.register_transaction().unwrap();
    /// coordinator.wait_for_flush(epoch).unwrap();
    /// ```
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

        // Use a deadline to prevent spurious wakeups from resetting the timeout clock
        let deadline = std::time::Instant::now() + timeout;

        // RACE CONDITION SAFETY: If the epoch was already flushed between register_transaction()
        // and this wait (rare but possible on fast systems), this loop exits immediately since
        // flushed_epoch >= epoch, and we return Ok(()) without waiting.
        //
        // EPOCH SEMANTICS: flushed_epoch = N means "epoch N has been flushed".
        // Transaction at epoch E waits while flushed_epoch < E (i.e., E has not been flushed yet).
        while state.flushed_epoch < epoch {
            let now = std::time::Instant::now();
            let remaining = if now >= deadline {
                Duration::from_secs(0)
            } else {
                deadline - now
            };

            if remaining.as_nanos() == 0 {
                return Err(Error::Storage(StorageError::WalError {
                    reason: format!(
                        "Group commit timeout waiting for epoch {} (current flushed: {})",
                        epoch, state.flushed_epoch
                    ),
                }));
            }

            let (new_state, timeout_result) = self
                .flush_complete
                .wait_timeout(state, remaining)
                .map_err(|_| {
                    Error::Storage(StorageError::LockPoisoned {
                        resource: "group_commit_state".to_string(),
                    })
                })?;

            state = new_state;

            // Check if we timed out (either by Condvar result OR by deadline)
            if (timeout_result.timed_out() || std::time::Instant::now() >= deadline)
                && state.flushed_epoch < epoch
            {
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

        // Mark this epoch as completed
        if epoch > state.flushed_epoch {
            state.completed_epochs.insert(epoch);
        }

        // Advance flushed_epoch contiguously to wake up waiters
        // We only advance if we have a contiguous sequence of completed epochs.
        // This prevents data loss scenarios where a later successful flush
        // could mask an earlier failed (or still pending) flush.
        let mut next_epoch = state.flushed_epoch + 1;
        while state.completed_epochs.contains(&next_epoch) {
            state.completed_epochs.remove(&next_epoch);
            state.flushed_epoch = next_epoch;
            next_epoch += 1;
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
    /// Useful for monitoring and testing to see how many transactions are currently
    /// waiting in the active epoch to be flushed.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let size = coordinator.current_batch_size().unwrap();
    /// assert_eq!(size, 0);
    /// ```
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
    /// Useful for monitoring and testing to determine which epoch incoming
    /// transactions will be assigned to.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let epoch = coordinator.current_epoch().unwrap();
    /// assert_eq!(epoch, 1); // Starts at 1
    /// ```
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
    /// Useful for monitoring and testing to see how far along the durability
    /// frontier has advanced.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let flushed = coordinator.flushed_epoch().unwrap();
    /// assert_eq!(flushed, 0); // Nothing flushed yet
    /// ```
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
    /// Called by the flush thread to check if the batch is full and should be flushed
    /// immediately, ignoring the `max_delay_ms` timeout.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let flush_now = coordinator.should_flush().unwrap();
    /// assert_eq!(flush_now, false);
    /// ```
    pub fn should_flush(&self) -> Result<bool, Error> {
        let state = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;
        Ok(state.batch_count >= self.config.max_batch_size)
    }

    /// Get the maximum delay for this coordinator.
    ///
    /// Returns the configured `max_delay_ms` as a `Duration`.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    /// use std::time::Duration;
    ///
    /// let coordinator = GroupCommitCoordinator::new(10, 100);
    /// assert_eq!(coordinator.max_delay(), Duration::from_millis(10));
    /// ```
    pub fn max_delay(&self) -> Duration {
        Duration::from_millis(self.config.max_delay_ms)
    }
}

#[cfg(test)]
mod tests;
