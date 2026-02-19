//! Global LSN (Log Sequence Number) allocator for concurrent WAL.
//!
//! The LSN allocator provides globally unique, monotonically increasing
//! sequence numbers for WAL entries. It is the **only** synchronization
//! point between concurrent writers - a single atomic increment.
//!
//! # Design
//!
//! - Uses `AtomicU64::fetch_add` for lock-free allocation
//! - Single allocation is O(1) with ~10-20ns latency
//! - Batch allocation amortizes atomic overhead for multi-operation transactions
//!
//! # Thread Safety
//!
//! The allocator is `Send` and `Sync`. Multiple threads can allocate LSNs
//! concurrently without any locking.
//!
//! # LSN Overflow (Theoretical Limitation)
//!
//! The allocator uses a `u64` counter with `fetch_add`, which will wrap around
//! to 0 after 2^64 allocations. This is a theoretical limitation:
//!
//! - At 1 million LSNs/second: overflow in ~584,542 years
//! - At 10 million LSNs/second: overflow in ~58,454 years
//! - At 100 million LSNs/second: overflow in ~5,845 years
//!
//! **Consequences of overflow**: LSN wraparound would cause duplicate LSNs,
//! breaking the uniqueness guarantee. Recovery would become ambiguous as
//! entries with "older" LSNs might actually be newer.
//!
//! **Mitigation**: For systems requiring true infinite operation, a database
//! restart with LSN reset (after full checkpoint) would be needed before
//! overflow. Monitoring `current()` against a threshold (e.g., `u64::MAX / 2`)
//! can provide early warning.

use std::sync::atomic::{AtomicU64, Ordering};

use super::LSN;

/// Global LSN allocator using atomic operations.
///
/// This is the single synchronization point for the concurrent WAL.
/// All writers allocate LSNs from this shared allocator, ensuring
/// global ordering of WAL entries.
pub struct LsnAllocator {
    /// Next LSN to allocate.
    next_lsn: AtomicU64,
}

impl LsnAllocator {
    /// Create a new LSN allocator starting at LSN 1.
    pub fn new() -> Self {
        Self {
            next_lsn: AtomicU64::new(1),
        }
    }

    /// Create a new LSN allocator starting at a specific LSN.
    ///
    /// Used during recovery to resume from the last known LSN.
    pub fn starting_at(lsn: LSN) -> Self {
        Self {
            next_lsn: AtomicU64::new(lsn.0),
        }
    }

    /// Allocate the next LSN atomically.
    ///
    /// This is a lock-free operation using `fetch_add`.
    ///
    /// # Memory Ordering
    ///
    /// Uses `Ordering::Relaxed` because:
    ///
    /// 1. **LSN uniqueness is guaranteed by the atomic counter itself**, not by
    ///    memory visibility. Each `fetch_add` returns a unique value.
    ///
    /// 2. **Happens-before relationships are established elsewhere**:
    ///    - The caller serializes data to a buffer (local operation)
    ///    - The caller writes to the ring buffer with `Release` ordering
    ///    - The flush coordinator reads with `Acquire` ordering
    ///    - This Release/Acquire pair provides the necessary synchronization
    ///
    /// 3. **The LSN is used for ordering, not synchronization**. The sort in
    ///    the flush coordinator uses the LSN values, which are correct
    ///    regardless of when they become visible to other threads.
    ///
    /// # Returns
    ///
    /// The allocated LSN. Each call returns a unique, monotonically
    /// increasing value.
    #[inline]
    pub fn allocate(&self) -> LSN {
        // Use fetch_update (CAS loop) to ensure we don't wrap state on overflow
        let lsn = self
            .next_lsn
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current == u64::MAX {
                    None // Overflow
                } else {
                    Some(current + 1)
                }
            })
            .expect("LSN Allocator Overflow");
        LSN(lsn)
    }

    /// Allocate a batch of consecutive LSNs atomically.
    ///
    /// This is more efficient than calling `allocate()` multiple times
    /// when a transaction has multiple operations.
    ///
    /// # Arguments
    ///
    /// * `count` - Number of LSNs to allocate (must be > 0)
    ///
    /// # Returns
    ///
    /// A tuple of (first_lsn, last_lsn) representing the allocated range.
    ///
    /// # Panics
    ///
    /// Panics if `count` is 0.
    /// Panics if the allocation would cause LSN overflow.
    #[inline]
    pub fn allocate_batch(&self, count: u64) -> (LSN, LSN) {
        assert!(count > 0, "Cannot allocate 0 LSNs");

        // Use fetch_update (CAS loop) to ensure we don't wrap state on overflow
        let first = self
            .next_lsn
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                // Check if `current + count` would overflow u64.
                if current > u64::MAX.saturating_sub(count) {
                    None
                } else {
                    Some(current + count)
                }
            })
            .expect("LSN Allocator Overflow");

        (LSN(first), LSN(first + count - 1))
    }

    /// Get the current (next to be allocated) LSN without allocating.
    ///
    /// This is useful for checkpointing and recovery.
    #[inline]
    pub fn current(&self) -> LSN {
        LSN(self.next_lsn.load(Ordering::Relaxed))
    }

    /// Set the next LSN to allocate.
    ///
    /// **Warning**: This should only be used during recovery to restore
    /// the allocator state. Using it during normal operation will cause
    /// duplicate LSNs.
    ///
    /// # Arguments
    ///
    /// * `lsn` - The next LSN to allocate
    pub fn set_next(&self, lsn: LSN) {
        self.next_lsn.store(lsn.0, Ordering::Relaxed);
    }
}

impl Default for LsnAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::thread;

    // ============================================================
    // TDD Tests - Written FIRST to define expected behavior
    // ============================================================

    #[test]
    fn test_allocator_starts_at_one() {
        let alloc = LsnAllocator::new();
        assert_eq!(alloc.current(), LSN(1));
    }

    #[test]
    fn test_allocator_starts_at_custom_lsn() {
        let alloc = LsnAllocator::starting_at(LSN(100));
        assert_eq!(alloc.current(), LSN(100));
    }

    #[test]
    fn test_single_allocation() {
        let alloc = LsnAllocator::new();

        let lsn1 = alloc.allocate();
        let lsn2 = alloc.allocate();
        let lsn3 = alloc.allocate();

        assert_eq!(lsn1, LSN(1));
        assert_eq!(lsn2, LSN(2));
        assert_eq!(lsn3, LSN(3));
        assert_eq!(alloc.current(), LSN(4));
    }

    #[test]
    fn test_batch_allocation() {
        let alloc = LsnAllocator::new();

        let (first, last) = alloc.allocate_batch(5);

        assert_eq!(first, LSN(1));
        assert_eq!(last, LSN(5));
        assert_eq!(alloc.current(), LSN(6));
    }

    #[test]
    fn test_batch_allocation_single() {
        let alloc = LsnAllocator::new();

        let (first, last) = alloc.allocate_batch(1);

        assert_eq!(first, LSN(1));
        assert_eq!(last, LSN(1));
    }

    #[test]
    #[should_panic(expected = "Cannot allocate 0 LSNs")]
    fn test_batch_allocation_zero_panics() {
        let alloc = LsnAllocator::new();
        let _ = alloc.allocate_batch(0);
    }

    #[test]
    fn test_set_next_lsn() {
        let alloc = LsnAllocator::new();

        alloc.set_next(LSN(1000));

        assert_eq!(alloc.current(), LSN(1000));
        assert_eq!(alloc.allocate(), LSN(1000));
        assert_eq!(alloc.allocate(), LSN(1001));
    }

    #[test]
    fn test_concurrent_allocation_unique() {
        let alloc = Arc::new(LsnAllocator::new());
        let num_threads = 8;
        let allocations_per_thread = 1000;

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let alloc = Arc::clone(&alloc);
                thread::spawn(move || {
                    let mut lsns = Vec::with_capacity(allocations_per_thread);
                    for _ in 0..allocations_per_thread {
                        lsns.push(alloc.allocate());
                    }
                    lsns
                })
            })
            .collect();

        // Collect all LSNs from all threads
        let mut all_lsns = HashSet::new();
        for handle in handles {
            let lsns = handle.join().unwrap();
            for lsn in lsns {
                // Each LSN should be unique
                assert!(all_lsns.insert(lsn.0), "Duplicate LSN detected: {:?}", lsn);
            }
        }

        // Should have exactly num_threads * allocations_per_thread unique LSNs
        assert_eq!(all_lsns.len(), num_threads * allocations_per_thread);

        // Current LSN should be one past the last allocated
        assert_eq!(
            alloc.current(),
            LSN((num_threads * allocations_per_thread + 1) as u64)
        );
    }

    #[test]
    fn test_concurrent_batch_allocation_no_overlap() {
        let alloc = Arc::new(LsnAllocator::new());
        let num_threads = 8;
        let batches_per_thread = 100;
        let batch_size = 10u64;

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let alloc = Arc::clone(&alloc);
                thread::spawn(move || {
                    let mut ranges = Vec::with_capacity(batches_per_thread);
                    for _ in 0..batches_per_thread {
                        ranges.push(alloc.allocate_batch(batch_size));
                    }
                    ranges
                })
            })
            .collect();

        // Collect all ranges
        let mut all_ranges: Vec<(LSN, LSN)> = Vec::new();
        for handle in handles {
            all_ranges.extend(handle.join().unwrap());
        }

        // Sort by first LSN
        all_ranges.sort_by_key(|(first, _)| first.0);

        // Verify no overlaps - each range should start after previous ends
        for i in 1..all_ranges.len() {
            let (_, prev_last) = all_ranges[i - 1];
            let (curr_first, _) = all_ranges[i];
            assert!(
                curr_first.0 > prev_last.0,
                "Overlapping ranges: prev_last={:?}, curr_first={:?}",
                prev_last,
                curr_first
            );
        }
    }

    #[test]
    fn test_monotonically_increasing() {
        let alloc = LsnAllocator::new();

        let mut prev = LSN(0);
        for _ in 0..1000 {
            let curr = alloc.allocate();
            assert!(curr.0 > prev.0, "LSN not monotonically increasing");
            prev = curr;
        }
    }

    #[test]
    fn test_default_impl() {
        let alloc = LsnAllocator::default();
        assert_eq!(alloc.current(), LSN(1));
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;

    #[test]
    #[should_panic(expected = "LSN Allocator Overflow")]
    fn test_allocator_overflow_boundary() {
        // Start just before the limit
        let alloc = LsnAllocator::starting_at(LSN(u64::MAX - 1));

        // This should succeed and return MAX-1
        let lsn = alloc.allocate();
        assert_eq!(lsn, LSN(u64::MAX - 1));

        // This should panic because next_lsn is now MAX (reserved/invalid)
        let _ = alloc.allocate();
    }

    #[test]
    #[should_panic(expected = "LSN Allocator Overflow")]
    fn test_batch_allocation_boundary() {
        // Start 10 steps before limit
        let alloc = LsnAllocator::starting_at(LSN(u64::MAX - 10));

        // Allocate exactly 10 items [MAX-10, ..., MAX-1]
        let (start, end) = alloc.allocate_batch(10);
        assert_eq!(start, LSN(u64::MAX - 10));
        assert_eq!(end, LSN(u64::MAX - 1));

        // Next allocation should fail as next_lsn is now MAX
        let _ = alloc.allocate();
    }

    #[test]
    fn test_batch_allocation_exact_fit_succeeds() {
        // Start 10 steps before limit
        let alloc = LsnAllocator::starting_at(LSN(u64::MAX - 10));

        // Allocate exactly 10 items [MAX-10, ..., MAX-1]
        // This MUST succeed (not panic) for the code to be correct.
        // A mutant changing `>` to `>=` in the check would panic here.
        let (start, end) = alloc.allocate_batch(10);
        assert_eq!(start, LSN(u64::MAX - 10));
        assert_eq!(end, LSN(u64::MAX - 1));
    }
}
