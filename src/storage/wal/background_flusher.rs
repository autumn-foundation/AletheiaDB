//! Background flush logic for Concurrent WAL System.
//!
//! This module contains the `BackgroundFlusher` struct and the `FlushNotifier` helper,
//! extracted from `concurrent_system.rs` to reduce file size and complexity.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use super::concurrent::ConcurrentWal;
use super::flush_coordinator::FlushCoordinator;
use super::group_commit::GroupCommitCoordinator;
use crate::core::error::Result;

/// Signal for waking up the flush thread when batch is full.
pub(crate) struct FlushNotifier {
    /// Lock for condvar.
    lock: Mutex<bool>,
    /// Condvar to signal immediate flush.
    condvar: Condvar,
}

impl FlushNotifier {
    pub(crate) fn new() -> Self {
        Self {
            lock: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    /// Signal the flush thread to wake up immediately.
    pub(crate) fn notify(&self) {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *guard = true;
        self.condvar.notify_one();
    }

    /// Wait for a signal or timeout, returns true if signaled.
    pub(crate) fn wait_timeout(&self, duration: Duration) -> bool {
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
pub(crate) const FLUSH_ERROR_WARNING_THRESHOLD: u64 = 3;

/// Helper struct to encapsulate background flush logic.
pub(crate) struct BackgroundFlusher {
    pub(crate) wal: Arc<ConcurrentWal>,
    pub(crate) coordinator: Arc<FlushCoordinator>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) flush_notifier: Arc<FlushNotifier>,
    pub(crate) group_commit: Option<Arc<GroupCommitCoordinator>>,
    pub(crate) error_counter: Arc<AtomicU64>,
    pub(crate) interval: Duration,
    pub(crate) sync_on_flush: bool,
}

impl BackgroundFlusher {
    pub(crate) fn run(&self) {
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
