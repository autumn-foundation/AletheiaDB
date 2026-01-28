use gallifreydb::core::GLOBAL_INTERNER;
/// TDD test to detect and fix race condition between snapshot creations
///
/// This test verifies that current and historical snapshots are created
/// atomically without concurrent writes creating inconsistency.
use gallifreydb::core::graph::Node;
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::{BiTemporalInterval, time};
use gallifreydb::storage::checkpoint::{CheckpointConfig, CheckpointManager};
use gallifreydb::storage::current::CurrentStorage;
use gallifreydb::storage::historical::HistoricalStorage;
use gallifreydb::storage::snapshot::StorageSnapshot;
use gallifreydb::storage::wal::LSN;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, RwLock};
use std::thread;
use tempfile::tempdir;

#[test]
#[ignore] // Will fail until fix is implemented
fn test_concurrent_write_during_snapshot_creation() {
    // Setup: Create storage with initial data
    let current = Arc::new(CurrentStorage::new());
    // Use RwLock to allow concurrent writes (CurrentStorage handles it internally, HistoricalStorage needs lock)
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

    // Add initial nodes
    for i in 1..=100 {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let props = PropertyMapBuilder::new().insert("id", i as i64).build();
        let node_id = NodeId::new(i).unwrap();
        let version_id = VersionId::new(i).unwrap();
        let node = Node::new(node_id, label, props.clone(), version_id);
        let temporal = BiTemporalInterval::current(time::now());

        current.insert_node_direct(node, time::now()).unwrap();
        historical
            .write()
            .unwrap()
            .add_node_version(node_id, version_id, temporal, label, props)
            .unwrap();
    }

    // Barrier to synchronize threads
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();
    let current_clone = current.clone();
    let historical_clone = historical.clone();

    // Thread 1: Create checkpoint (will call create_snapshot twice)
    let checkpoint_thread = thread::spawn(
        move || -> (gallifreydb::storage::checkpoint::CheckpointStats, PathBuf) {
            barrier_clone.wait(); // Synchronize start

            let dir = tempdir().unwrap();
            let path = dir.keep(); // Persist directory to allow main thread access
            let config = CheckpointConfig::with_data_dir(&path);
            let mut manager = CheckpointManager::new(config).unwrap();

            // This should be ATOMIC - no writes should sneak in between snapshots
            let historical_guard = historical_clone.read().unwrap();
            let stats = manager
                .create_checkpoint(LSN(1), &current_clone, &*historical_guard)
                .unwrap();

            (stats, path)
        },
    );

    // Thread 2: Concurrent writer (tries to write during snapshot creation)
    let writer_thread = thread::spawn(move || {
        barrier.wait(); // Synchronize start

        // Try to write many times to increase chance of hitting the race window
        for i in 101..=200 {
            let label = GLOBAL_INTERNER.intern("Person").unwrap();
            let props = PropertyMapBuilder::new().insert("id", i as i64).build();
            let node_id = NodeId::new(i).unwrap();
            let version_id = VersionId::new(i).unwrap();
            let node = Node::new(node_id, label, props.clone(), version_id);
            let temporal = BiTemporalInterval::current(time::now());

            // This write should either:
            // 1. Happen BEFORE both snapshots (both see it)
            // 2. Happen AFTER both snapshots (neither sees it)
            // 3. Be BLOCKED during snapshot creation
            //
            // It should NEVER happen between the two snapshot creations!

            // Update both storages (simulating transaction)
            current.insert_node_direct(node, time::now()).unwrap();
            // In a real DB, these happen together. Here we do them sequentially which
            // increases the chance of race if checkpoint interleaves.
            historical
                .write()
                .unwrap()
                .add_node_version(node_id, version_id, temporal, label, props)
                .unwrap();
        }
    });

    let (stats, data_path) = checkpoint_thread.join().unwrap();
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

    // Validation logic
    // Recover from checkpoint to inspect its content
    let config = CheckpointConfig::with_data_dir(&data_path);

    // Create a dummy WAL for recovery (checkpoint recovery requires WAL system, even if empty)
    use gallifreydb::storage::wal::concurrent_system::{
        ConcurrentWalSystem, ConcurrentWalSystemConfig,
    };
    let wal_dir = data_path.join("wal_recovery"); // separate dir inside data_path
    std::fs::create_dir_all(&wal_dir).unwrap();
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    let mut manager = CheckpointManager::new(config).unwrap();
    let (recovered_current, recovered_historical, _) = manager.recover(&wal).unwrap();

    // Check 1: Orphaned Versions
    // For every historical version that is current (valid_to is MAX), check if it exists in current
    if let Some(version) = recovered_historical
        .__test_get_node_versions_iterator()
        .find(|v| {
            v.temporal.valid_time().is_current() && recovered_current.get_node(v.node_id).is_err()
        })
    {
        panic!(
            "Found orphaned version: {:?} for node {:?} (exists in historical but not current)",
            version.id, version.node_id
        );
    }

    // Check 2: Missing History
    // For every node in current, check if it has a current version in historical
    for node_id in recovered_current.get_all_node_ids() {
        if recovered_historical
            .get_current_node_version(node_id)
            .is_none()
        {
            panic!(
                "Found node without history: {:?} (exists in current but no current version in historical)",
                node_id
            );
        }
    }

    // Cleanup
    std::fs::remove_dir_all(data_path).unwrap();
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
