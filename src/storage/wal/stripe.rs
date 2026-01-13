//! WAL stripe - a single partition in the striped WAL architecture.
//!
//! Each stripe contains:
//! - A lock-free ring buffer for pending entries
//! - Per-stripe metrics for monitoring
//!
//! Writers are assigned to stripes using thread-local affinity to minimize
//! cache contention. The flush coordinator drains all stripes and merges
//! entries in LSN order.
//!
//! # Thread Safety
//!
//! Multiple threads can append to the same stripe concurrently (the ring
//! buffer handles synchronization). Only the flush coordinator should drain.

use std::sync::atomic::{AtomicU64, Ordering};

use super::LSN;
use super::ring_buffer::{
    CompletionHandle, DEFAULT_RING_BUFFER_CAPACITY, PendingEntry, WalRingBuffer,
};

/// A single stripe in the concurrent WAL.
///
/// Each stripe has its own ring buffer and metrics. Writers are assigned
/// to stripes to distribute contention across multiple buffers.
pub struct WalStripe {
    /// The stripe's unique identifier (0-indexed).
    id: usize,
    /// Lock-free ring buffer for pending entries.
    ring_buffer: WalRingBuffer,
    /// Total entries appended to this stripe.
    append_count: AtomicU64,
    /// Total bytes appended to this stripe.
    bytes_appended: AtomicU64,
}

impl WalStripe {
    /// Create a new stripe with default ring buffer capacity.
    pub fn new(id: usize) -> Self {
        Self::with_capacity(id, DEFAULT_RING_BUFFER_CAPACITY)
    }

    /// Create a new stripe with custom ring buffer capacity.
    pub fn with_capacity(id: usize, capacity: usize) -> Self {
        Self {
            id,
            ring_buffer: WalRingBuffer::new(capacity),
            append_count: AtomicU64::new(0),
            bytes_appended: AtomicU64::new(0),
        }
    }

    /// Get the stripe's ID.
    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    /// Append an entry to this stripe (async mode - no waiting).
    ///
    /// # Returns
    ///
    /// - `Ok(())` if appended successfully
    /// - `Err(entry)` if buffer is full or closed
    pub fn append_async(&self, lsn: LSN, data: Vec<u8>) -> Result<(), PendingEntry> {
        let entry = PendingEntry::new_async(lsn, data);
        self.append_entry(entry)
    }

    /// Append an entry to this stripe (sync mode - returns handle to wait on).
    ///
    /// # Returns
    ///
    /// - `Ok(handle)` - Handle to wait for durability
    /// - `Err(entry)` if buffer is full or closed
    pub fn append_sync(&self, lsn: LSN, data: Vec<u8>) -> Result<CompletionHandle, PendingEntry> {
        let (entry, handle) = PendingEntry::new_sync(lsn, data);
        self.append_entry(entry).map(|()| handle)
    }

    /// Append an entry to this stripe (sync mode with blocking - returns handle to wait on).
    ///
    /// This method blocks until space is available in the ring buffer.
    ///
    /// # Returns
    ///
    /// - `Ok(handle)` - Handle to wait for durability
    /// - `Err(entry)` if buffer is closed
    pub fn append_sync_blocking(
        &self,
        lsn: LSN,
        data: Vec<u8>,
    ) -> Result<CompletionHandle, PendingEntry> {
        let (entry, handle) = PendingEntry::new_sync(lsn, data);
        self.append_entry_blocking(entry).map(|()| handle)
    }

    /// Append an entry to this stripe (blocking mode - waits for space).
    ///
    /// This method blocks until space is available in the ring buffer.
    /// It uses exponential backoff with sleep to avoid spinning.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if appended successfully (after waiting if needed)
    /// - `Err(entry)` if buffer is closed
    pub fn append_blocking(&self, lsn: LSN, data: Vec<u8>) -> Result<(), PendingEntry> {
        let entry = PendingEntry::new_async(lsn, data);
        self.append_entry_blocking(entry)
    }

    /// Append a pre-constructed entry (non-blocking).
    fn append_entry(&self, entry: PendingEntry) -> Result<(), PendingEntry> {
        let data_len = entry.data.len();

        match self.ring_buffer.try_append(entry) {
            Ok(()) => {
                self.append_count.fetch_add(1, Ordering::Relaxed);
                self.bytes_appended
                    .fetch_add(data_len as u64, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Append a pre-constructed entry (blocking - waits for space).
    fn append_entry_blocking(&self, entry: PendingEntry) -> Result<(), PendingEntry> {
        let data_len = entry.data.len();

        match self.ring_buffer.append_blocking(entry) {
            Ok(()) => {
                self.append_count.fetch_add(1, Ordering::Relaxed);
                self.bytes_appended
                    .fetch_add(data_len as u64, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Drain all pending entries from this stripe.
    ///
    /// This should only be called by the flush coordinator.
    pub fn drain(&self) -> Vec<PendingEntry> {
        self.ring_buffer.drain()
    }

    /// Check if the stripe's ring buffer is approximately empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ring_buffer.is_empty_approx()
    }

    /// Get approximate number of pending entries.
    #[inline]
    pub fn pending_count(&self) -> usize {
        self.ring_buffer.len_approx()
    }

    /// Get total entries appended to this stripe.
    #[inline]
    pub fn total_appends(&self) -> u64 {
        self.append_count.load(Ordering::Relaxed)
    }

    /// Get total bytes appended to this stripe.
    #[inline]
    pub fn total_bytes(&self) -> u64 {
        self.bytes_appended.load(Ordering::Relaxed)
    }

    /// Close the stripe, preventing new appends.
    pub fn close(&self) {
        self.ring_buffer.close();
    }

    /// Check if the stripe is closed.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.ring_buffer.is_closed()
    }
}

/// Metrics for a WAL stripe.
#[derive(Debug, Clone)]
pub struct StripeMetrics {
    /// Stripe ID.
    pub id: usize,
    /// Total entries appended.
    pub total_appends: u64,
    /// Total bytes appended.
    pub total_bytes: u64,
    /// Current pending entries.
    pub pending_count: usize,
}

impl WalStripe {
    /// Get current metrics for this stripe.
    pub fn metrics(&self) -> StripeMetrics {
        StripeMetrics {
            id: self.id,
            total_appends: self.total_appends(),
            total_bytes: self.total_bytes(),
            pending_count: self.pending_count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    // ============================================================
    // TDD Tests - Written FIRST to define expected behavior
    // ============================================================

    #[test]
    fn test_stripe_creation() {
        let stripe = WalStripe::new(0);
        assert_eq!(stripe.id(), 0);
        assert!(stripe.is_empty());
        assert!(!stripe.is_closed());
    }

    #[test]
    fn test_stripe_with_custom_capacity() {
        let stripe = WalStripe::with_capacity(5, 64);
        assert_eq!(stripe.id(), 5);
        // Capacity rounds to next power of 2
    }

    #[test]
    fn test_stripe_append_async() {
        let stripe = WalStripe::new(0);

        let result = stripe.append_async(LSN(1), vec![1, 2, 3]);
        assert!(result.is_ok());

        assert_eq!(stripe.total_appends(), 1);
        assert_eq!(stripe.total_bytes(), 3);
        assert_eq!(stripe.pending_count(), 1);
    }

    #[test]
    fn test_stripe_append_sync() {
        let stripe = WalStripe::new(0);

        let result = stripe.append_sync(LSN(1), vec![1, 2, 3, 4]);
        assert!(result.is_ok());

        let handle = result.unwrap();
        assert!(!handle.is_complete());

        assert_eq!(stripe.total_appends(), 1);
        assert_eq!(stripe.total_bytes(), 4);
    }

    #[test]
    fn test_stripe_drain() {
        let stripe = WalStripe::new(0);

        // Append several entries
        for i in 0..5 {
            stripe.append_async(LSN(i), vec![i as u8]).unwrap();
        }

        assert_eq!(stripe.pending_count(), 5);

        // Drain
        let entries = stripe.drain();
        assert_eq!(entries.len(), 5);
        assert!(stripe.is_empty());

        // Verify LSNs
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, LSN(i as u64));
        }
    }

    #[test]
    fn test_stripe_close() {
        let stripe = WalStripe::new(0);

        stripe.close();
        assert!(stripe.is_closed());

        // Appends should fail after close
        let result = stripe.append_async(LSN(1), vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn test_stripe_metrics() {
        let stripe = WalStripe::new(42);

        stripe.append_async(LSN(1), vec![1, 2, 3]).unwrap();
        stripe.append_async(LSN(2), vec![4, 5]).unwrap();

        let metrics = stripe.metrics();
        assert_eq!(metrics.id, 42);
        assert_eq!(metrics.total_appends, 2);
        assert_eq!(metrics.total_bytes, 5);
        assert_eq!(metrics.pending_count, 2);
    }

    #[test]
    fn test_stripe_concurrent_appends() {
        let stripe = Arc::new(WalStripe::with_capacity(0, 1024));
        let num_threads = 4;
        let appends_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let stripe = Arc::clone(&stripe);
                thread::spawn(move || {
                    for i in 0..appends_per_thread {
                        let lsn = LSN((t * appends_per_thread + i) as u64);
                        stripe.append_async(lsn, vec![t as u8]).unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            stripe.total_appends(),
            (num_threads * appends_per_thread) as u64
        );
    }

    #[test]
    fn test_stripe_sync_completion() {
        let stripe = WalStripe::new(0);

        let handle = stripe.append_sync(LSN(1), vec![1]).unwrap();

        // Drain and notify
        let entries = stripe.drain();
        assert_eq!(entries.len(), 1);

        // Notify completion
        entries[0].notify_completion();

        // Handle should now be complete
        assert!(handle.is_complete());
        assert!(handle.wait().is_ok());
    }

    #[test]
    fn test_stripe_sync_error_notification() {
        let stripe = WalStripe::new(0);

        let handle = stripe.append_sync(LSN(1), vec![1]).unwrap();

        // Drain and notify with error
        let entries = stripe.drain();
        entries[0].notify_error("test error");

        // Handle should return error
        assert!(handle.is_complete());
        let result = handle.wait();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "test error");
    }

    #[test]
    fn test_stripe_interleaved_append_drain() {
        let stripe = WalStripe::new(0);

        for cycle in 0..5 {
            // Append
            for i in 0..3 {
                stripe
                    .append_async(LSN((cycle * 3 + i) as u64), vec![])
                    .unwrap();
            }

            // Drain
            let entries = stripe.drain();
            assert_eq!(entries.len(), 3);

            // Metrics accumulate
            assert_eq!(stripe.total_appends(), ((cycle + 1) * 3) as u64);
        }
    }
}
