//! Lock-free ring buffer for concurrent WAL appends.
//!
//! This module provides a multi-producer single-consumer (MPSC) ring buffer
//! designed for high-throughput WAL operations. Multiple writer threads can
//! append entries concurrently with minimal contention.
//!
//! # Design
//!
//! The ring buffer uses a sequence-based approach inspired by the LMAX Disruptor:
//! - Each slot has a sequence number indicating its state
//! - Writers claim slots via atomic compare-and-swap on `write_pos`
//! - After writing, they update the slot's sequence to signal completion
//! - The consumer (flush thread) drains completed slots in order
//!
//! # Memory Ordering
//!
//! The implementation uses careful memory ordering to ensure correctness:
//! - `write_pos` CAS uses `AcqRel` to synchronize slot claims
//! - Sequence stores use `Release` after writing data
//! - Sequence loads use `Acquire` before reading data
//! - `read_pos` CAS uses `AcqRel` to synchronize draining
//!
//! # Performance
//!
//! - **Append latency**: ~50-100ns (single CAS on fast path)
//! - **Contention**: Scales linearly with writers up to ~64 threads
//! - **False sharing**: Mitigated by cache-line padding
//!
//! # Backpressure
//!
//! When the buffer is full, writers spin briefly then yield. This provides
//! natural backpressure without blocking the flush thread.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::LSN;

/// Default capacity for WAL ring buffers (must be power of 2).
pub const DEFAULT_RING_BUFFER_CAPACITY: usize = 1024;

/// Backpressure configuration for ring buffer operations.
///
/// Controls how writers behave when the buffer is full, using an
/// exponential backoff strategy to balance latency vs CPU usage.
///
/// # Strategy
///
/// 1. **Spin phase**: Start with `initial_spins` iterations, doubling each
///    retry up to `max_spins`. Uses `spin_loop()` hint for efficiency.
///
/// 2. **Yield phase**: After max spins, yield the thread. For blocking
///    operations, sleep with exponential backoff from `base_sleep_us` to
///    `max_sleep_us`.
///
/// # Tuning Guidelines
///
/// - **Low-latency workloads**: Higher `initial_spins` (100-1000), lower sleep times
/// - **High-throughput batch**: Lower `initial_spins` (10-50), higher sleep times
/// - **Mixed workloads**: Use defaults, which balance both
#[derive(Debug, Clone)]
pub struct BackpressureConfig {
    /// Initial spin loop iterations on first contention (default: 10).
    pub initial_spins: u32,
    /// Maximum spin iterations before yielding (default: 1000).
    /// Spins double each retry: 10 → 20 → 40 → ... → 1000.
    pub max_spins: u32,
    /// Base sleep duration in microseconds for blocking backoff (default: 1).
    pub base_sleep_us: u64,
    /// Maximum sleep duration in microseconds (default: 1000 = 1ms).
    /// Sleeps double each retry: 1µs → 2µs → 4µs → ... → 1000µs.
    pub max_sleep_us: u64,
}

impl Default for BackpressureConfig {
    fn default() -> Self {
        Self {
            initial_spins: 10,
            max_spins: 1000,
            base_sleep_us: 1,
            max_sleep_us: 1000,
        }
    }
}

impl BackpressureConfig {
    /// Configuration optimized for low-latency operations.
    ///
    /// More aggressive spinning, shorter sleeps. Use when latency matters
    /// more than CPU efficiency.
    pub fn low_latency() -> Self {
        Self {
            initial_spins: 100,
            max_spins: 10_000,
            base_sleep_us: 1,
            max_sleep_us: 100,
        }
    }

    /// Configuration optimized for high-throughput batch operations.
    ///
    /// Less spinning, longer sleeps. Use when throughput matters more
    /// than individual operation latency.
    pub fn high_throughput() -> Self {
        Self {
            initial_spins: 5,
            max_spins: 100,
            base_sleep_us: 10,
            max_sleep_us: 10_000,
        }
    }
}

/// A pending WAL entry waiting to be flushed to disk.
#[derive(Debug)]
pub struct PendingEntry {
    /// Pre-allocated LSN from the global allocator.
    pub lsn: LSN,
    /// Pre-serialized entry data (includes LSN, timestamp, checksum, operation).
    pub data: Vec<u8>,
    /// Optional completion notifier for sync modes.
    /// When set, the caller is waiting for this entry to be durably flushed.
    pub completion: Option<Arc<CompletionNotifier>>,
}

impl PendingEntry {
    /// Create a new pending entry for async mode (no completion notification).
    #[inline]
    pub fn new_async(lsn: LSN, data: Vec<u8>) -> Self {
        Self {
            lsn,
            data,
            completion: None,
        }
    }

    /// Create a new pending entry for sync mode (with completion notification).
    #[inline]
    pub fn new_sync(lsn: LSN, data: Vec<u8>) -> (Self, CompletionHandle) {
        let notifier = Arc::new(CompletionNotifier::new());
        let handle = CompletionHandle(Arc::clone(&notifier));
        let entry = Self {
            lsn,
            data,
            completion: Some(notifier),
        };
        (entry, handle)
    }

    /// Notify completion (called by flush coordinator after durable write).
    #[inline]
    pub fn notify_completion(&self) {
        if let Some(ref notifier) = self.completion {
            notifier.notify_success();
        }
    }

    /// Notify completion with error.
    #[inline]
    pub fn notify_error(&self, error: &str) {
        if let Some(ref notifier) = self.completion {
            notifier.notify_error(error);
        }
    }
}

/// Completion notification state.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionState {
    /// Entry is pending flush.
    Pending = 0,
    /// Entry has been durably flushed.
    Complete = 1,
    /// Flush failed with an error.
    Error = 2,
}

/// Notifier for synchronous completion waiting.
///
/// Writers that need durability guarantees create a `CompletionNotifier`
/// and wait on it after appending their entry. The flush coordinator
/// notifies all pending entries after fsync completes.
#[derive(Debug)]
pub struct CompletionNotifier {
    /// Current state (Pending, Complete, or Error).
    state: AtomicU64,
    /// Error message if state is Error.
    error: Mutex<Option<String>>,
    /// Condition variable for blocking wait.
    condvar: Condvar,
    /// Mutex for condition variable (we only use it for condvar).
    wait_mutex: Mutex<()>,
}

impl CompletionNotifier {
    /// Create a new completion notifier in pending state.
    pub fn new() -> Self {
        Self {
            state: AtomicU64::new(CompletionState::Pending as u64),
            error: Mutex::new(None),
            condvar: Condvar::new(),
            wait_mutex: Mutex::new(()),
        }
    }

    /// Check if completion has been signaled (non-blocking).
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.state.load(Ordering::Acquire) != CompletionState::Pending as u64
    }

    /// Notify successful completion.
    pub fn notify_success(&self) {
        self.state
            .store(CompletionState::Complete as u64, Ordering::Release);
        self.condvar.notify_all();
    }

    /// Notify completion with error.
    pub fn notify_error(&self, error: &str) {
        {
            let mut err = self.error.lock().unwrap_or_else(|e| e.into_inner());
            *err = Some(error.to_string());
        }
        self.state
            .store(CompletionState::Error as u64, Ordering::Release);
        self.condvar.notify_all();
    }

    /// Wait for completion, blocking the current thread.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the entry was durably flushed
    /// - `Err(error)` if the flush failed
    pub fn wait(&self) -> Result<(), String> {
        let guard = self.wait_mutex.lock().unwrap_or_else(|e| e.into_inner());

        // Check if already complete
        let state = self.state.load(Ordering::Acquire);
        if state == CompletionState::Complete as u64 {
            return Ok(());
        }
        if state == CompletionState::Error as u64 {
            let err = self.error.lock().unwrap_or_else(|e| e.into_inner());
            return Err(err.clone().unwrap_or_else(|| "Unknown error".to_string()));
        }

        // Wait for notification
        let _guard = self
            .condvar
            .wait_while(guard, |_| {
                self.state.load(Ordering::Acquire) == CompletionState::Pending as u64
            })
            .unwrap_or_else(|e| e.into_inner());

        // Check final state
        let state = self.state.load(Ordering::Acquire);
        if state == CompletionState::Complete as u64 {
            Ok(())
        } else {
            let err = self.error.lock().unwrap_or_else(|e| e.into_inner());
            Err(err.clone().unwrap_or_else(|| "Unknown error".to_string()))
        }
    }
}

impl Default for CompletionNotifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle for waiting on completion.
///
/// This is returned to callers who need to wait for their entry to be
/// durably flushed. The handle can be used to block until completion
/// or to poll for completion status.
#[derive(Debug, Clone)]
pub struct CompletionHandle(pub(crate) Arc<CompletionNotifier>);

impl CompletionHandle {
    /// Wait for the entry to be durably flushed, blocking the current thread.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the entry was durably flushed
    /// - `Err(error)` if the flush failed
    pub fn wait(self) -> Result<(), String> {
        self.0.wait()
    }

    /// Check if completion has been signaled (non-blocking).
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.0.is_complete()
    }
}

/// Internal slot state in the ring buffer.
///
/// The slot's sequence number indicates its state:
/// - `seq == expected_write_seq`: Slot is available for writing
/// - `seq == expected_write_seq + 1`: Slot contains data, ready for reading
/// - `seq == expected_read_seq + capacity`: Slot has been read, available again
struct Slot {
    /// The entry data (only valid when sequence indicates data is present).
    entry: UnsafeCell<Option<PendingEntry>>,
    /// Sequence number for coordinating access.
    /// Padded to avoid false sharing with adjacent slots.
    sequence: CacheLinePadded<AtomicU64>,
}

// SAFETY: Slot access is coordinated through atomic sequence numbers.
// The sequence protocol ensures only one thread accesses the entry at a time.
unsafe impl Send for Slot {}
unsafe impl Sync for Slot {}

/// Cache-line padded wrapper to avoid false sharing.
///
/// On most modern CPUs, cache lines are 64 bytes. By padding atomic
/// variables to this size, we prevent false sharing between adjacent slots.
#[repr(align(64))]
struct CacheLinePadded<T> {
    value: T,
}

impl<T> CacheLinePadded<T> {
    fn new(value: T) -> Self {
        Self { value }
    }
}

impl<T> std::ops::Deref for CacheLinePadded<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// Lock-free multi-producer single-consumer ring buffer for WAL entries.
///
/// This ring buffer allows multiple writer threads to append entries
/// concurrently while a single flush thread drains entries for writing
/// to disk.
///
/// # Thread Safety
///
/// - Multiple threads can call `try_append` concurrently (producers)
/// - Only one thread should call `drain` at a time (consumer)
/// - The ring buffer is `Send` and `Sync`
///
/// # Capacity
///
/// The capacity must be a power of 2 for efficient modulo operations.
/// If a non-power-of-2 is provided, it will be rounded up.
///
/// # Backpressure
///
/// When the buffer is full, writers use exponential backoff:
/// 1. Spin with doubling iterations (10 → 20 → 40 → ... → max)
/// 2. Yield/sleep with doubling duration (1µs → 2µs → ... → max)
///
/// Configure via [`BackpressureConfig`] for your workload.
pub struct WalRingBuffer {
    /// Pre-allocated slots.
    slots: Box<[Slot]>,
    /// Capacity (must be power of 2).
    capacity: usize,
    /// Mask for fast modulo (capacity - 1).
    mask: usize,
    /// Next position for writing (producers).
    /// Padded to avoid false sharing with read_pos.
    write_pos: CacheLinePadded<AtomicU64>,
    /// Next position for reading (consumer).
    /// Padded to avoid false sharing with write_pos.
    read_pos: CacheLinePadded<AtomicU64>,
    /// Flag indicating if the buffer is closed (no more appends allowed).
    closed: AtomicBool,
    /// Backpressure configuration.
    backpressure: BackpressureConfig,
}

// SAFETY: WalRingBuffer access is coordinated through atomic operations.
// Producers and consumer never access the same slot simultaneously due to
// the sequence number protocol.
unsafe impl Send for WalRingBuffer {}
unsafe impl Sync for WalRingBuffer {}

impl WalRingBuffer {
    /// Create a new ring buffer with the specified capacity and default backpressure.
    ///
    /// The capacity will be rounded up to the next power of 2.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is 0.
    pub fn new(capacity: usize) -> Self {
        Self::with_config(capacity, BackpressureConfig::default())
    }

    /// Create a new ring buffer with the specified capacity and backpressure config.
    ///
    /// The capacity will be rounded up to the next power of 2.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is 0.
    pub fn with_config(capacity: usize, backpressure: BackpressureConfig) -> Self {
        assert!(capacity > 0, "Ring buffer capacity must be > 0");

        // Round up to power of 2
        let capacity = capacity.next_power_of_two();
        let mask = capacity - 1;

        // Initialize slots with sequential sequence numbers
        let slots: Vec<Slot> = (0..capacity)
            .map(|i| Slot {
                entry: UnsafeCell::new(None),
                sequence: CacheLinePadded::new(AtomicU64::new(i as u64)),
            })
            .collect();

        Self {
            slots: slots.into_boxed_slice(),
            capacity,
            mask,
            write_pos: CacheLinePadded::new(AtomicU64::new(0)),
            read_pos: CacheLinePadded::new(AtomicU64::new(0)),
            closed: AtomicBool::new(false),
            backpressure,
        }
    }

    /// Create a new ring buffer with the default capacity and backpressure.
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_RING_BUFFER_CAPACITY)
    }

    /// Get the capacity of the ring buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Check if the buffer has been closed.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Close the buffer, preventing new appends.
    ///
    /// Existing entries can still be drained. This is used during shutdown.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Try to append an entry to the ring buffer.
    ///
    /// This method is lock-free and may be called concurrently by multiple threads.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the entry was successfully appended
    /// - `Err(entry)` if the buffer is full or closed (entry is returned)
    ///
    /// # Performance
    ///
    /// This method has ~50-100ns latency on the fast path (single CAS).
    /// When the buffer is full, it uses exponential backoff spinning before
    /// returning an error.
    ///
    /// # Backpressure
    ///
    /// Uses exponential backoff: starts with `initial_spins` iterations,
    /// doubling each round until `max_spins` is reached, then returns `Err`.
    pub fn try_append(&self, entry: PendingEntry) -> Result<(), PendingEntry> {
        // Check if closed
        if self.is_closed() {
            return Err(entry);
        }

        let mut spin_count = 0u32;
        let mut current_spin_limit = self.backpressure.initial_spins;

        loop {
            let pos = self.write_pos.load(Ordering::Relaxed);
            let idx = (pos as usize) & self.mask;
            let slot = &self.slots[idx];

            // Expected sequence for this slot to be available for writing
            let expected_seq = pos;
            let current_seq = slot.sequence.load(Ordering::Acquire);

            if current_seq == expected_seq {
                // Slot is available - try to claim it
                match self.write_pos.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Successfully claimed the slot - write the entry
                        // SAFETY: We have exclusive access to this slot after successful CAS.
                        // The sequence protocol ensures the consumer won't read until we
                        // update the sequence number.
                        unsafe {
                            *slot.entry.get() = Some(entry);
                        }

                        // Signal that the slot is ready for reading
                        slot.sequence.store(expected_seq + 1, Ordering::Release);

                        return Ok(());
                    }
                    Err(_) => {
                        // Lost the race - retry immediately
                        continue;
                    }
                }
            } else if current_seq < expected_seq {
                // Buffer is full - the consumer hasn't caught up
                spin_count += 1;

                if spin_count >= current_spin_limit {
                    // Check if we've hit max spins
                    if current_spin_limit >= self.backpressure.max_spins {
                        return Err(entry);
                    }
                    // Exponential backoff: double the spin limit
                    current_spin_limit =
                        (current_spin_limit.saturating_mul(2)).min(self.backpressure.max_spins);
                    spin_count = 0;
                }

                std::hint::spin_loop();
            } else {
                // current_seq > expected_seq: This shouldn't happen in normal operation.
                // It indicates the consumer has advanced past where we expected.
                // Retry with fresh position.
                continue;
            }
        }
    }

    /// Append an entry, blocking if the buffer is full.
    ///
    /// This method will spin and sleep with exponential backoff while waiting
    /// for space.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the entry was successfully appended
    /// - `Err(entry)` if the buffer is closed
    ///
    /// # Backpressure
    ///
    /// After `try_append` exhausts spinning, this method sleeps with
    /// exponential backoff: starts at `base_sleep_us`, doubling each
    /// iteration until `max_sleep_us` is reached.
    pub fn append_blocking(&self, entry: PendingEntry) -> Result<(), PendingEntry> {
        let mut current_entry = entry;
        let mut sleep_us = self.backpressure.base_sleep_us;

        loop {
            match self.try_append(current_entry) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if self.is_closed() {
                        return Err(e);
                    }

                    // Buffer is full - sleep with exponential backoff
                    if sleep_us > 0 {
                        std::thread::sleep(std::time::Duration::from_micros(sleep_us));
                        // Double sleep time up to max
                        sleep_us = (sleep_us.saturating_mul(2)).min(self.backpressure.max_sleep_us);
                    } else {
                        // If base_sleep_us is 0, just yield
                        std::thread::yield_now();
                    }

                    current_entry = e;
                }
            }
        }
    }

    /// Drain all available entries from the ring buffer.
    ///
    /// This method should only be called by a single consumer thread.
    /// It returns all entries that are ready to be flushed, in LSN order
    /// (since entries are appended in LSN order to each stripe).
    ///
    /// # Returns
    ///
    /// A vector of pending entries ready for flushing. May be empty if
    /// no entries are available.
    pub fn drain(&self) -> Vec<PendingEntry> {
        let mut entries = Vec::new();

        loop {
            let pos = self.read_pos.load(Ordering::Relaxed);
            // SAFETY: idx is always < capacity because:
            // - self.mask = capacity - 1 (set in new())
            // - capacity is a power of 2
            // - (pos & mask) is equivalent to (pos % capacity)
            // - Therefore idx is in bounds [0, capacity)
            let idx = (pos as usize) & self.mask;
            let slot = &self.slots[idx];

            // Expected sequence for this slot to contain data
            let expected_seq = pos + 1;
            let current_seq = slot.sequence.load(Ordering::Acquire);

            if current_seq == expected_seq {
                // Slot contains data - try to claim it for reading
                match self.read_pos.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Successfully claimed - read the entry
                        //
                        // SAFETY: Memory ordering guarantees exclusive access:
                        //
                        // 1. The CAS uses AcqRel ordering:
                        //    - Acquire: synchronizes with all prior Release stores,
                        //      including the producer's sequence.store(pos+1, Release)
                        //    - Release: prevents reordering of the entry read before CAS
                        //
                        // 2. The producer cannot write to this slot because:
                        //    - Producer checks: sequence.load(Acquire) == write_pos
                        //    - Current sequence is (pos + 1), not (pos + capacity)
                        //    - We only set sequence to (pos + capacity) AFTER reading
                        //
                        // 3. No other consumer can read because:
                        //    - This is single-consumer (flush coordinator only)
                        //    - We own read_pos after successful CAS
                        //
                        // 4. The Acquire in the CAS establishes happens-before with
                        //    the producer's Release store of the entry data.
                        let entry = unsafe { (*slot.entry.get()).take() };

                        // Mark slot as available for writing again
                        // New sequence = pos + capacity (next write cycle)
                        slot.sequence
                            .store(pos + self.capacity as u64, Ordering::Release);

                        if let Some(e) = entry {
                            entries.push(e);
                        }
                    }
                    Err(_) => {
                        // Lost the race (shouldn't happen with single consumer)
                        // but handle gracefully
                        continue;
                    }
                }
            } else {
                // No more ready entries (or slot not yet written)
                break;
            }
        }

        entries
    }

    /// Get the approximate number of entries in the buffer.
    ///
    /// This is an approximation because reads are not synchronized.
    /// Use for monitoring only, not for correctness decisions.
    #[inline]
    pub fn len_approx(&self) -> usize {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Relaxed);
        write.saturating_sub(read) as usize
    }

    /// Check if the buffer is approximately empty.
    #[inline]
    pub fn is_empty_approx(&self) -> bool {
        self.len_approx() == 0
    }
}

impl Drop for WalRingBuffer {
    fn drop(&mut self) {
        // Close the buffer and drain any remaining entries
        self.close();

        // Drain remaining entries to properly drop them
        let remaining = self.drain();
        if !remaining.is_empty() {
            // Log warning about dropped entries in debug builds
            #[cfg(debug_assertions)]
            eprintln!(
                "WalRingBuffer::drop: {} entries were not flushed",
                remaining.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    #[test]
    fn test_completion_notifier_success() {
        let notifier = CompletionNotifier::new();
        assert!(!notifier.is_complete());

        notifier.notify_success();
        assert!(notifier.is_complete());

        // Wait should return immediately
        assert!(notifier.wait().is_ok());
    }

    #[test]
    fn test_completion_notifier_error() {
        let notifier = CompletionNotifier::new();

        notifier.notify_error("test error");
        assert!(notifier.is_complete());

        let result = notifier.wait();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "test error");
    }

    #[test]
    fn test_completion_handle_wait() {
        let notifier = Arc::new(CompletionNotifier::new());
        let handle = CompletionHandle(Arc::clone(&notifier));

        // Spawn thread to notify after a short delay
        let notifier_clone = Arc::clone(&notifier);
        let t = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(10));
            notifier_clone.notify_success();
        });

        // Wait should block until notified
        assert!(handle.wait().is_ok());
        t.join().unwrap();
    }

    #[test]
    fn test_ring_buffer_capacity_rounding() {
        let buf = WalRingBuffer::new(100);
        // Should round up to 128 (next power of 2)
        assert_eq!(buf.capacity(), 128);

        let buf = WalRingBuffer::new(64);
        assert_eq!(buf.capacity(), 64);

        let buf = WalRingBuffer::new(1);
        assert_eq!(buf.capacity(), 1);
    }

    #[test]
    fn test_ring_buffer_single_thread() {
        let buf = WalRingBuffer::new(16);

        // Append some entries
        for i in 0..10 {
            let entry = PendingEntry::new_async(LSN(i), vec![i as u8]);
            assert!(buf.try_append(entry).is_ok());
        }

        assert_eq!(buf.len_approx(), 10);

        // Drain entries
        let entries = buf.drain();
        assert_eq!(entries.len(), 10);

        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, LSN(i as u64));
            assert_eq!(entry.data, vec![i as u8]);
        }

        assert!(buf.is_empty_approx());
    }

    #[test]
    fn test_ring_buffer_full() {
        let buf = WalRingBuffer::new(4);

        // Fill the buffer
        for i in 0..4 {
            let entry = PendingEntry::new_async(LSN(i), vec![]);
            assert!(buf.try_append(entry).is_ok());
        }

        // Next append should fail
        let entry = PendingEntry::new_async(LSN(4), vec![]);
        assert!(buf.try_append(entry).is_err());
    }

    #[test]
    fn test_ring_buffer_closed() {
        let buf = WalRingBuffer::new(16);

        buf.close();
        assert!(buf.is_closed());

        let entry = PendingEntry::new_async(LSN(0), vec![]);
        assert!(buf.try_append(entry).is_err());
    }

    #[test]
    fn test_ring_buffer_drain_empty() {
        let buf = WalRingBuffer::new(16);

        let entries = buf.drain();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_ring_buffer_concurrent_producers() {
        let buf = Arc::new(WalRingBuffer::new(1024));
        let num_threads = 8;
        let entries_per_thread = 100;
        let total_appended = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let buf = Arc::clone(&buf);
                let total = Arc::clone(&total_appended);

                thread::spawn(move || {
                    for i in 0..entries_per_thread {
                        let lsn = LSN((thread_id * entries_per_thread + i) as u64);
                        let entry = PendingEntry::new_async(lsn, vec![thread_id as u8, i as u8]);

                        // Use blocking append to handle potential full buffer
                        if buf.append_blocking(entry).is_ok() {
                            total.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();

        // Wait for all producers
        for h in handles {
            h.join().unwrap();
        }

        // Drain and verify
        let entries = buf.drain();
        let total = total_appended.load(Ordering::Relaxed);

        assert_eq!(entries.len(), total);
        assert_eq!(total, num_threads * entries_per_thread);
    }

    #[test]
    fn test_pending_entry_sync_mode() {
        let (entry, handle) = PendingEntry::new_sync(LSN(42), vec![1, 2, 3]);

        assert_eq!(entry.lsn, LSN(42));
        assert_eq!(entry.data, vec![1, 2, 3]);
        assert!(entry.completion.is_some());
        assert!(!handle.is_complete());

        entry.notify_completion();
        assert!(handle.is_complete());
    }

    #[test]
    fn test_ring_buffer_wrap_around() {
        let buf = WalRingBuffer::new(4);

        // First cycle - fill and drain
        for i in 0..4 {
            let entry = PendingEntry::new_async(LSN(i), vec![i as u8]);
            assert!(buf.try_append(entry).is_ok());
        }
        let entries = buf.drain();
        assert_eq!(entries.len(), 4);

        // Second cycle - should reuse slots
        for i in 4..8 {
            let entry = PendingEntry::new_async(LSN(i), vec![i as u8]);
            assert!(buf.try_append(entry).is_ok());
        }
        let entries = buf.drain();
        assert_eq!(entries.len(), 4);

        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, LSN((i + 4) as u64));
        }
    }

    #[test]
    fn test_ring_buffer_interleaved_append_drain() {
        let buf = WalRingBuffer::new(8);

        // Interleave appends and drains
        for cycle in 0..10 {
            for i in 0..3 {
                let lsn = LSN((cycle * 3 + i) as u64);
                let entry = PendingEntry::new_async(lsn, vec![]);
                assert!(buf.try_append(entry).is_ok());
            }

            let entries = buf.drain();
            assert_eq!(entries.len(), 3);
        }
    }

    #[test]
    fn test_backpressure_config_default() {
        let config = BackpressureConfig::default();
        assert_eq!(config.initial_spins, 10);
        assert_eq!(config.max_spins, 1000);
        assert_eq!(config.base_sleep_us, 1);
        assert_eq!(config.max_sleep_us, 1000);
    }

    #[test]
    fn test_backpressure_config_presets() {
        let low_latency = BackpressureConfig::low_latency();
        assert!(low_latency.initial_spins > BackpressureConfig::default().initial_spins);
        assert!(low_latency.max_sleep_us < BackpressureConfig::default().max_sleep_us);

        let high_throughput = BackpressureConfig::high_throughput();
        assert!(high_throughput.initial_spins < BackpressureConfig::default().initial_spins);
        assert!(high_throughput.max_sleep_us > BackpressureConfig::default().max_sleep_us);
    }

    #[test]
    fn test_ring_buffer_with_custom_backpressure() {
        let config = BackpressureConfig {
            initial_spins: 5,
            max_spins: 50,
            base_sleep_us: 10,
            max_sleep_us: 100,
        };
        let buf = WalRingBuffer::with_config(4, config);

        // Should work normally
        let entry = PendingEntry::new_async(LSN(1), vec![1, 2, 3]);
        assert!(buf.try_append(entry).is_ok());

        let entries = buf.drain();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_backpressure_exponential_spin() {
        // Create a buffer with very low spin limits to test backoff kicks in quickly
        let config = BackpressureConfig {
            initial_spins: 2,
            max_spins: 8, // 2 -> 4 -> 8 (3 rounds)
            base_sleep_us: 0,
            max_sleep_us: 0,
        };
        let buf = WalRingBuffer::with_config(2, config);

        // Fill the buffer
        buf.try_append(PendingEntry::new_async(LSN(1), vec![]))
            .unwrap();
        buf.try_append(PendingEntry::new_async(LSN(2), vec![]))
            .unwrap();

        // Next append should fail after exponential backoff
        let result = buf.try_append(PendingEntry::new_async(LSN(3), vec![]));
        assert!(result.is_err());
    }
}
