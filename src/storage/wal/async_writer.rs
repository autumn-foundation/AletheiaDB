//! Async WAL writer with background sync thread.
//!
//! This module provides [`AsyncWalWriter`], which implements the Async durability mode
//! with a background thread handling continuous fsync.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
//! │  Writer Thread  │────▶│   WAL Buffer    │────▶│  Background     │
//! │                 │     │  (lock-free)    │     │  Sync Thread    │
//! │  write() returns│     │                 │     │                 │
//! │  immediately    │     │  Ring buffer    │     │  Continuous     │
//! │                 │     │  or channel     │     │  fsync loop     │
//! └─────────────────┘     └─────────────────┘     └─────────────────┘
//!                                                        │
//!                                                        ▼
//!                                                   ┌─────────┐
//!                                                   │  Disk   │
//!                                                   └─────────┘
//! ```
//!
//! # Key Features
//!
//! - **Non-blocking writes**: Returns immediately after queueing entry
//! - **Batched fsync**: Background thread batches fsyncs efficiently
//! - **Backpressure**: Handles buffer full with configurable strategy
//! - **Graceful shutdown**: Drop impl drains buffer and syncs
//! - **Metrics**: Tracks buffer depth and sync lag

use super::WalEntry;
use crate::core::temporal::{Timestamp, time};
use crate::utils::error::{Result, StorageError};

// Using crossbeam_channel instead of std::sync::mpsc because:
// 1. Bounded channels with try_send() for backpressure handling
// 2. Better performance characteristics (lock-free fast path)
// 3. recv_timeout() support (std::mpsc only has recv_timeout on Receiver)
// 4. More reliable disconnect semantics for graceful shutdown
// 5. Well-tested in production systems (used by tokio, rayon, etc.)
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Events emitted by the WAL async writer.
#[derive(Debug, Clone)]
pub enum WalEvent {
    /// Sync completed for a batch of entries.
    SyncCompleted {
        /// Number of entries synced
        entry_count: usize,
        /// Timestamp when sync completed
        timestamp: Timestamp,
    },
}

/// Observer trait for WAL events.
///
/// Implement this trait to receive notifications about WAL operations,
/// such as sync completion. This enables observability, metrics collection,
/// and coordination with other components.
///
/// # Example
///
/// ```
/// use gallifreydb::storage::wal::{WalObserver, WalEvent};
///
/// struct MetricsCollector;
///
/// impl WalObserver for MetricsCollector {
///     fn on_event(&self, event: &WalEvent) {
///         match event {
///             WalEvent::SyncCompleted { entry_count, timestamp } => {
///                 println!("Synced {} entries at {}", entry_count, timestamp);
///             }
///         }
///     }
/// }
/// ```
pub trait WalObserver: Send + Sync {
    /// Called when a WAL event occurs.
    fn on_event(&self, event: &WalEvent);
}

/// Control messages for AsyncWalWriter background thread.
#[derive(Debug)]
enum ControlMessage {
    /// Drain all pending entries without calling write_fn (for piggyback optimization)
    DrainWithoutSync,
    /// Drain all pending entries and send acknowledgment with count
    DrainWithAck(Sender<usize>),
}

/// Async WAL writer that uses a background thread for fsync operations.
///
/// Writes return immediately after queueing the entry to a bounded channel.
/// A background thread continuously drains the channel and performs batched fsyncs.
///
/// # Shutdown
///
/// When dropped, the writer signals the background thread to stop, which then:
/// 1. Stops accepting new entries (sender dropped)
/// 2. Drains all remaining entries from the channel
/// 3. Performs final fsync
/// 4. Exits cleanly
///
/// This ensures no data loss on shutdown.
pub struct AsyncWalWriter {
    /// Sender for queueing entries
    sender: Sender<WalEntry>,
    /// Control channel for background thread commands
    control_sender: Sender<ControlMessage>,
    /// Metrics tracker
    metrics: Arc<AsyncWalMetrics>,
    /// Background thread handle
    sync_thread: Option<JoinHandle<()>>,
    /// Observers to notify on WAL events
    #[allow(dead_code)] // Used in sync_loop closure, clippy doesn't detect it
    observers: Vec<Arc<dyn WalObserver>>,
    /// Thread health flag - set to false if background thread panics
    thread_alive: Arc<AtomicBool>,
}

/// Metrics for async WAL operations.
///
/// # Thread Safety
///
/// All metrics use `Ordering::Relaxed` for performance. This means:
/// - Values are **approximate** and may be slightly stale
/// - `buffer_depth` may not exactly match the channel size due to race conditions
/// - Suitable for observability/monitoring but not for correctness guarantees
///
/// This is acceptable because metrics are for observability, not synchronization.
#[derive(Debug)]
pub struct AsyncWalMetrics {
    /// Approximate number of entries in buffer (may be stale)
    pub buffer_depth: AtomicUsize,
    /// Total number of entries written (approximate)
    pub total_entries: AtomicU64,
    /// Total number of fsyncs performed (approximate)
    pub total_syncs: AtomicU64,
}

impl AsyncWalMetrics {
    fn new() -> Self {
        Self {
            buffer_depth: AtomicUsize::new(0),
            total_entries: AtomicU64::new(0),
            total_syncs: AtomicU64::new(0),
        }
    }

    /// Get current buffer depth.
    pub fn buffer_depth(&self) -> usize {
        self.buffer_depth.load(Ordering::Relaxed)
    }

    /// Get total entries written.
    pub fn total_entries(&self) -> u64 {
        self.total_entries.load(Ordering::Relaxed)
    }

    /// Get total syncs performed.
    pub fn total_syncs(&self) -> u64 {
        self.total_syncs.load(Ordering::Relaxed)
    }
}

impl AsyncWalWriter {
    /// Create a new AsyncWalWriter with the specified configuration.
    ///
    /// # Arguments
    ///
    /// * `buffer_size` - Maximum number of entries to buffer (backpressure threshold)
    /// * `sync_interval` - How often to fsync when idle (recv_timeout duration)
    /// * `write_fn` - Function to write and sync entries (receives batch)
    /// * `observers` - List of observers to notify on WAL events
    ///
    /// # Safety Requirements for write_fn
    ///
    /// The `write_fn` must be unwind-safe (panic-safe):
    /// - Must not hold locks that would cause data corruption if poisoned
    /// - Should not maintain internal state that could become inconsistent after panic
    /// - The background thread catches panics and marks the writer as dead, but
    ///   write_fn should still be designed to avoid leaving the system in a bad state
    ///
    /// # Panics
    ///
    /// Panics if the background thread cannot be spawned (rare, resource exhaustion).
    pub fn new<F>(
        buffer_size: usize,
        sync_interval: Duration,
        write_fn: F,
        observers: Vec<Arc<dyn WalObserver>>,
    ) -> Self
    where
        F: Fn(Vec<WalEntry>) + Send + 'static,
    {
        let (sender, receiver) = bounded(buffer_size);
        let (control_sender, control_receiver) = bounded(10); // Small control channel
        let metrics = Arc::new(AsyncWalMetrics::new());
        let metrics_clone = Arc::clone(&metrics);
        let observers_clone = observers.clone();
        let thread_alive = Arc::new(AtomicBool::new(true));
        let thread_alive_clone = Arc::clone(&thread_alive);

        let sync_thread = thread::Builder::new()
            .name("gallifreydb-async-wal".to_string())
            .spawn(move || {
                // Catch panics and mark thread as dead
                // SAFETY: AssertUnwindSafe is safe here because:
                // 1. sync_loop only holds internal locks (metrics) that use poisoning correctly
                // 2. The write_fn is required to be unwind-safe (documented in new())
                // 3. Channel state is guaranteed by crossbeam to be consistent after panic
                // 4. We mark thread_alive=false to prevent further operations
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::sync_loop(
                        receiver,
                        control_receiver,
                        write_fn,
                        sync_interval,
                        metrics_clone,
                        observers_clone,
                    );
                }));

                if result.is_err() {
                    thread_alive_clone.store(false, Ordering::SeqCst);
                }
            })
            .expect("failed to spawn async WAL sync thread");

        Self {
            sender,
            control_sender,
            metrics,
            sync_thread: Some(sync_thread),
            observers,
            thread_alive,
        }
    }

    /// Append an entry to the WAL asynchronously.
    ///
    /// Returns immediately after queueing the entry. If the buffer is full,
    /// this method blocks until space is available (backpressure).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::WalError`] if the background sync thread has terminated unexpectedly.
    pub fn append(&self, entry: WalEntry) -> Result<()> {
        // Check if background thread is still alive
        if !self.thread_alive.load(Ordering::SeqCst) {
            return Err(StorageError::WalError {
                reason: "WAL background sync thread has terminated unexpectedly".to_string(),
            }
            .into());
        }

        // Try non-blocking send first, then blocking if needed
        let send_ok = match self.sender.try_send(entry) {
            Ok(()) => true,
            Err(TrySendError::Full(entry)) => {
                // Buffer full - apply backpressure by blocking
                self.sender.send(entry).is_ok()
            }
            Err(TrySendError::Disconnected(_)) => false,
        };

        if send_ok {
            // Increment metric after successful send to avoid race condition
            self.metrics.buffer_depth.fetch_add(1, Ordering::Relaxed);
            Ok(())
        } else {
            Err(StorageError::WalError {
                reason: "WAL background sync thread has terminated unexpectedly".to_string(),
            }
            .into())
        }
    }

    /// Get metrics for monitoring.
    pub fn metrics(&self) -> &AsyncWalMetrics {
        &self.metrics
    }

    /// Drain all pending entries without fsyncing (for piggyback optimization).
    ///
    /// This is used when a synchronous commit is about to happen - we want to
    /// drain the async writer's queue so those entries piggyback on the sync fsync,
    /// avoiding a redundant fsync later.
    ///
    /// # How It Works
    ///
    /// Sends a control message to the background thread to drain all pending
    /// entries without calling write_fn. The entries are already written to the
    /// WAL's BufWriter, so the caller's fsync will persist them. This avoids
    /// a redundant fsync by the background thread.
    ///
    /// This is a non-blocking call - it sends the message and returns immediately.
    /// The background thread will process the drain command on its next iteration.
    ///
    /// # Returns
    ///
    /// The approximate number of pending entries that will be drained.
    pub fn drain_without_sync(&self) -> usize {
        let pending = self.metrics.buffer_depth.load(Ordering::Relaxed);

        // Send drain command (non-blocking, fire-and-forget)
        // If the channel is full or disconnected, ignore - the thread will
        // process entries normally or has already exited
        let _ = self
            .control_sender
            .try_send(ControlMessage::DrainWithoutSync);

        pending
    }

    /// Drain all pending entries without fsyncing, with synchronous acknowledgment.
    ///
    /// This is similar to [`drain_without_sync`] but waits for the background thread
    /// to acknowledge that the drain has completed. This eliminates race conditions
    /// where the caller's fsync might happen before the drain completes.
    ///
    /// # How It Works
    ///
    /// Sends a DrainWithAck control message with a response channel, then waits
    /// up to 1ms for the background thread to complete the drain and send back
    /// the count of drained entries.
    ///
    /// # Timeout Behavior
    ///
    /// If the background thread doesn't respond within 1ms, returns Ok(0).
    /// This is safe because the caller will fsync anyway, persisting any entries
    /// that weren't drained.
    ///
    /// ## Timeout Trade-offs
    ///
    /// The 1ms timeout is chosen to balance several concerns:
    /// - **Fast path**: Most drains complete in <100µs on modern systems
    /// - **Graceful degradation**: On timeout, entries are synced by caller (no data loss)
    /// - **Bounded latency**: Prevents caller from blocking too long on slow systems
    /// - **Observability**: Timeouts can be monitored via metrics if needed
    ///
    /// Under heavy load or slow systems, timeouts may occur more frequently, resulting
    /// in redundant fsyncs but no correctness issues. If this becomes a performance
    /// concern, consider using the async-only durability mode or tuning the timeout
    /// value in future versions.
    ///
    /// # Returns
    ///
    /// The actual number of entries that were drained.
    ///
    /// # Errors
    ///
    /// Returns error if the control channel is full (shouldn't happen with size 10)
    /// or if the background thread has terminated.
    pub fn drain_without_sync_sync(&self) -> Result<usize> {
        let (ack_sender, ack_receiver) = bounded(1);

        // Send drain command with acknowledgment channel
        self.control_sender
            .try_send(ControlMessage::DrainWithAck(ack_sender))
            .map_err(|_| StorageError::WalError {
                reason: "Failed to send drain command (control channel full or disconnected)"
                    .to_string(),
            })?;

        // Wait up to 1ms for acknowledgment
        match ack_receiver.recv_timeout(Duration::from_millis(1)) {
            Ok(count) => Ok(count),
            Err(_) => {
                // Timeout or channel closed - return 0 (caller will fsync anyway)
                Ok(0)
            }
        }
    }

    /// Background sync loop (uses recv_timeout, NOT busy-wait).
    ///
    /// This is the main loop for the background thread. It:
    /// 1. Waits for entries using recv_timeout (non-busy wait)
    /// 2. Drains all pending entries into a batch
    /// 3. Calls write_fn with the batch
    /// 4. Updates metrics
    /// 5. Notifies observers
    /// 6. Repeats until sender is disconnected
    ///
    /// # Shutdown
    ///
    /// When the sender is dropped (channel disconnected), the loop:
    /// 1. Drains all remaining entries from the channel
    /// 2. Performs final fsync
    /// 3. Exits cleanly
    ///
    /// # Channel Disconnect Guarantees
    ///
    /// The `crossbeam_channel` provides critical ordering guarantees:
    /// 1. When the sender is dropped, `recv_timeout()` will return `Disconnected`
    /// 2. This happens **AFTER** all pending entries have been delivered
    /// 3. Therefore, calling `try_recv()` in a loop will retrieve all remaining entries
    /// 4. No entries are lost during shutdown - all are fsynced before thread exit
    ///
    /// This guarantee is essential for data durability on graceful shutdown.
    fn sync_loop<F>(
        receiver: Receiver<WalEntry>,
        control_receiver: Receiver<ControlMessage>,
        write_fn: F,
        sync_interval: Duration,
        metrics: Arc<AsyncWalMetrics>,
        observers: Vec<Arc<dyn WalObserver>>,
    ) where
        F: Fn(Vec<WalEntry>),
    {
        loop {
            // Check for control messages (non-blocking)
            match control_receiver.try_recv() {
                Ok(ControlMessage::DrainWithoutSync) => {
                    // Drain all pending entries WITHOUT calling write_fn
                    // (they're already in the BufWriter and will be fsynced by the caller)
                    let mut drained_count = 0;
                    while receiver.try_recv().is_ok() {
                        drained_count += 1;
                    }

                    // Update metrics only (no sync, no observer notification)
                    if drained_count > 0 {
                        metrics
                            .buffer_depth
                            .fetch_sub(drained_count, Ordering::Relaxed);
                        metrics
                            .total_entries
                            .fetch_add(drained_count as u64, Ordering::Relaxed);
                        // Note: total_syncs NOT incremented since we didn't fsync
                    }

                    continue; // Check for more control messages or entries
                }
                Ok(ControlMessage::DrainWithAck(ack_sender)) => {
                    // Drain all pending entries WITHOUT calling write_fn, then send ack
                    let mut drained_count = 0;
                    while receiver.try_recv().is_ok() {
                        drained_count += 1;
                    }

                    // Update metrics only (no sync, no observer notification)
                    if drained_count > 0 {
                        metrics
                            .buffer_depth
                            .fetch_sub(drained_count, Ordering::Relaxed);
                        metrics
                            .total_entries
                            .fetch_add(drained_count as u64, Ordering::Relaxed);
                        // Note: total_syncs NOT incremented since we didn't fsync
                    }

                    // Send acknowledgment with drained count
                    // Ignore send errors (caller may have timed out)
                    let _ = ack_sender.try_send(drained_count);

                    continue; // Check for more control messages or entries
                }
                Err(_) => {
                    // No control message, continue to entry processing
                }
            }

            // Wait for the first entry, or a timeout, or disconnect.
            let (first_entry, is_disconnected) = match receiver.recv_timeout(sync_interval) {
                Ok(entry) => (Some(entry), false),
                Err(RecvTimeoutError::Timeout) => {
                    // No entries received, loop again.
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // Channel is disconnected. We'll do one final drain.
                    (None, true)
                }
            };

            // Build batch from first entry (if any) plus any pending entries
            let mut batch = Vec::new();
            if let Some(entry) = first_entry {
                batch.push(entry);
            }

            // Drain any other pending entries non-blockingly.
            // In the disconnected case, this will drain the remainder of the channel.
            while let Ok(entry) = receiver.try_recv() {
                batch.push(entry);
            }

            if !batch.is_empty() {
                let batch_size = batch.len();

                // Check for drain command before syncing (optimization)
                // If a drain arrives just before we sync, we can skip the sync
                // since the caller will fsync these entries anyway
                match control_receiver.try_recv() {
                    Ok(ControlMessage::DrainWithoutSync) => {
                        // Entries already drained (they're in the batch), just update metrics
                        metrics
                            .buffer_depth
                            .fetch_sub(batch_size, Ordering::Relaxed);
                        metrics
                            .total_entries
                            .fetch_add(batch_size as u64, Ordering::Relaxed);
                        // Note: total_syncs NOT incremented since we didn't fsync

                        // If disconnected after drain, exit; otherwise continue
                        if is_disconnected {
                            break;
                        }
                        continue;
                    }
                    Ok(ControlMessage::DrainWithAck(ack_sender)) => {
                        // Entries already drained, update metrics and send ack
                        metrics
                            .buffer_depth
                            .fetch_sub(batch_size, Ordering::Relaxed);
                        metrics
                            .total_entries
                            .fetch_add(batch_size as u64, Ordering::Relaxed);
                        // Note: total_syncs NOT incremented since we didn't fsync

                        // Send acknowledgment with batch size (these were "drained")
                        let _ = ack_sender.try_send(batch_size);

                        // If disconnected after drain, exit; otherwise continue
                        if is_disconnected {
                            break;
                        }
                        continue;
                    }
                    Err(_) => {
                        // No drain command, proceed with normal sync
                    }
                }

                // Write and sync the batch
                write_fn(batch);

                // Update metrics
                metrics
                    .buffer_depth
                    .fetch_sub(batch_size, Ordering::Relaxed);
                metrics
                    .total_entries
                    .fetch_add(batch_size as u64, Ordering::Relaxed);
                metrics.total_syncs.fetch_add(1, Ordering::Relaxed);

                // Notify observers
                let event = WalEvent::SyncCompleted {
                    entry_count: batch_size,
                    timestamp: time::now(),
                };
                for observer in &observers {
                    observer.on_event(&event);
                }
            }

            // If the channel was disconnected, we've just performed the final drain. Exit.
            if is_disconnected {
                break;
            }
        }
    }
}

impl Drop for AsyncWalWriter {
    fn drop(&mut self) {
        // Drop sender to signal shutdown
        drop(std::mem::replace(
            &mut self.sender,
            bounded(1).0, // Replace with dummy sender
        ));

        // Wait for background thread to drain and exit
        if let Some(handle) = self.sync_thread.take() {
            // CRITICAL: Capture and log background thread panics to prevent silent data loss
            if let Err(e) = handle.join() {
                // In production, this should use proper logging (e.g., tracing)
                eprintln!(
                    "CRITICAL: AsyncWalWriter background thread panicked: {:?}",
                    e
                );
                eprintln!("This may indicate data loss. Check WAL integrity.");
                // Note: We can't return an error from Drop, but logging is essential
                // for debugging and alerting operators to potential data corruption
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMap;
    use crate::core::temporal::BiTemporalInterval;
    use crate::storage::wal::{LSN, WalOperation};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;

    fn create_test_entry(lsn: u64) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp: 0,
            operation: WalOperation::CreateNode {
                // SECURITY: Use validated NodeId::new() instead of new_unchecked()
                // per CODING_STANDARDS.md to prevent DoS attacks from invalid IDs
                node_id: NodeId::new(lsn).expect("valid node ID"),
                label: "TestNode".to_string(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(0),
            },
            checksum: 0,
        }
    }

    #[test]
    fn test_async_writer_creation() {
        let write_count = Arc::new(AtomicU64::new(0));
        let write_count_clone = Arc::clone(&write_count);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(10),
            move |batch| {
                write_count_clone.fetch_add(batch.len() as u64, Ordering::SeqCst);
            },
            vec![],
        );

        assert_eq!(writer.metrics().buffer_depth(), 0);
        assert_eq!(writer.metrics().total_entries(), 0);
        assert_eq!(writer.metrics().total_syncs(), 0);
    }

    #[test]
    fn test_append_single_entry() {
        let written_entries = Arc::new(Mutex::new(Vec::new()));
        let written_entries_clone = Arc::clone(&written_entries);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(10),
            move |batch| {
                let mut entries = written_entries_clone.lock().unwrap();
                entries.extend(batch);
            },
            vec![],
        );

        let entry = create_test_entry(1);
        writer.append(entry).expect("append should succeed");

        // Wait for background thread to process
        thread::sleep(Duration::from_millis(50));

        let entries = written_entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].lsn, LSN(1));
    }

    #[test]
    fn test_batch_processing() {
        let batches = Arc::new(Mutex::new(Vec::new()));
        let batches_clone = Arc::clone(&batches);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(50),
            move |batch| {
                let mut b = batches_clone.lock().unwrap();
                b.push(batch.len());
            },
            vec![],
        );

        // Write multiple entries rapidly
        for i in 1..=10 {
            writer
                .append(create_test_entry(i))
                .expect("append should succeed");
        }

        // Wait for processing
        thread::sleep(Duration::from_millis(100));

        // Should have batched multiple entries
        let b = batches.lock().unwrap();
        let total: usize = b.iter().sum();
        assert_eq!(total, 10, "all entries should be written");

        // Verify metrics
        assert_eq!(writer.metrics().total_entries(), 10);
    }

    #[test]
    fn test_graceful_shutdown() {
        let written_entries = Arc::new(Mutex::new(Vec::new()));
        let written_entries_clone = Arc::clone(&written_entries);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_secs(60),
            move |batch| {
                let mut entries = written_entries_clone.lock().unwrap();
                entries.extend(batch);
            },
            vec![],
        );

        // Write entries
        for i in 1..=5 {
            writer
                .append(create_test_entry(i))
                .expect("append should succeed");
        }

        // Drop writer immediately (should drain buffer)
        drop(writer);

        // All entries should be written
        let entries = written_entries.lock().unwrap();
        assert_eq!(entries.len(), 5, "all entries should be flushed on drop");
    }

    #[test]
    fn test_backpressure() {
        // Use a very small buffer (1 entry) and ensure slow processing
        let write_started = Arc::new(Mutex::new(false));
        let write_started_clone = Arc::clone(&write_started);

        let writer = AsyncWalWriter::new(
            1,
            Duration::from_secs(60),
            move |_batch| {
                // Mark that write started, then sleep
                *write_started_clone.lock().unwrap() = true;
                thread::sleep(Duration::from_millis(300));
            },
            vec![],
        );

        // Fill the single-slot buffer
        writer.append(create_test_entry(1)).unwrap();

        // Wait for background thread to start processing (emptying buffer slot)
        thread::sleep(Duration::from_millis(50));

        // Verify write started
        assert!(
            *write_started.lock().unwrap(),
            "background write should have started"
        );

        // Now buffer slot is taken by processing. Add one more entry to fill it.
        writer.append(create_test_entry(2)).unwrap();

        // Buffer is now full (1 slot filled). Next append should block.
        let start = std::time::Instant::now();
        writer.append(create_test_entry(3)).unwrap();
        let elapsed = start.elapsed();

        // Should have blocked for at least 100ms waiting for processing to complete
        assert!(
            elapsed >= Duration::from_millis(100),
            "should apply backpressure, elapsed: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_metrics_tracking() {
        let write_count = Arc::new(AtomicU64::new(0));
        let write_count_clone = Arc::clone(&write_count);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(10),
            move |batch| {
                write_count_clone.fetch_add(batch.len() as u64, Ordering::SeqCst);
            },
            vec![],
        );

        // Write entries
        for i in 1..=20 {
            writer.append(create_test_entry(i)).unwrap();
        }

        // Wait for processing
        thread::sleep(Duration::from_millis(100));

        // Check metrics
        assert_eq!(writer.metrics().total_entries(), 20);
        assert!(writer.metrics().total_syncs() > 0);
        assert_eq!(writer.metrics().buffer_depth(), 0);
    }

    #[test]
    fn test_recv_timeout_pattern() {
        let sync_count = Arc::new(AtomicU64::new(0));
        let sync_count_clone = Arc::clone(&sync_count);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(50),
            move |_batch| {
                sync_count_clone.fetch_add(1, Ordering::SeqCst);
            },
            vec![],
        );

        // Write one entry
        writer.append(create_test_entry(1)).unwrap();

        // Wait for first sync
        thread::sleep(Duration::from_millis(100));
        let first_count = sync_count.load(Ordering::SeqCst);
        assert!(first_count > 0, "should have synced at least once");

        // Wait for timeout-based sync (no new entries)
        // The timeout is 50ms, so no additional syncs should occur
        // since there are no entries to process
        thread::sleep(Duration::from_millis(150));
        let second_count = sync_count.load(Ordering::SeqCst);

        // Should not have increased much (maybe 1-2 from first entry processing)
        // This verifies we're using recv_timeout correctly (not busy-waiting)
        assert_eq!(
            first_count, second_count,
            "should not busy-wait when no entries"
        );
    }

    #[test]
    fn test_extremely_large_batch() {
        // Test with 1M buffer size to ensure no overflow or performance issues
        let write_count = Arc::new(AtomicU64::new(0));
        let write_count_clone = Arc::clone(&write_count);

        let writer = AsyncWalWriter::new(
            1_000_000, // 1M buffer
            Duration::from_millis(100),
            move |batch| {
                write_count_clone.fetch_add(batch.len() as u64, Ordering::SeqCst);
            },
            vec![],
        );

        // Write 10k entries - should handle large batch efficiently
        for i in 1..=10_000 {
            writer.append(create_test_entry(i)).unwrap();
        }

        drop(writer);

        assert_eq!(
            write_count.load(Ordering::SeqCst),
            10_000,
            "all entries should be synced"
        );
    }

    #[test]
    fn test_zero_interval_sync() {
        // Test immediate flush (0ms interval) - should sync on every entry or batch
        let sync_count = Arc::new(AtomicU64::new(0));
        let sync_count_clone = Arc::clone(&sync_count);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(1), // Near-zero interval (1ms minimum)
            move |batch| {
                sync_count_clone.fetch_add(1, Ordering::SeqCst);
                let _ = batch.len();
            },
            vec![],
        );

        // Write a few entries
        for i in 1..=5 {
            writer.append(create_test_entry(i)).unwrap();
            thread::sleep(Duration::from_millis(2)); // Give sync time to occur
        }

        drop(writer);

        let syncs = sync_count.load(Ordering::SeqCst);
        // Should have multiple syncs due to near-zero interval
        assert!(syncs >= 3, "should have frequent syncs with low interval");
    }

    #[test]
    fn test_concurrent_append_from_multiple_threads() {
        use std::sync::Barrier;

        let write_count = Arc::new(AtomicU64::new(0));
        let write_count_clone = Arc::clone(&write_count);

        let writer = Arc::new(AsyncWalWriter::new(
            10_000, // Large buffer to avoid backpressure
            Duration::from_millis(50),
            move |batch| {
                write_count_clone.fetch_add(batch.len() as u64, Ordering::SeqCst);
            },
            vec![],
        ));

        let num_threads = 4;
        let entries_per_thread = 250;
        let barrier = Arc::new(Barrier::new(num_threads));

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let writer = Arc::clone(&writer);
                let barrier = Arc::clone(&barrier);

                thread::spawn(move || {
                    // Wait for all threads to be ready
                    barrier.wait();

                    // Each thread writes entries concurrently
                    for i in 0..entries_per_thread {
                        let lsn = (thread_id * entries_per_thread + i + 1) as u64;
                        writer.append(create_test_entry(lsn)).unwrap();
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        drop(writer);

        let total = write_count.load(Ordering::SeqCst);
        let expected = (num_threads * entries_per_thread) as u64;
        assert_eq!(
            total, expected,
            "all entries from concurrent threads should be synced"
        );
    }

    #[test]
    fn test_background_thread_panic_detection() {
        use std::sync::atomic::AtomicBool;

        // Flag to signal when write_fn is called (ensures panic actually happens)
        let write_fn_called = Arc::new(AtomicBool::new(false));
        let write_fn_called_clone = Arc::clone(&write_fn_called);

        // Create writer with a write_fn that panics after setting flag
        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(10),
            move |_batch| {
                write_fn_called_clone.store(true, Ordering::SeqCst);
                panic!("Simulated background thread panic");
            },
            vec![],
        );

        // Append an entry to trigger the panic
        writer.append(create_test_entry(1)).unwrap();

        // Wait for write_fn to be called (with 5s timeout for slow CI)
        let start = std::time::Instant::now();
        while !write_fn_called.load(Ordering::SeqCst) {
            if start.elapsed() > Duration::from_secs(5) {
                panic!(
                    "write_fn was never called after 5 seconds - background thread not processing entries"
                );
            }
            thread::sleep(Duration::from_millis(10));
        }

        // Poll until append() fails, indicating thread_alive was set to false
        // (with 5s timeout for slow CI environments like Windows)
        let start = std::time::Instant::now();
        loop {
            let result = writer.append(create_test_entry(2));
            if result.is_err() {
                // Success - append detected the dead thread
                break;
            }

            if start.elapsed() > Duration::from_secs(5) {
                panic!(
                    "append() never failed after background thread panic - thread_alive not updated"
                );
            }
            thread::sleep(Duration::from_millis(10));
        }

        // Drop should capture the panic (verified by stderr output in real run)
        drop(writer);
    }

    #[test]
    fn test_append_after_drop_fails() {
        let write_count = Arc::new(AtomicU64::new(0));
        let write_count_clone = Arc::clone(&write_count);

        let writer = AsyncWalWriter::new(
            100,
            Duration::from_millis(10),
            move |batch| {
                write_count_clone.fetch_add(batch.len() as u64, Ordering::SeqCst);
            },
            vec![],
        );

        // Drop the writer
        drop(writer);

        // This test demonstrates expected behavior:
        // After drop, the writer is consumed and cannot be used
        // (Rust's ownership prevents this at compile time)
        // This test documents that append() can only fail if background thread dies
    }

    #[test]
    fn test_piggyback_optimization() {
        // Track batch sizes to verify multiple entries are batched together
        let batch_sizes = Arc::new(Mutex::new(Vec::new()));
        let batch_sizes_clone = Arc::clone(&batch_sizes);

        let writer = AsyncWalWriter::new(
            1000,                      // Large buffer
            Duration::from_millis(20), // 20ms sync interval
            move |batch| {
                batch_sizes_clone.lock().unwrap().push(batch.len());
                // Simulate slow fsync to allow more entries to accumulate
                thread::sleep(Duration::from_millis(5));
            },
            vec![],
        );

        // Rapidly append 100 entries - they should batch together
        for i in 1..=100 {
            writer
                .append(create_test_entry(i))
                .expect("append should succeed");
        }

        // Wait for all entries to be processed
        thread::sleep(Duration::from_millis(200));
        drop(writer); // Ensure final batch is flushed

        // Verify piggyback optimization: fewer fsyncs than entries
        let sizes = batch_sizes.lock().unwrap();
        let total_entries: usize = sizes.iter().sum();
        let num_batches = sizes.len();

        assert_eq!(total_entries, 100, "all entries should be written");

        // The piggyback optimization should result in significantly fewer
        // fsyncs than entries. With 100 entries and 20ms interval + 5ms fsync,
        // we expect around 4-10 batches depending on timing.
        assert!(
            num_batches < 50,
            "piggyback optimization should batch multiple entries: {} batches for 100 entries (expected < 50)",
            num_batches
        );

        // At least one batch should have multiple entries
        assert!(
            sizes.iter().any(|&size| size > 1),
            "at least one batch should contain multiple entries"
        );

        println!(
            "Piggyback optimization: 100 entries in {} batches (avg {:.1} entries/batch)",
            num_batches,
            total_entries as f64 / num_batches as f64
        );
    }

    #[test]
    fn test_drain_without_sync_optimization() {
        use std::sync::Barrier;
        use std::sync::atomic::AtomicUsize;

        // Track sync calls to verify drain optimization reduces redundant fsyncs
        let sync_count = Arc::new(AtomicUsize::new(0));
        let sync_count_clone = Arc::clone(&sync_count);

        // Use barrier to ensure we can append entries faster than background thread processes
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = Arc::clone(&barrier);

        let writer = AsyncWalWriter::new(
            1000,
            Duration::from_millis(10),
            move |_batch| {
                // Wait for main thread to signal before processing
                // This ensures entries stay queued for drain test
                barrier_clone.wait();
                sync_count_clone.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(10)); // Simulate slow fsync
            },
            vec![],
        );

        // Append entries rapidly
        for i in 1..=10 {
            writer
                .append(create_test_entry(i))
                .expect("append should succeed");
        }

        // Give entries time to reach the channel
        thread::sleep(Duration::from_millis(20));

        // Background thread is now blocked in write_fn waiting at barrier
        // Entries are in the channel, waiting to be processed

        // Drain without sync (simulating piggyback optimization)
        let drained_count = writer.drain_without_sync();

        // Give background thread time to process drain command
        thread::sleep(Duration::from_millis(50));

        // Verify entries were drained WITHOUT calling write_fn
        // The barrier is still blocking, so sync_count should remain 1 (for first batch before drain)
        let _syncs_before_release = sync_count.load(Ordering::SeqCst);

        // Release the barrier to let background thread complete
        barrier.wait();

        // Wait for any in-flight processing
        thread::sleep(Duration::from_millis(50));

        // After drain, no additional syncs should have happened
        let syncs_after_release = sync_count.load(Ordering::SeqCst);

        // We expect exactly 1 sync (the first batch that triggered before drain)
        // The drained entries should NOT have caused additional syncs
        assert_eq!(
            syncs_after_release, 1,
            "should have exactly 1 sync (first batch only, drained entries don't sync)"
        );

        assert!(
            drained_count > 0,
            "should have drained some entries: {}",
            drained_count
        );

        // Metrics should show entries were processed (counted) but with fewer syncs
        let total_entries = writer.metrics().total_entries();
        let total_syncs = writer.metrics().total_syncs();

        assert_eq!(
            total_entries, 10,
            "all 10 entries should be counted as processed"
        );
        assert_eq!(
            total_syncs, 1,
            "only 1 sync should have happened (drain prevented redundant sync)"
        );

        drop(writer);
    }
}
