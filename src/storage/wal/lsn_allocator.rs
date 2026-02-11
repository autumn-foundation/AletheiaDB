//! Global LSN (Log Sequence Number) allocator for concurrent WAL.
//!
//! The LSN allocator provides globally unique, monotonically increasing
//! sequence numbers for WAL entries. It is the **only** synchronization
//! point between concurrent writers - a single atomic increment.
//!
//! # Design
//!
//! - Uses `AtomicU64::fetch_update` (CAS loop) for lock-free allocation with overflow protection.
//! - Single allocation is O(1) with low latency.
//! - Batch allocation amortizes atomic overhead for multi-operation transactions.
//!
//! # Thread Safety
//!
//! The allocator is `Send` and `Sync`. Multiple threads can allocate LSNs
//! concurrently without any locking.
//!
//! # LSN Overflow (Theoretical Limitation)
//!
//! The allocator uses a `u64` counter. Overflow is explicitly checked and returns an error
//! rather than wrapping around.
//!
//! - At 1 million LSNs/second: overflow in ~584,542 years
//!
//! **Consequences of overflow**: LSN exhaustion is a terminal state. The database must be
//! restarted with LSN reset (after full checkpoint) to continue.
//!
//! **Mitigation**: Monitoring `current()` against a threshold (e.g., `u64::MAX / 2`)
//! can provide early warning.

use std::sync::atomic::{AtomicU64, Ordering};

use super::LSN;
use crate::utils::error::{Error, Result, StorageError};

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
    /// This uses `fetch_update` (CAS loop) to ensure we don't wrap on overflow.
    ///
    /// # Memory Ordering
    ///
    /// Uses `Ordering::Relaxed` because LSN uniqueness is guaranteed by the atomic
    /// update itself. Happens-before relationships are established by the WAL write path.
    ///
    /// # Returns
    ///
    /// The allocated LSN. Each call returns a unique, monotonically
    /// increasing value.
    #[inline]
    pub fn allocate(&self) -> Result<LSN> {
        // Use fetch_update to atomically check for overflow BEFORE incrementing.
        // This prevents the counter from ever wrapping to 0, which would be catastrophic.
        let res = self
            .next_lsn
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                if val == u64::MAX {
                    None
                } else {
                    Some(val + 1)
                }
            });

        match res {
            Ok(lsn) => Ok(LSN(lsn)),
            Err(_) => Err(Error::Storage(StorageError::WalError {
                reason: "LSN Allocator Overflow: limit reached".to_string(),
            })),
        }
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
    #[inline]
    pub fn allocate_batch(&self, count: u64) -> Result<(LSN, LSN)> {
        assert!(count > 0, "Cannot allocate 0 LSNs");

        // Use fetch_update to atomically check for overflow BEFORE adding count.
        let res = self
            .next_lsn
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| {
                val.checked_add(count)
            });

        match res {
            Ok(first) => Ok((LSN(first), LSN(first + count - 1))),
            Err(current) => Err(Error::Storage(StorageError::WalError {
                reason: format!(
                    "LSN Allocator Overflow: allocation of {} LSNs starting at {} would wrap u64",
                    count, current
                ),
            })),
        }
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

        let lsn1 = alloc.allocate().unwrap();
        let lsn2 = alloc.allocate().unwrap();
        let lsn3 = alloc.allocate().unwrap();

        assert_eq!(lsn1, LSN(1));
        assert_eq!(lsn2, LSN(2));
        assert_eq!(lsn3, LSN(3));
        assert_eq!(alloc.current(), LSN(4));
    }

    #[test]
    fn test_batch_allocation() {
        let alloc = LsnAllocator::new();

        let (first, last) = alloc.allocate_batch(5).unwrap();

        assert_eq!(first, LSN(1));
        assert_eq!(last, LSN(5));
        assert_eq!(alloc.current(), LSN(6));
    }

    #[test]
    fn test_batch_allocation_single() {
        let alloc = LsnAllocator::new();

        let (first, last) = alloc.allocate_batch(1).unwrap();

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
        assert_eq!(alloc.allocate().unwrap(), LSN(1000));
        assert_eq!(alloc.allocate().unwrap(), LSN(1001));
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
                        lsns.push(alloc.allocate().unwrap());
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
                        ranges.push(alloc.allocate_batch(batch_size).unwrap());
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
            let curr = alloc.allocate().unwrap();
            assert!(curr.0 > prev.0, "LSN not monotonically increasing");
            prev = curr;
        }
    }

    #[test]
    fn test_default_impl() {
        let alloc = LsnAllocator::default();
        assert_eq!(alloc.current(), LSN(1));
    }

    #[test]
    fn test_allocate_batch_overflow_returns_error() {
        // Initialize near u64::MAX
        // u64::MAX - 5 so that adding 10 wraps around
        let alloc = LsnAllocator::starting_at(LSN(u64::MAX - 5));

        let result = alloc.allocate_batch(10);
        assert!(result.is_err());
        match result {
            Err(Error::Storage(StorageError::WalError { reason })) => {
                assert!(reason.contains("Overflow"));
            }
            _ => panic!("Expected WalError::Overflow, got {:?}", result),
        }

        // Verify that the state was NOT modified
        assert_eq!(alloc.current().0, u64::MAX - 5);
    }

    #[test]
    fn test_allocate_overflow_returns_error() {
        let alloc = LsnAllocator::starting_at(LSN(u64::MAX));

        let result = alloc.allocate();
        assert!(result.is_err());
        match result {
            Err(Error::Storage(StorageError::WalError { reason })) => {
                assert!(reason.contains("Overflow"));
            }
            _ => panic!("Expected WalError::Overflow, got {:?}", result),
        }

        // Verify that the state was NOT modified
        assert_eq!(alloc.current().0, u64::MAX);
    }
}
