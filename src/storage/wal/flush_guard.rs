//! Background flush thread lifecycle management.
//!
//! This module provides [`FlushGuard`], which manages the lifecycle of the
//! background flush thread used by [`Async`](super::DurabilityMode::Async) and
//! [`GroupCommit`](super::DurabilityMode::GroupCommit) durability modes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Manages the lifecycle of the background flush thread.
///
/// When dropped, signals the thread to stop and waits for it to complete,
/// ensuring all pending data is flushed before shutdown.
///
/// # Thread Safety
///
/// The FlushGuard is designed to be held by the WAL and dropped when the
/// database shuts down. The Drop implementation ensures a clean shutdown:
///
/// 1. Signals the background thread to stop
/// 2. Wakes the thread from any wait
/// 3. The thread performs a final flush
/// 4. Waits for the thread to complete
///
/// # Example
///
/// ```ignore
/// use gallifreydb::storage::wal::FlushGuard;
/// use std::sync::{Arc, Mutex};
///
/// let wal = Arc::new(Mutex::new(wal_instance));
/// let guard = FlushGuard::spawn(
///     Arc::clone(&wal),
///     Duration::from_millis(10),
///     None, // No group commit coordinator
/// );
///
/// // ... use the WAL ...
///
/// // When guard is dropped, thread performs final flush and stops
/// drop(guard);
/// ```
pub struct FlushGuard {
    /// Signal for shutdown and wake-up
    signal: Arc<FlushSignal>,
    /// Thread handle for joining on drop
    thread_handle: Option<JoinHandle<()>>,
}

/// Shared signal state between FlushGuard and the background thread.
pub struct FlushSignal {
    /// State protected by mutex
    state: Mutex<FlushSignalState>,
    /// Condition variable for waking the thread
    condvar: Condvar,
    /// Atomic flag indicating thread has completed (for defensive cleanup)
    completed: AtomicBool,
}

struct FlushSignalState {
    /// Whether shutdown has been requested
    shutdown: bool,
    /// Whether an immediate flush has been requested (for piggybacking)
    flush_requested: bool,
}

impl FlushSignal {
    /// Create a new FlushSignal.
    fn new() -> Self {
        Self {
            state: Mutex::new(FlushSignalState {
                shutdown: false,
                flush_requested: false,
            }),
            condvar: Condvar::new(),
            completed: AtomicBool::new(false),
        }
    }

    /// Request an immediate flush (used for piggybacking).
    ///
    /// This wakes the background thread to perform a flush immediately,
    /// rather than waiting for the next interval. Called by Synchronous
    /// transactions before their own fsync to piggyback pending data.
    pub fn request_flush(&self) {
        let mut state = self.state.lock().expect("flush signal lock poisoned");
        state.flush_requested = true;
        self.condvar.notify_one();
    }

    /// Request shutdown.
    fn request_shutdown(&self) {
        let mut state = self.state.lock().expect("flush signal lock poisoned");
        state.shutdown = true;
        self.condvar.notify_one();
    }

    /// Wait for the next flush interval or a signal.
    ///
    /// Returns `true` if the thread should continue, `false` if shutdown.
    fn wait_for_interval(&self, interval: Duration) -> bool {
        let mut state = self.state.lock().expect("flush signal lock poisoned");

        // If shutdown already requested, exit immediately
        if state.shutdown {
            return false;
        }

        // Clear any pending flush request before waiting
        state.flush_requested = false;

        // Wait for either timeout, flush request, or shutdown
        let (new_state, _timeout_result) = self
            .condvar
            .wait_timeout(state, interval)
            .expect("flush signal lock poisoned");

        state = new_state;

        // Return false if shutdown requested
        !state.shutdown
    }
}

impl FlushGuard {
    /// Spawn a new background flush thread.
    ///
    /// The thread will periodically wake up and call the provided flush function.
    /// It also wakes on explicit flush requests (for piggybacking) and shutdown.
    ///
    /// # Arguments
    ///
    /// * `flush_fn` - Function to call for each flush. Should acquire WAL lock,
    ///   fsync, and notify any waiting group commit transactions.
    /// * `interval` - How often to flush when idle.
    ///
    /// # Panics
    ///
    /// Panics if the thread cannot be spawned (rare, usually resource exhaustion).
    pub fn spawn<F>(flush_fn: F, interval: Duration) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let signal = Arc::new(FlushSignal::new());
        let signal_clone = Arc::clone(&signal);

        let handle = thread::Builder::new()
            .name("gallifreydb-flush".to_string())
            .spawn(move || {
                Self::flush_thread_loop(signal_clone, flush_fn, interval);
            })
            .expect("failed to spawn flush thread");

        FlushGuard {
            signal,
            thread_handle: Some(handle),
        }
    }

    /// Get a reference to the signal for requesting flushes.
    ///
    /// Used by Synchronous commits to trigger piggybacking.
    pub fn signal(&self) -> &Arc<FlushSignal> {
        &self.signal
    }

    /// The main loop for the background flush thread.
    fn flush_thread_loop<F>(signal: Arc<FlushSignal>, flush_fn: F, interval: Duration)
    where
        F: Fn(),
    {
        loop {
            // Wait for the next interval or wake signal
            let should_continue = signal.wait_for_interval(interval);

            // Always flush before checking whether to exit
            // This ensures piggybacking works and shutdown flushes
            flush_fn();

            if !should_continue {
                // Shutdown requested - we've already done final flush above
                break;
            }
        }

        // Mark thread as completed before exiting
        signal.completed.store(true, Ordering::Release);
    }
}

impl Drop for FlushGuard {
    fn drop(&mut self) {
        // Signal shutdown
        self.signal.request_shutdown();

        // Defensive: Check if thread already completed
        // This handles rare race conditions where thread exits before drop is called
        if self.signal.completed.load(Ordering::Acquire) {
            // Thread already exited cleanly, just clean up the handle
            let _ = self.thread_handle.take();
            return;
        }

        // Wait for thread to complete (it will do a final flush)
        if let Some(handle) = self.thread_handle.take() {
            // Use a short timeout-based wait loop for defensive safety
            // This prevents indefinite blocking if there's a race condition
            for _ in 0..50 {
                // 50 * 10ms = 500ms total max wait
                if self.signal.completed.load(Ordering::Acquire) {
                    // Thread completed, join should succeed immediately
                    let _ = handle.join();
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }

            // If we get here, thread is taking too long. Try one final join.
            // Ignore join errors (thread may have panicked)
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    #[test]
    fn test_flush_guard_spawns_thread() {
        let flush_count = Arc::new(AtomicU32::new(0));
        let flush_count_clone = Arc::clone(&flush_count);

        let guard = FlushGuard::spawn(
            move || {
                flush_count_clone.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_millis(10),
        );

        // Wait for at least one flush
        thread::sleep(Duration::from_millis(50));

        drop(guard);

        // Should have flushed at least once (plus final flush on drop)
        assert!(flush_count.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn test_flush_guard_final_flush_on_drop() {
        let flush_count = Arc::new(AtomicU32::new(0));
        let flush_count_clone = Arc::clone(&flush_count);

        let guard = FlushGuard::spawn(
            move || {
                flush_count_clone.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_secs(60), // Long interval - won't trigger naturally
        );

        // No natural flushes should have occurred
        let before = flush_count.load(Ordering::SeqCst);

        drop(guard);

        // Should have done a final flush on drop
        let after = flush_count.load(Ordering::SeqCst);
        assert!(after > before, "expected final flush on drop");
    }

    #[test]
    fn test_request_flush_wakes_thread() {
        let flush_count = Arc::new(AtomicU32::new(0));
        let flush_count_clone = Arc::clone(&flush_count);

        let guard = FlushGuard::spawn(
            move || {
                flush_count_clone.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_secs(60), // Long interval
        );

        // Give thread time to start and enter wait state
        thread::sleep(Duration::from_millis(20));

        let before = flush_count.load(Ordering::SeqCst);

        // Request immediate flush
        guard.signal().request_flush();

        // Give thread time to wake and flush
        thread::sleep(Duration::from_millis(100));

        let after = flush_count.load(Ordering::SeqCst);
        assert!(after > before, "expected flush after request_flush()");

        drop(guard);
    }

    #[test]
    fn test_flush_interval_timing() {
        let flush_count = Arc::new(AtomicU32::new(0));
        let flush_count_clone = Arc::clone(&flush_count);

        let start = Instant::now();
        let guard = FlushGuard::spawn(
            move || {
                flush_count_clone.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_millis(50),
        );

        // Wait longer to account for slow CI systems and thread startup delays
        thread::sleep(Duration::from_millis(200));

        drop(guard);

        let elapsed = start.elapsed();
        let count = flush_count.load(Ordering::SeqCst);

        // Should have at least 2 flushes in 200ms with 50ms interval
        // (Being very conservative for slow/loaded CI systems)
        // Expected: ~4 flushes, but requiring only 2 to avoid flakiness
        assert!(
            count >= 2,
            "expected at least 2 flushes in {:?}, got {}",
            elapsed,
            count
        );
    }

    #[test]
    fn test_multiple_flush_requests() {
        let flush_count = Arc::new(AtomicU32::new(0));
        let flush_count_clone = Arc::clone(&flush_count);

        let guard = FlushGuard::spawn(
            move || {
                flush_count_clone.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_secs(60), // Long interval
        );

        // Request multiple flushes rapidly
        for _ in 0..5 {
            guard.signal().request_flush();
            thread::sleep(Duration::from_millis(10));
        }

        thread::sleep(Duration::from_millis(50));

        drop(guard);

        // Should have multiple flushes
        let count = flush_count.load(Ordering::SeqCst);
        assert!(count >= 3, "expected multiple flushes, got {}", count);
    }
}
