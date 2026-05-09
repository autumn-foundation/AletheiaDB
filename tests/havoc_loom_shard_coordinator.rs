#![cfg(loom)]

//! Loom model for `ShardCoordinator` lock ordering.
//!
//! This models the potential deadlock between `active_transactions` and `connections`
//! in the ShardCoordinator. The coordinator must drop the connections lock before
//! reinserting a transaction or updating active transactions.

use loom::sync::{Arc, RwLock};
use loom::thread;

#[test]
fn test_havoc_coordinator_deadlock_prevented() {
    loom::model(|| {
        let active_transactions = Arc::new(RwLock::new(()));
        let connections = Arc::new(RwLock::new(()));

        let tx1 = active_transactions.clone();
        let conn1 = connections.clone();
        let t1 = thread::spawn(move || {
            // Simulate prepare: holds connections.read() and requests active_transactions.write()
            let c = conn1.read().unwrap();

            // To prevent deadlock, connections must be dropped BEFORE acquiring active_transactions.write()
            drop(c);
            let _t = tx1.write().unwrap();
        });

        let tx2 = active_transactions.clone();
        let conn2 = connections.clone();
        let t2 = thread::spawn(move || {
            // Simulate an operation that holds active_transactions and requests connections
            // (or another thread simulating health checks that requires connections.write())
            let _t = tx2.write().unwrap();
            let _c = conn2.write().unwrap();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
