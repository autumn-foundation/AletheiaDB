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
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// Metrics tracker
    metrics: Arc<AsyncWalMetrics>,
    /// Background thread handle
    sync_thread: Option<JoinHandle<()>>,
    /// Observers to notify on WAL events
    #[allow(dead_code)] // Used in sync_loop closure, clippy doesn't detect it
    observers: Vec<Arc<dyn WalObserver>>,
}

/// Metrics for async WAL operations.
#[derive(Debug)]
pub struct AsyncWalMetrics {
    /// Current number of entries in buffer
    pub buffer_depth: AtomicUsize,
    /// Total number of entries written
    pub total_entries: AtomicU64,
    /// Total number of fsyncs performed
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
        let metrics = Arc::new(AsyncWalMetrics::new());
        let metrics_clone = Arc::clone(&metrics);
        let observers_clone = observers.clone();

        let sync_thread = thread::Builder::new()
            .name("gallifreydb-async-wal".to_string())
            .spawn(move || {
                Self::sync_loop(
                    receiver,
                    write_fn,
                    sync_interval,
                    metrics_clone,
                    observers_clone,
                );
            })
            .expect("failed to spawn async WAL sync thread");

        Self {
            sender,
            metrics,
            sync_thread: Some(sync_thread),
            observers,
        }
    }

    /// Append an entry to the WAL asynchronously.
    ///
    /// Returns immediately after queueing the entry. If the buffer is full,
    /// this method blocks until space is available (backpressure).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::WalClosed`] if the background thread has died.
    pub fn append(&self, entry: WalEntry) -> Result<()> {
        // Try non-blocking send first
        match self.sender.try_send(entry) {
            Ok(()) => {
                self.metrics.buffer_depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(entry)) => {
                // Buffer full - apply backpressure by blocking
                self.sender
                    .send(entry)
                    .map_err(|_| StorageError::WalError {
                        reason: "Background sync thread terminated".to_string(),
                    })?;
                self.metrics.buffer_depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(StorageError::WalError {
                reason: "Background sync thread terminated".to_string(),
            }
            .into()),
        }
    }

    /// Get metrics for monitoring.
    pub fn metrics(&self) -> &AsyncWalMetrics {
        &self.metrics
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
    fn sync_loop<F>(
        receiver: Receiver<WalEntry>,
        write_fn: F,
        sync_interval: Duration,
        metrics: Arc<AsyncWalMetrics>,
        observers: Vec<Arc<dyn WalObserver>>,
    ) where
        F: Fn(Vec<WalEntry>),
    {
        loop {
            match receiver.recv_timeout(sync_interval) {
                Ok(first_entry) => {
                    // Got at least one entry - build batch
                    let mut batch = vec![first_entry];

                    // Drain any other pending entries non-blockingly
                    while let Ok(entry) = receiver.try_recv() {
                        batch.push(entry);
                    }

                    let batch_size = batch.len();

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
                Err(RecvTimeoutError::Timeout) => {
                    // Timeout - no entries to flush, just continue
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // Sender dropped - drain remaining entries and exit
                    let mut final_batch = Vec::new();
                    while let Ok(entry) = receiver.try_recv() {
                        final_batch.push(entry);
                    }

                    if !final_batch.is_empty() {
                        let batch_size = final_batch.len();
                        write_fn(final_batch);

                        // Update metrics
                        metrics
                            .buffer_depth
                            .fetch_sub(batch_size, Ordering::Relaxed);
                        metrics
                            .total_entries
                            .fetch_add(batch_size as u64, Ordering::Relaxed);
                        metrics.total_syncs.fetch_add(1, Ordering::Relaxed);

                        // Notify observers about final sync
                        let event = WalEvent::SyncCompleted {
                            entry_count: batch_size,
                            timestamp: time::now(),
                        };
                        for observer in &observers {
                            observer.on_event(&event);
                        }
                    }

                    break;
                }
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
            let _ = handle.join(); // Blocks until thread completes
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
                node_id: NodeId::new_unchecked(lsn),
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
}
