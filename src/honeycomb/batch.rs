//! Batch buffer for collecting events before transmission.
//!
//! The `BatchBuffer` collects events and triggers transmission when either:
//! - The batch reaches the maximum size
//! - The batch timeout expires

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::config::TransmissionOptions;
use super::event::Event;

/// Statistics about batch operations.
#[derive(Debug, Default)]
pub struct BatchStats {
    /// Total events added to the buffer.
    pub events_added: AtomicU64,
    /// Total events sent successfully.
    pub events_sent: AtomicU64,
    /// Total events dropped (due to capacity or errors).
    pub events_dropped: AtomicU64,
    /// Total batches sent.
    pub batches_sent: AtomicU64,
    /// Total batches failed.
    pub batches_failed: AtomicU64,
}

impl BatchStats {
    /// Create new statistics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an event being added.
    pub fn record_event_added(&self) {
        self.events_added.fetch_add(1, Ordering::Relaxed);
    }

    /// Record events being sent.
    pub fn record_events_sent(&self, count: u64) {
        self.events_sent.fetch_add(count, Ordering::Relaxed);
    }

    /// Record an event being dropped.
    pub fn record_event_dropped(&self) {
        self.events_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a batch being sent.
    pub fn record_batch_sent(&self) {
        self.batches_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a batch failing.
    pub fn record_batch_failed(&self) {
        self.batches_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Get snapshot of current stats.
    pub fn snapshot(&self) -> BatchStatsSnapshot {
        BatchStatsSnapshot {
            events_added: self.events_added.load(Ordering::Relaxed),
            events_sent: self.events_sent.load(Ordering::Relaxed),
            events_dropped: self.events_dropped.load(Ordering::Relaxed),
            batches_sent: self.batches_sent.load(Ordering::Relaxed),
            batches_failed: self.batches_failed.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of batch statistics at a point in time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchStatsSnapshot {
    /// Total events added to the buffer.
    pub events_added: u64,
    /// Total events sent successfully.
    pub events_sent: u64,
    /// Total events dropped.
    pub events_dropped: u64,
    /// Total batches sent.
    pub batches_sent: u64,
    /// Total batches failed.
    pub batches_failed: u64,
}

/// A buffer that collects events into batches for efficient transmission.
///
/// Events are accumulated until either:
/// - The buffer reaches `max_batch_size` events
/// - The `batch_timeout` duration has elapsed since the first event
///
/// # Thread Safety
///
/// `BatchBuffer` is thread-safe and can be shared across threads using `Arc`.
///
/// # Lock Ordering
///
/// This struct uses two mutexes internally. To prevent deadlocks, they MUST always
/// be acquired in this order:
/// 1. `events` mutex first
/// 2. `batch_start` mutex second
///
/// All methods in this struct follow this ordering. If you modify this struct,
/// ensure any new code maintains this lock ordering to prevent deadlocks.
#[derive(Debug)]
pub struct BatchBuffer {
    /// Internal buffer of events.
    events: Mutex<Vec<Event>>,
    /// Configuration options.
    options: TransmissionOptions,
    /// Timestamp when the first event was added to the current batch.
    batch_start: Mutex<Option<Instant>>,
    /// Statistics about batch operations.
    stats: Arc<BatchStats>,
}

impl BatchBuffer {
    /// Create a new batch buffer with the given options.
    pub fn new(options: TransmissionOptions) -> Self {
        Self {
            events: Mutex::new(Vec::with_capacity(options.max_batch_size)),
            options,
            batch_start: Mutex::new(None),
            stats: Arc::new(BatchStats::new()),
        }
    }

    /// Create a new batch buffer with default options.
    pub fn with_defaults() -> Self {
        Self::new(TransmissionOptions::default())
    }

    /// Add an event to the buffer.
    ///
    /// Returns `Ok(Some(batch))` if the buffer is now full and should be flushed,
    /// `Ok(None)` if the event was added but the buffer is not yet full,
    /// or `Err` if the buffer is at capacity.
    ///
    /// # Mutex Poisoning
    ///
    /// If a previous thread panicked while holding the mutex, this method recovers
    /// the data rather than propagating the error. This ensures observability data
    /// is preserved even during crash scenarios.
    pub fn add(&self, event: Event) -> Result<Option<Vec<Event>>, super::error::Error> {
        // Recover from poisoned mutex - observability should work even after panics
        let mut events = self.events.lock().unwrap_or_else(|poisoned| {
            #[cfg(feature = "observability")]
            tracing::warn!("BatchBuffer events mutex was poisoned, recovering data");
            #[cfg(not(feature = "observability"))]
            eprintln!("Warning: BatchBuffer events mutex was poisoned, recovering data");
            poisoned.into_inner()
        });

        // Check capacity
        if events.len() >= self.options.pending_work_capacity {
            self.stats.record_event_dropped();
            return Err(super::error::Error::Buffer(
                "Buffer at capacity, event dropped".to_string(),
            ));
        }

        // Set batch start time if this is the first event
        // Recover from poisoned mutex
        let mut batch_start = self.batch_start.lock().unwrap_or_else(|poisoned| {
            #[cfg(feature = "observability")]
            tracing::warn!("BatchBuffer batch_start mutex was poisoned, recovering data");
            #[cfg(not(feature = "observability"))]
            eprintln!("Warning: BatchBuffer batch_start mutex was poisoned, recovering data");
            poisoned.into_inner()
        });
        if batch_start.is_none() {
            *batch_start = Some(Instant::now());
        }

        events.push(event);
        self.stats.record_event_added();

        // Check if batch is full
        if events.len() >= self.options.max_batch_size {
            let batch = std::mem::replace(
                &mut *events,
                Vec::with_capacity(self.options.max_batch_size),
            );
            *batch_start = None;
            return Ok(Some(batch));
        }

        Ok(None)
    }

    /// Check if the batch timeout has expired.
    ///
    /// Returns `true` if there are events in the buffer and the timeout has expired.
    pub fn is_timeout_expired(&self) -> bool {
        let batch_start = match self.batch_start.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };

        match *batch_start {
            Some(start) => start.elapsed() >= self.options.batch_timeout,
            None => false,
        }
    }

    /// Flush the buffer, returning all pending events.
    ///
    /// Returns an empty vector if the buffer is empty.
    ///
    /// # Mutex Poisoning
    ///
    /// Recovers from poisoned mutexes to ensure observability data is not lost.
    pub fn flush(&self) -> Result<Vec<Event>, super::error::Error> {
        // Recover from poisoned mutex - observability should work even after panics
        let mut events = self.events.lock().unwrap_or_else(|poisoned| {
            #[cfg(feature = "observability")]
            tracing::warn!("BatchBuffer events mutex was poisoned during flush, recovering data");
            #[cfg(not(feature = "observability"))]
            eprintln!(
                "Warning: BatchBuffer events mutex was poisoned during flush, recovering data"
            );
            poisoned.into_inner()
        });

        let mut batch_start = self.batch_start.lock().unwrap_or_else(|poisoned| {
            #[cfg(feature = "observability")]
            tracing::warn!(
                "BatchBuffer batch_start mutex was poisoned during flush, recovering data"
            );
            #[cfg(not(feature = "observability"))]
            eprintln!(
                "Warning: BatchBuffer batch_start mutex was poisoned during flush, recovering data"
            );
            poisoned.into_inner()
        });

        let batch = std::mem::replace(
            &mut *events,
            Vec::with_capacity(self.options.max_batch_size),
        );
        *batch_start = None;

        Ok(batch)
    }

    /// Get the number of events currently in the buffer.
    pub fn len(&self) -> usize {
        self.events.lock().map(|e| e.len()).unwrap_or(0)
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a reference to the buffer's statistics.
    pub fn stats(&self) -> &Arc<BatchStats> {
        &self.stats
    }

    /// Get the maximum batch size.
    pub fn max_batch_size(&self) -> usize {
        self.options.max_batch_size
    }

    /// Get the batch timeout duration.
    pub fn batch_timeout(&self) -> Duration {
        self.options.batch_timeout
    }

    /// Get the pending work capacity.
    pub fn pending_work_capacity(&self) -> usize {
        self.options.pending_work_capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =====================================================
    // BatchStats Tests
    // =====================================================

    #[test]
    fn test_batch_stats_new() {
        let stats = BatchStats::new();
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.events_added, 0);
        assert_eq!(snapshot.events_sent, 0);
        assert_eq!(snapshot.events_dropped, 0);
        assert_eq!(snapshot.batches_sent, 0);
        assert_eq!(snapshot.batches_failed, 0);
    }

    #[test]
    fn test_batch_stats_record_event_added() {
        let stats = BatchStats::new();
        stats.record_event_added();
        stats.record_event_added();
        assert_eq!(stats.snapshot().events_added, 2);
    }

    #[test]
    fn test_batch_stats_record_events_sent() {
        let stats = BatchStats::new();
        stats.record_events_sent(10);
        stats.record_events_sent(5);
        assert_eq!(stats.snapshot().events_sent, 15);
    }

    #[test]
    fn test_batch_stats_record_event_dropped() {
        let stats = BatchStats::new();
        stats.record_event_dropped();
        assert_eq!(stats.snapshot().events_dropped, 1);
    }

    #[test]
    fn test_batch_stats_record_batch_sent() {
        let stats = BatchStats::new();
        stats.record_batch_sent();
        stats.record_batch_sent();
        assert_eq!(stats.snapshot().batches_sent, 2);
    }

    #[test]
    fn test_batch_stats_record_batch_failed() {
        let stats = BatchStats::new();
        stats.record_batch_failed();
        assert_eq!(stats.snapshot().batches_failed, 1);
    }

    #[test]
    fn test_batch_stats_concurrent() {
        use std::thread;

        let stats = Arc::new(BatchStats::new());
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let stats = Arc::clone(&stats);
                thread::spawn(move || {
                    for _ in 0..100 {
                        stats.record_event_added();
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(stats.snapshot().events_added, 1000);
    }

    // =====================================================
    // BatchStatsSnapshot Tests
    // =====================================================

    #[test]
    fn test_batch_stats_snapshot_default() {
        let snapshot = BatchStatsSnapshot::default();
        assert_eq!(snapshot.events_added, 0);
        assert_eq!(snapshot.events_sent, 0);
    }

    #[test]
    fn test_batch_stats_snapshot_clone() {
        let snapshot = BatchStatsSnapshot {
            events_added: 10,
            events_sent: 5,
            events_dropped: 2,
            batches_sent: 1,
            batches_failed: 0,
        };
        let cloned = snapshot;
        assert_eq!(snapshot, cloned);
    }

    #[test]
    fn test_batch_stats_snapshot_debug() {
        let snapshot = BatchStatsSnapshot::default();
        let debug_str = format!("{snapshot:?}");
        assert!(debug_str.contains("BatchStatsSnapshot"));
    }

    // =====================================================
    // BatchBuffer Creation Tests
    // =====================================================

    #[test]
    fn test_batch_buffer_new() {
        let options = TransmissionOptions::default();
        let buffer = BatchBuffer::new(options);

        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
        assert_eq!(buffer.max_batch_size(), 50);
    }

    #[test]
    fn test_batch_buffer_with_defaults() {
        let buffer = BatchBuffer::with_defaults();
        assert!(buffer.is_empty());
        assert_eq!(buffer.max_batch_size(), 50);
    }

    #[test]
    fn test_batch_buffer_custom_options() {
        let options = TransmissionOptions::default()
            .with_max_batch_size(100)
            .with_batch_timeout(Duration::from_millis(200));

        let buffer = BatchBuffer::new(options);

        assert_eq!(buffer.max_batch_size(), 100);
        assert_eq!(buffer.batch_timeout(), Duration::from_millis(200));
    }

    // =====================================================
    // BatchBuffer Add Tests
    // =====================================================

    #[test]
    fn test_batch_buffer_add_single_event() {
        let buffer = BatchBuffer::with_defaults();
        let event = Event::new();

        let result = buffer.add(event).unwrap();
        assert!(result.is_none()); // Not full yet
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn test_batch_buffer_add_fills_batch() {
        let options = TransmissionOptions::default().with_max_batch_size(3);
        let buffer = BatchBuffer::new(options);

        // Add events until batch is full
        assert!(buffer.add(Event::new()).unwrap().is_none());
        assert!(buffer.add(Event::new()).unwrap().is_none());

        // Third event should trigger batch return
        let batch = buffer.add(Event::new()).unwrap();
        assert!(batch.is_some());
        assert_eq!(batch.unwrap().len(), 3);

        // Buffer should be empty now
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_batch_buffer_add_updates_stats() {
        let buffer = BatchBuffer::with_defaults();
        buffer.add(Event::new()).unwrap();
        buffer.add(Event::new()).unwrap();

        let stats = buffer.stats().snapshot();
        assert_eq!(stats.events_added, 2);
    }

    #[test]
    fn test_batch_buffer_add_at_capacity() {
        let options = TransmissionOptions::default()
            .with_max_batch_size(100)
            .with_pending_work_capacity(3);
        let buffer = BatchBuffer::new(options);

        // Add up to capacity
        buffer.add(Event::new()).unwrap();
        buffer.add(Event::new()).unwrap();
        buffer.add(Event::new()).unwrap();

        // Next add should fail
        let result = buffer.add(Event::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("at capacity"));

        // Should have recorded a dropped event
        assert_eq!(buffer.stats().snapshot().events_dropped, 1);
    }

    // =====================================================
    // BatchBuffer Timeout Tests
    // =====================================================

    #[test]
    fn test_batch_buffer_timeout_not_expired_empty() {
        let buffer = BatchBuffer::with_defaults();
        assert!(!buffer.is_timeout_expired());
    }

    #[test]
    fn test_batch_buffer_timeout_not_expired_recent() {
        let options = TransmissionOptions::default().with_batch_timeout(Duration::from_secs(10));
        let buffer = BatchBuffer::new(options);

        buffer.add(Event::new()).unwrap();

        // Just added, timeout should not be expired
        assert!(!buffer.is_timeout_expired());
    }

    #[test]
    fn test_batch_buffer_timeout_expired() {
        let options = TransmissionOptions::default().with_batch_timeout(Duration::from_millis(1));
        let buffer = BatchBuffer::new(options);

        buffer.add(Event::new()).unwrap();

        // Wait for timeout to expire
        std::thread::sleep(Duration::from_millis(10));

        assert!(buffer.is_timeout_expired());
    }

    #[test]
    fn test_batch_buffer_timeout_reset_after_flush() {
        let options = TransmissionOptions::default().with_batch_timeout(Duration::from_millis(1));
        let buffer = BatchBuffer::new(options);

        buffer.add(Event::new()).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        assert!(buffer.is_timeout_expired());

        // Flush should reset the timeout
        buffer.flush().unwrap();
        assert!(!buffer.is_timeout_expired());
    }

    // =====================================================
    // BatchBuffer Flush Tests
    // =====================================================

    #[test]
    fn test_batch_buffer_flush_empty() {
        let buffer = BatchBuffer::with_defaults();
        let batch = buffer.flush().unwrap();
        assert!(batch.is_empty());
    }

    #[test]
    fn test_batch_buffer_flush_with_events() {
        let buffer = BatchBuffer::with_defaults();

        buffer.add(Event::new()).unwrap();
        buffer.add(Event::new()).unwrap();
        buffer.add(Event::new()).unwrap();

        assert_eq!(buffer.len(), 3);

        let batch = buffer.flush().unwrap();
        assert_eq!(batch.len(), 3);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_batch_buffer_flush_multiple_times() {
        let buffer = BatchBuffer::with_defaults();

        buffer.add(Event::new()).unwrap();
        buffer.add(Event::new()).unwrap();

        let batch1 = buffer.flush().unwrap();
        assert_eq!(batch1.len(), 2);

        buffer.add(Event::new()).unwrap();

        let batch2 = buffer.flush().unwrap();
        assert_eq!(batch2.len(), 1);
    }

    // =====================================================
    // BatchBuffer Thread Safety Tests
    // =====================================================

    #[test]
    fn test_batch_buffer_concurrent_adds() {
        use std::thread;

        let buffer = Arc::new(BatchBuffer::new(
            TransmissionOptions::default()
                .with_max_batch_size(1000)
                .with_pending_work_capacity(10000),
        ));

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let buffer = Arc::clone(&buffer);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let _ = buffer.add(Event::new());
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Some events may have been batched out, so just verify stats
        let stats = buffer.stats().snapshot();
        assert_eq!(stats.events_added, 1000);
    }

    #[test]
    fn test_batch_buffer_concurrent_add_and_flush() {
        use std::thread;

        let buffer = Arc::new(BatchBuffer::new(
            TransmissionOptions::default()
                .with_max_batch_size(10)
                .with_pending_work_capacity(10000),
        ));

        let buffer_add = Arc::clone(&buffer);
        let add_handle = thread::spawn(move || {
            for _ in 0..100 {
                let _ = buffer_add.add(Event::new());
            }
        });

        let buffer_flush = Arc::clone(&buffer);
        let flush_handle = thread::spawn(move || {
            let mut total_flushed = 0;
            for _ in 0..50 {
                if let Ok(batch) = buffer_flush.flush() {
                    total_flushed += batch.len();
                }
                thread::sleep(Duration::from_micros(100));
            }
            total_flushed
        });

        add_handle.join().unwrap();
        let flushed = flush_handle.join().unwrap();

        // Verify we processed events (exact count depends on timing)
        let remaining = buffer.flush().unwrap().len();
        let stats = buffer.stats().snapshot();
        assert_eq!(stats.events_added, 100);
        assert!(flushed + remaining <= 100);
    }

    // =====================================================
    // BatchBuffer Accessor Tests
    // =====================================================

    #[test]
    fn test_batch_buffer_accessors() {
        let options = TransmissionOptions::default()
            .with_max_batch_size(100)
            .with_batch_timeout(Duration::from_millis(500))
            .with_pending_work_capacity(5000);
        let buffer = BatchBuffer::new(options);

        assert_eq!(buffer.max_batch_size(), 100);
        assert_eq!(buffer.batch_timeout(), Duration::from_millis(500));
        assert_eq!(buffer.pending_work_capacity(), 5000);
    }

    #[test]
    fn test_batch_buffer_stats_shared() {
        let buffer = BatchBuffer::with_defaults();
        let stats1 = Arc::clone(buffer.stats());
        let stats2 = Arc::clone(buffer.stats());

        buffer.add(Event::new()).unwrap();

        // Both references should see the same stats
        assert_eq!(stats1.snapshot().events_added, 1);
        assert_eq!(stats2.snapshot().events_added, 1);
    }

    // =====================================================
    // BatchBuffer Debug Tests
    // =====================================================

    #[test]
    fn test_batch_buffer_debug() {
        let buffer = BatchBuffer::with_defaults();
        let debug_str = format!("{buffer:?}");
        assert!(debug_str.contains("BatchBuffer"));
    }

    // =====================================================
    // Edge Cases
    // =====================================================

    #[test]
    fn test_batch_buffer_batch_size_one() {
        let options = TransmissionOptions::default().with_max_batch_size(1);
        let buffer = BatchBuffer::new(options);

        // Every add should return a batch
        let batch = buffer.add(Event::new()).unwrap();
        assert!(batch.is_some());
        assert_eq!(batch.unwrap().len(), 1);

        let batch = buffer.add(Event::new()).unwrap();
        assert!(batch.is_some());
        assert_eq!(batch.unwrap().len(), 1);
    }

    #[test]
    fn test_batch_buffer_add_preserves_event_data() {
        let options = TransmissionOptions::default().with_max_batch_size(2);
        let buffer = BatchBuffer::new(options);

        let mut event1 = Event::new();
        event1.add_field("index", 1);

        let mut event2 = Event::new();
        event2.add_field("index", 2);

        buffer.add(event1).unwrap();
        let batch = buffer.add(event2).unwrap().unwrap();

        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch[0].data.get("index"),
            Some(&serde_json::Value::Number(1.into()))
        );
        assert_eq!(
            batch[1].data.get("index"),
            Some(&serde_json::Value::Number(2.into()))
        );
    }
}
