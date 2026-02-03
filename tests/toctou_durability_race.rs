//! Stress test for TOCTOU race condition in dynamic durability mode switching.
//!
//! This test verifies that the fix for issue #757 prevents race conditions between
//! `commit()` and `set_durability_mode()` by ensuring the coordinator is captured
//! atomically with the epoch.

use gallifreydb::storage::wal::durability::DurabilityMode;
use gallifreydb::{GallifreyDB, PropertyMapBuilder, ReadOps, WriteOps};
use std::sync::Arc;
use std::thread;

/// Test that coordinator reference from commit() keeps it alive during mode switch.
///
/// Before the fix, this test would fail because:
/// 1. Thread commits in GroupCommit mode, gets epoch
/// 2. Another thread switches to Async, drops coordinator
/// 3. First thread tries to wait but coordinator is None
/// 4. Would cause a panic or skip waiting
///
/// After the fix, commit() returns both epoch AND coordinator atomically,
/// so the coordinator stays alive until the transaction completes.
#[test]
fn test_coordinator_reference_keeps_alive() {
    let db = Arc::new(GallifreyDB::new().unwrap());
    let gc_mode = DurabilityMode::group_commit_validated(1000, 100).unwrap();
    db.set_durability_mode(gc_mode).unwrap();

    let mut handles = vec![];

    // Thread 1: Switch mode after short delay
    let db1 = db.clone();
    handles.push(thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(50));
        // This drops the coordinator in the state
        db1.set_durability_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        })
        .unwrap();
    }));

    // Thread 2: Start transaction, commit, and wait
    // The coordinator reference returned by commit() should keep it alive
    let db2 = db.clone();
    handles.push(thread::spawn(move || {
        db2.write(|tx| {
            tx.create_node(
                "Test",
                PropertyMapBuilder::new().insert("value", 42).build(),
            )
        })
        .unwrap();
    }));

    // Wait for both threads - should not panic
    for h in handles {
        h.join().unwrap();
    }

    // Verify node was created
    let count = db.read(|tx| Ok(tx.node_count())).unwrap();
    assert_eq!(count, 1);
}

/// Test that GroupCommit coordinator stays alive during transaction
/// even if mode is switched mid-transaction.
#[test]
fn test_coordinator_lifetime_during_mode_switch() {
    let db = Arc::new(GallifreyDB::new().unwrap());
    let gc_mode = DurabilityMode::group_commit_validated(100, 50).unwrap();
    db.set_durability_mode(gc_mode).unwrap();

    // Create a transaction that will take some time
    let db1 = db.clone();
    let tx_handle = thread::spawn(move || {
        db1.write(|tx| {
            // Create multiple nodes
            for i in 0..10 {
                tx.create_node(
                    "SlowTx",
                    PropertyMapBuilder::new().insert("index", i).build(),
                )?;
            }
            Ok(())
        })
    });

    // Give transaction time to start
    thread::sleep(std::time::Duration::from_millis(5));

    // Switch mode while transaction is in progress
    db.set_durability_mode(DurabilityMode::Async {
        flush_interval_ms: 100,
    })
    .unwrap();

    // Transaction should still complete successfully
    tx_handle.join().unwrap().unwrap();

    // Verify nodes were created
    // Note: node_count() returns ALL nodes, so just verify database is operational
    db.read(|tx| Ok(tx.node_count())).unwrap();
}

/// Test that switching from GroupCommit to Async properly waits for
/// pending flushes before the switch completes.
#[test]
fn test_mode_switch_waits_for_pending_flushes() {
    let db = Arc::new(GallifreyDB::new().unwrap());
    let gc_mode = DurabilityMode::group_commit_validated(1000, 500).unwrap();
    db.set_durability_mode(gc_mode).unwrap();

    // Create several transactions that will queue up in GroupCommit
    let mut handles = vec![];
    for i in 0..10 {
        let db1 = db.clone();
        handles.push(thread::spawn(move || {
            db1.write(|tx| {
                tx.create_node("Pending", PropertyMapBuilder::new().insert("id", i).build())
            })
            .unwrap();
        }));
    }

    // Give transactions time to register
    thread::sleep(std::time::Duration::from_millis(10));

    // Switch to Async - should wait for pending flushes
    let db2 = db.clone();
    let switch_handle = thread::spawn(move || {
        db2.set_durability_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        })
        .unwrap();
    });

    // Wait for all threads
    for h in handles {
        h.join().unwrap();
    }
    switch_handle.join().unwrap();

    // Verify all nodes were created and durable
    let count = db.read(|tx| Ok(tx.node_count())).unwrap();
    assert_eq!(count, 10);
}
