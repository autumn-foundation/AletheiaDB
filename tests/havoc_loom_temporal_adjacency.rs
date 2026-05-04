#![cfg(loom)]

//! Loom model for `TemporalAdjacencyIndex` lock ordering.
//!
//! `TemporalAdjacencyIndex::insert_edge()` (`src/index/temporal_adjacency.rs`)
//! acquires two DashMap entry locks simultaneously — one from the `outgoing` map
//! and one from the `incoming` map — and documents:
//!
//! > Acquires locks in consistent order (by node ID) to prevent deadlock when
//! > two threads insert edges in opposite directions.
//!
//! Invariant: always acquire the lock whose *node ID is smaller* first,
//! regardless of whether it lives in the outgoing or the incoming map.
//!
//! ## Why per-node locks instead of per-(map,node) locks
//!
//! Using a separate `Mutex` for each `(map, node)` pair (e.g. `outgoing_a`,
//! `incoming_a`) produces disjoint lock sets for `A→B` and `B→A`, so loom
//! cannot construct a wait cycle and the ABBA scenario is invisible.
//!
//! In production the DashMap shards *can* collide across maps when the hash
//! of a node ID lands on the same shard index in both `outgoing` and
//! `incoming`. We model this worst case with a single `Mutex` per node that
//! represents the combined lock needed for that node in any map.  This lets
//! loom see the full ABBA cycle when lock ordering is violated.
//!
//! Correct ordering (A < B, so node A first):
//!
//!   Thread 1 (A→B):  node_a → node_b
//!   Thread 2 (B→A):  node_a → node_b   ← same order, no cycle
//!
//! Wrong ordering (source-first, no invariant):
//!
//!   Thread 1 (A→B):  node_a → node_b
//!   Thread 2 (B→A):  node_b → node_a   ← ABBA cycle, loom detects deadlock

use loom::sync::{Arc, Mutex};
use loom::thread;

/// Per-node lock model.
///
/// A single `Mutex` per node represents the coarsest possible lock that both
/// outgoing and incoming DashMap shards could share for that node ID.
struct TemporalAdjacencyModel {
    node_a: Mutex<Vec<u64>>, // all edges involving node A (id=1, lower)
    node_b: Mutex<Vec<u64>>, // all edges involving node B (id=2, higher)
}

impl TemporalAdjacencyModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            node_a: Mutex::new(Vec::new()),
            node_b: Mutex::new(Vec::new()),
        })
    }

    /// Insert A→B. A < B → acquire node_a first, node_b second.
    fn insert_a_to_b(&self, edge_id: u64) {
        let mut a = self.node_a.lock().unwrap(); // lower-id node first
        let mut b = self.node_b.lock().unwrap(); // higher-id node second
        a.push(edge_id);
        b.push(edge_id);
    }

    /// Insert B→A. B > A → still acquire node_a first (lower id), node_b second.
    ///
    /// This is the correct ordering: both directions acquire locks in the same
    /// node-ID order, preventing the ABBA cycle.
    fn insert_b_to_a(&self, edge_id: u64) {
        let mut a = self.node_a.lock().unwrap(); // lower-id node first
        let mut b = self.node_b.lock().unwrap(); // higher-id node second
        b.push(edge_id); // B is source
        a.push(edge_id); // A is target
    }
}

/// Two threads inserting edges in *opposite* directions must not deadlock.
///
/// With per-node locks and correct node-ID ordering, both threads acquire
/// `node_a` before `node_b`, so no wait cycle can form.  Loom exhaustively
/// verifies this across all interleavings.
#[test]
fn test_opposite_direction_edges_no_deadlock() {
    loom::model(|| {
        let index = TemporalAdjacencyModel::new();
        let i1 = index.clone();
        let i2 = index.clone();

        let t1 = thread::spawn(move || {
            i1.insert_a_to_b(100); // A→B: node_a → node_b
        });

        let t2 = thread::spawn(move || {
            i2.insert_b_to_a(200); // B→A: node_a → node_b (same order)
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

/// Two threads inserting edges in the *same* direction must not deadlock.
#[test]
fn test_same_direction_edges_no_deadlock() {
    loom::model(|| {
        let index = TemporalAdjacencyModel::new();
        let i1 = index.clone();
        let i2 = index.clone();

        let t1 = thread::spawn(move || {
            i1.insert_a_to_b(1);
        });

        let t2 = thread::spawn(move || {
            i2.insert_a_to_b(2);
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

/// Three concurrent edge insertions covering both directions simultaneously.
#[test]
fn test_three_way_edge_insertion_no_deadlock() {
    loom::model(|| {
        let index = TemporalAdjacencyModel::new();
        let i1 = index.clone();
        let i2 = index.clone();
        let i3 = index.clone();

        let t1 = thread::spawn(move || {
            i1.insert_a_to_b(1);
        });

        let t2 = thread::spawn(move || {
            i2.insert_b_to_a(2);
        });

        let t3 = thread::spawn(move || {
            i3.insert_a_to_b(3);
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();
    });
}
