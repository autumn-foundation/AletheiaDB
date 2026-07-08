//! Regression test for the `NodeScanIterator` out-of-memory (OOM) vector.
//!
//! Historically `NodeScanIterator` eagerly materialized *every* node id into a
//! `Vec<NodeId>` on the first `next()` call (via `get_all_node_ids()` /
//! `get_node_ids_by_label()`). On a graph with millions/billions of nodes that
//! single allocation is O(N) and blows up memory before a single row is
//! returned. The fix streams ids lazily over the `[0, max_id)` range, so the
//! per-`next()` allocation is O(1) and independent of N.
//!
//! We cannot allocate enough to actually OOM in CI, so we assert the *lazy
//! behavior* directly with a counting global allocator: the first `next()` must
//! allocate an amount that does NOT scale with the number of nodes. Under the
//! old eager implementation the first `next()` allocates at least
//! `N * size_of::<u64>()` bytes for the id `Vec`; under the streaming fix it is
//! a small constant (just the one node it returns).
//!
//! The allocator is installed as `#[global_allocator]` for this integration
//! test binary only, so it does not affect the main crate or other test
//! binaries. Counting is gated behind a thread-local flag armed only around the
//! measured `next()` call, so unrelated allocations (setup, the test harness)
//! are excluded.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use aletheiadb::PropertyMapBuilder;
use aletheiadb::query::executor::{NodeScanIterator, ResultIterator};
use aletheiadb::storage::current::CurrentStorage;

thread_local! {
    /// When `true`, allocations on this thread are accumulated into `BYTES`.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    /// Bytes allocated on this thread while `MEASURING` is armed.
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: `CountingAllocator` forwards every allocation/deallocation to the
// system allocator unchanged; it only observes sizes. The thread-local
// bookkeeping uses `Cell<Copy>` values which never allocate, so no reentrancy
// into the allocator can occur while counting.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && MEASURING.try_with(|m| m.get()).unwrap_or(false) {
            BYTES.with(|b| b.set(b.get() + layout.size()));
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// The first `next()` of a full scan must allocate O(1), not O(N).
#[test]
fn first_next_allocation_is_bounded_not_proportional_to_node_count() {
    const N: u64 = 5_000;

    let current = Arc::new(CurrentStorage::new());
    for i in 0..N {
        current
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("n{i}"))
                    .build(),
            )
            .expect("create_node");
    }

    // Warm up the thread-locals so their first access does not allocate inside
    // the measured window.
    MEASURING.with(|m| m.set(false));
    BYTES.with(|b| b.set(0));

    let mut iter = NodeScanIterator::new(None, Arc::clone(&current));

    // Measure allocations during the FIRST next() only. The old eager impl
    // built the entire Vec<NodeId> here (>= N * 8 bytes); the streaming fix
    // allocates only the single returned node.
    MEASURING.with(|m| m.set(true));
    let first = iter.next();
    MEASURING.with(|m| m.set(false));
    let bytes = BYTES.with(|b| b.get());

    assert!(
        matches!(first, Some(Ok(_))),
        "first next() should yield a node, got {:?}",
        first.map(|r| r.is_ok())
    );

    // The eager implementation allocated at least this many bytes for the id
    // Vec alone. A quarter of it is a very generous ceiling for the O(1) path
    // (which only clones one small node) yet far below the eager cost.
    let eager_vec_bytes = (N as usize) * std::mem::size_of::<u64>();
    let ceiling = eager_vec_bytes / 4;
    assert!(
        bytes < ceiling,
        "first next() allocated {bytes} bytes; expected O(1) (< {ceiling}). \
         Eager full-scan materialization has regressed (OOM vector, supersedes #3171). \
         Eager Vec<NodeId> alone would be ~{eager_vec_bytes} bytes."
    );
}
