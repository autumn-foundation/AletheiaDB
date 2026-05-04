#![cfg(loom)]

//! Loom model for `TemporalVectorIndex` lock ordering.
//!
//! `TemporalVectorIndex` (`src/index/vector/temporal/mod.rs`) holds two
//! `parking_lot::RwLock`s and documents:
//!
//! > consistent lock ordering (metadata → snapshot_data) to prevent deadlocks
//!
//! Concretely:
//!
//! * `create_snapshot_internal` – acquires `current_state.write()` then
//!   `snapshot_data.write()` in a single critical section (step 3).
//! * `prune_snapshots`          – acquires `snapshot_data.write()` alone.
//! * `memory_stats`             – acquires `current_state.read()` then
//!   `snapshot_data.read()` in the same documented order.
//! * `add_vector` / `remove`    – acquires `current_state.write()` alone.
//!
//! No path acquires `snapshot_data` (write) while holding nothing and then
//! tries to acquire `current_state`, so there is no inversion.  This model
//! verifies that under all loom-explored interleavings no deadlock arises.
//!
//! Note: production code uses `parking_lot::RwLock` (not `std`); we use
//! `loom::sync::RwLock` here because loom only instruments its own types.

use loom::sync::{Arc, RwLock};
use loom::thread;

struct TemporalVectorModel {
    current_state: RwLock<u64>,
    snapshot_data: RwLock<Vec<u64>>,
}

impl TemporalVectorModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            current_state: RwLock::new(0),
            snapshot_data: RwLock::new(Vec::new()),
        })
    }

    /// Mirrors `create_snapshot_internal` step 3:
    /// current_state.write() → snapshot_data.write()
    fn create_snapshot(&self) {
        let mut state = self.current_state.write().unwrap();
        let mut data = self.snapshot_data.write().unwrap();
        let val = *state;
        data.push(val);
        *state += 1;
    }

    /// Mirrors `prune_snapshots`: snapshot_data.write() alone (no current_state).
    fn prune_snapshots(&self) {
        let mut data = self.snapshot_data.write().unwrap();
        if data.len() > 5 {
            data.remove(0);
        }
    }

    /// Mirrors `memory_stats`: current_state.read() → snapshot_data.read()
    fn memory_stats(&self) -> (u64, usize) {
        let state = self.current_state.read().unwrap();
        let data = self.snapshot_data.read().unwrap();
        (*state, data.len())
    }

    /// Mirrors `add_vector` / record_transaction: current_state.write() alone.
    fn add_vector(&self, value: u64) {
        let mut state = self.current_state.write().unwrap();
        *state = value;
    }
}

/// snapshot creation vs pruning vs stats reader — all three concurrent.
#[test]
fn test_snapshot_prune_stats_no_deadlock() {
    loom::model(|| {
        let index = TemporalVectorModel::new();
        let i1 = index.clone();
        let i2 = index.clone();
        let i3 = index.clone();

        let t1 = thread::spawn(move || {
            i1.create_snapshot(); // current_state.write → snapshot_data.write
        });

        let t2 = thread::spawn(move || {
            i2.prune_snapshots(); // snapshot_data.write alone
        });

        let t3 = thread::spawn(move || {
            i3.memory_stats(); // current_state.read → snapshot_data.read
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();
    });
}

/// add_vector (current_state.write alone) vs create_snapshot
/// (current_state.write → snapshot_data.write).
#[test]
fn test_add_vs_snapshot_no_deadlock() {
    loom::model(|| {
        let index = TemporalVectorModel::new();
        let i1 = index.clone();
        let i2 = index.clone();

        let t1 = thread::spawn(move || {
            i1.add_vector(42);
        });

        let t2 = thread::spawn(move || {
            i2.create_snapshot();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

/// Two concurrent snapshot creations — same lock order, so no cycle.
#[test]
fn test_concurrent_snapshots_no_deadlock() {
    loom::model(|| {
        let index = TemporalVectorModel::new();
        let i1 = index.clone();
        let i2 = index.clone();

        let t1 = thread::spawn(move || {
            i1.create_snapshot();
        });

        let t2 = thread::spawn(move || {
            i2.create_snapshot();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
