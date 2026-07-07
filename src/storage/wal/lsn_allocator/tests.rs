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
