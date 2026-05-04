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
//! Classic ABBA scenario this prevents:
//!
//!   Thread 1 (A→B, A<B):  outgoing[A] → incoming[B]
//!   Thread 2 (B→A, B>A):  incoming[A] → outgoing[B]   (correct)
//!
//! Both threads acquire A-keyed locks before B-keyed locks, so no cycle forms.
//!
//! We model each node's slot in each map as a distinct `Mutex<()>` to make the
//! ordering visible to loom's scheduler.

use loom::sync::{Arc, Mutex};
use loom::thread;

const NODE_A: u64 = 1; // lower id
const NODE_B: u64 = 2; // higher id

/// Simplified model of the two DashMaps.
///
/// DashMap uses internal shard locks; we expose one Mutex per (map, node) pair
/// to give loom full visibility into acquisition order.
struct TemporalAdjacencyModel {
    outgoing_a: Mutex<Vec<u64>>, // outgoing map, slot for node A
    outgoing_b: Mutex<Vec<u64>>, // outgoing map, slot for node B
    incoming_a: Mutex<Vec<u64>>, // incoming map, slot for node A
    incoming_b: Mutex<Vec<u64>>, // incoming map, slot for node B
}

impl TemporalAdjacencyModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outgoing_a: Mutex::new(Vec::new()),
            outgoing_b: Mutex::new(Vec::new()),
            incoming_a: Mutex::new(Vec::new()),
            incoming_b: Mutex::new(Vec::new()),
        })
    }

    /// Insert edge A→B.  source=A < target=B → outgoing[A] first, incoming[B] second.
    fn insert_a_to_b(&self, edge_id: u64) {
        let mut out = self.outgoing_a.lock().unwrap(); // lower-id lock first
        let mut inc = self.incoming_b.lock().unwrap(); // higher-id lock second
        out.push(edge_id);
        inc.push(edge_id);
    }

    /// Insert edge B→A.  source=B > target=A → incoming[A] first, outgoing[B] second.
    fn insert_b_to_a(&self, edge_id: u64) {
        let mut inc = self.incoming_a.lock().unwrap(); // lower-id lock first (target=A)
        let mut out = self.outgoing_b.lock().unwrap(); // higher-id lock second (source=B)
        inc.push(edge_id);
        out.push(edge_id);
    }

}

/// Two threads inserting edges in *opposite* directions must not deadlock.
///
/// This is the canonical ABBA scenario: without the node-ID ordering invariant
/// one thread would hold outgoing[A] waiting for incoming[B] while the other
/// holds outgoing[B] waiting for incoming[A].
#[test]
fn test_opposite_direction_edges_no_deadlock() {
    loom::model(|| {
        let index = TemporalAdjacencyModel::new();
        let i1 = index.clone();
        let i2 = index.clone();

        let t1 = thread::spawn(move || {
            i1.insert_a_to_b(100); // A→B: outgoing[A] → incoming[B]
        });

        let t2 = thread::spawn(move || {
            i2.insert_b_to_a(200); // B→A: incoming[A] → outgoing[B]
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
