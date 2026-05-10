#![cfg(loom)]

//! Loom model for `ShardCoordinator` lock ordering.
//!
//! This models the potential deadlock between `active_transactions` and `connections`
//! in the ShardCoordinator. The coordinator must drop the connections lock before
//! reinserting a transaction or updating active transactions.

use loom::sync::{Arc, RwLock};
use loom::thread;

struct ShardCoordinatorModel {
    active_transactions: RwLock<()>,
    connections: RwLock<()>,
}

impl ShardCoordinatorModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active_transactions: RwLock::new(()),
            connections: RwLock::new(()),
        })
    }

    /// Mirrors `prepare_distributed_transaction()` and `commit_distributed_transaction()`
    fn prepare_distributed_transaction(&self) {
        // Simulate start of prepare: removing from active_transactions
        let _txns = self.active_transactions.write().unwrap();
        drop(_txns);

        let c = self.connections.read().unwrap();

        // Check if all prepared, failing, and inserting back into active_transactions
        drop(c); // Prevent deadlock with active_transactions.write()
        let _txns = self.active_transactions.write().unwrap();
    }

    /// A hypothetical future operation that acquires locks in reverse order
    /// `active_transactions` -> `connections` (which would trigger the deadlock)
    fn reverse_order_operation(&self) {
        let _txns = self.active_transactions.write().unwrap();
        let _c = self.connections.write().unwrap();
    }
}

#[test]
fn test_havoc_coordinator_deadlock_prevented() {
    loom::model(|| {
        let coord = ShardCoordinatorModel::new();
        let c1 = coord.clone();
        let c2 = coord.clone();

        let t1 = thread::spawn(move || {
            c1.prepare_distributed_transaction();
        });

        let t2 = thread::spawn(move || {
            c2.reverse_order_operation();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
