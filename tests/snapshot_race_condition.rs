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

use aletheiadb::storage::wal::LSN;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

#[test]
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

    // Create a temporary directory for checkpoint
    let dir = tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();

    // Thread 1: Create checkpoint (will call create_snapshot twice)
    let dir_path_clone = dir_path.clone();
    let checkpoint_thread = thread::spawn(move || {
        barrier_clone.wait(); // Synchronize start

        let config = CheckpointConfig::with_data_dir(&dir_path_clone);
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

    // Validate that current and historical snapshots are consistent
    // by recovering the checkpoint and ensuring no orphaned versions exist.
    let config = CheckpointConfig::with_data_dir(&dir_path);
    let mut manager = CheckpointManager::new(config).unwrap();

    // Create a dummy WAL system since recover requires it
    let wal_dir = tempdir().unwrap();
    let wal_config =
        aletheiadb::storage::wal::concurrent_system::ConcurrentWalSystemConfig::new(wal_dir.path());
    let wal =
        aletheiadb::storage::wal::concurrent_system::ConcurrentWalSystem::new(wal_config).unwrap();

    let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal).unwrap();

    // Check that every historical node version references a valid node in current storage
    let historical_node_versions = recovered_historical.get_all_node_versions();
    for &node_id in historical_node_versions.keys() {
        let node = recovered_current.get_node(node_id);
        assert!(
            node.is_ok(),
            "Orphaned version detected! Node {} exists in historical but not in current.",
            node_id.as_u64()
        );
    }
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
