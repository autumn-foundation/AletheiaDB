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
use std::sync::{Arc, Barrier, RwLock};
use std::thread;
use tempfile::tempdir;

#[test]
#[ignore] // Will fail until fix is implemented (Issue #XXX - Race Condition)
fn test_concurrent_write_during_snapshot_creation()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Setup: Create storage with initial data
    let current = Arc::new(CurrentStorage::new());
    // Use RwLock to allow concurrent writes (CurrentStorage handles it internally, HistoricalStorage needs lock)
    // Note: This RwLock simulates the lack of atomic checkpointing in the production code by ensuring
    // that writes to history are blocked during checkpointing, but writes to current are not.
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

    // Create tempdir in main thread for proper RAII cleanup
    let temp_dir = tempdir()?;
    let data_path = temp_dir.path().to_path_buf();

    // Add initial nodes
    for i in 1..=100 {
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let props = PropertyMapBuilder::new().insert("id", i as i64).build();
        let node_id = NodeId::new(i).unwrap();
        let version_id = VersionId::new(i).unwrap();
        let node = Node::new(node_id, label, props.clone(), version_id);
        let temporal = BiTemporalInterval::current(time::now());

        current
            .insert_node_direct(node, time::now())
            .expect("Failed to insert initial node");
        historical
            .write()
            .unwrap()
            .add_node_version(node_id, version_id, temporal, label, props)
            .expect("Failed to add initial node version");
    }

    // Barrier to synchronize threads
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();
    let current_clone = current.clone();
    let historical_clone = historical.clone();
    let checkpoint_path = data_path.clone();

    // Thread 1: Create checkpoint (will call create_snapshot twice)
    let checkpoint_thread = thread::spawn(
        move || -> Result<gallifreydb::storage::checkpoint::CheckpointStats, Box<dyn std::error::Error + Send + Sync + 'static>> {
            barrier_clone.wait(); // Synchronize start

            let config = CheckpointConfig::with_data_dir(&checkpoint_path);
            let mut manager = CheckpointManager::new(config)?;

            // Acquire read lock on historical storage.
            // Note: In this test scenario, this lock actually helps REPRODUCE the "missing history" bug.
            // 1. Checkpoint thread acquires Read Lock.
            // 2. Writer thread inserts to `current` (allowed, concurrent).
            // 3. Writer thread tries to insert to `historical` -> BLOCKED by Read Lock.
            // 4. Checkpoint takes snapshot of `current` -> SEES the new node.
            // 5. Checkpoint takes snapshot of `historical` -> DOES NOT SEE the new version (writer blocked).
            // 6. Checkpoint finishes.
            // 7. Writer unblocks and writes to `historical`.
            // Result: Checkpoint has node in `current` but no version in `historical`. Inconsistent!
            let historical_guard = historical_clone
                .read()
                .map_err(|_| "Failed to acquire read lock on historical storage")?;
            let stats = manager
                .create_checkpoint(LSN(1), &current_clone, &historical_guard)?;

            Ok(stats)
        },
    );

    // Thread 2: Concurrent writer (tries to write during snapshot creation)
    let writer_thread = thread::spawn(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            barrier.wait(); // Synchronize start

            // Small delay to ensure checkpoint thread has time to acquire the lock first
            // and create the "torn read" condition described above.
            // Intentional delay to increase probability of checkpoint thread
            // acquiring lock before writer, creating the "torn read" condition
            std::thread::sleep(std::time::Duration::from_millis(10));

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
                current
                    .insert_node_direct(node, time::now())
                    .map_err(|e| format!("Failed to insert node in writer thread: {}", e))?;

                // In a real DB, these happen together. Here we do them sequentially which
                // increases the chance of race if checkpoint interleaves.
                historical
                    .write()
                    .unwrap()
                    .add_node_version(node_id, version_id, temporal, label, props)
                    .map_err(|e| format!("Failed to add node version in writer thread: {}", e))?;
            }
            Ok(())
        },
    );

    let stats = checkpoint_thread.join().unwrap()?;
    writer_thread.join().unwrap()?;

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
    std::fs::create_dir_all(&wal_dir)?;
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let mut manager = CheckpointManager::new(config)?;
    let (recovered_current, recovered_historical, _) = manager.recover(&wal)?;

    // Check 1: Orphaned Versions
    // For every historical version that is current (valid_to is MAX), check if it exists in current
    if let Some(version) = recovered_historical.iter_node_versions().find(|v| {
        v.temporal.valid_time().is_current() && recovered_current.get_node(v.node_id).is_err()
    }) {
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

    // Cleanup happens automatically via tempdir
    Ok(())
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
