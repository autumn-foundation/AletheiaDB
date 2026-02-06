use aletheiadb::core::GLOBAL_INTERNER;
/// TDD test to detect and fix race condition between snapshot creations
///
/// This test verifies that current and historical snapshots are created
/// atomically without concurrent writes creating inconsistency.
use aletheiadb::core::graph::Node;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::checkpoint::{CheckpointConfig, CheckpointManager};
use aletheiadb::storage::current::CurrentStorage;
use aletheiadb::storage::historical::HistoricalStorage;
use aletheiadb::storage::snapshot::StorageSnapshot;
use aletheiadb::storage::wal::LSN;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

#[test]
#[ignore] // Will fail until fix is implemented
fn test_concurrent_write_during_snapshot_creation() {
    // Setup: Create storage with initial data
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(HistoricalStorage::new());

    // Add initial nodes
    for i in 1..=100 {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let props = PropertyMapBuilder::new().insert("id", i as i64).build();
        let node_id = NodeId::new(i).unwrap();
        let version_id = VersionId::new(i).unwrap();
        let node = Node::new(node_id, label, props, version_id);
        current.insert_node_direct(node, time::now()).unwrap();
    }

    // Barrier to synchronize threads
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();
    let current_clone = current.clone();
    let historical_clone = historical.clone();

    // Thread 1: Create checkpoint (will call create_snapshot twice)
    let checkpoint_thread = thread::spawn(move || {
        barrier_clone.wait(); // Synchronize start

        let dir = tempdir().unwrap();
        let config = CheckpointConfig::with_data_dir(dir.path());
        let mut manager = CheckpointManager::new(config).unwrap();

        // This should be ATOMIC - no writes should sneak in between snapshots
        manager
            .create_checkpoint(LSN(1), &current_clone, &historical_clone)
            .unwrap()
    });

    // Thread 2: Concurrent writer (tries to write during snapshot creation)
    let writer_thread = thread::spawn(move || {
        barrier.wait(); // Synchronize start

        // Try to write many times to increase chance of hitting the race window
        for i in 101..=200 {
            let label = GLOBAL_INTERNER.intern("Person").unwrap();
            let props = PropertyMapBuilder::new().insert("id", i as i64).build();
            let node_id = NodeId::new(i).unwrap();
            let version_id = VersionId::new(i).unwrap();
            let node = Node::new(node_id, label, props, version_id);

            // This write should either:
            // 1. Happen BEFORE both snapshots (both see it)
            // 2. Happen AFTER both snapshots (neither sees it)
            // 3. Be BLOCKED during snapshot creation
            //
            // It should NEVER happen between the two snapshot creations!
            let _ = current.insert_node_direct(node, time::now());
        }
    });

    let stats = checkpoint_thread.join().unwrap();
    writer_thread.join().unwrap();

    // Verify checkpoint is consistent:
    // - Either captured 100 nodes (before writes)
    // - Or captured 200 nodes (after writes)
    // - Or something in between BUT with matching historical versions
    //
    // What we DON'T want: checkpoint with orphaned versions (historical version
    // referencing node that doesn't exist in current snapshot)

    // For now, just verify basic stats are sane
    assert!(
        stats.node_count >= 100,
        "Should have at least initial 100 nodes"
    );

    // TODO: After fix, add validation that current and historical snapshots
    // are consistent (no orphaned versions)
}

/// Simpler test: Verify the ABSENCE of write coordination (documents current bug)
#[test]
fn test_snapshots_created_sequentially_without_coordination() {
    // This test PASSES but documents the bug: snapshots are created
    // sequentially without any write coordination.

    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();

    // Create snapshots - currently these are independent operations
    let current_snapshot = current.create_snapshot(LSN(1));

    // BUG: Writes can happen here! ←—— RACE CONDITION WINDOW

    let historical_snapshot = historical.create_snapshot(LSN(1));

    // Both snapshots exist but may be inconsistent
    assert_eq!(current_snapshot.lsn(), LSN(1));
    assert_eq!(historical_snapshot.lsn(), LSN(1));

    // The LSN values match, but the ACTUAL DATA may not be from the same point in time!
}
