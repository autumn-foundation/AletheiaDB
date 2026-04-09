import re

with open('tests/snapshot_race_condition.rs', 'r') as f:
    content = f.read()

# Replace thread 1 logic
old_thread1 = """    // Thread 1: Create checkpoint (will call create_snapshot twice)
    let checkpoint_thread = thread::spawn(move || {
        barrier_clone.wait(); // Synchronize start

        let dir = tempdir().unwrap();
        let config = CheckpointConfig::with_data_dir(dir.path());
        let mut manager = CheckpointManager::new(config).unwrap();"""

new_thread1 = """    // Create a temporary directory for checkpoint
    let dir = tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();

    // Thread 1: Create checkpoint (will call create_snapshot twice)
    let dir_path_clone = dir_path.clone();
    let checkpoint_thread = thread::spawn(move || {
        barrier_clone.wait(); // Synchronize start

        let config = CheckpointConfig::with_data_dir(&dir_path_clone);
        let mut manager = CheckpointManager::new(config).unwrap();"""

content = content.replace(old_thread1, new_thread1)

# Replace the TODO comment
old_todo = """    // TODO: After fix, add validation that current and historical snapshots
    // are consistent (no orphaned versions)"""

new_todo = """    // Validate that current and historical snapshots are consistent
    // by recovering the checkpoint and ensuring no orphaned versions exist.
    let config = CheckpointConfig::with_data_dir(&dir_path);
    let mut manager = CheckpointManager::new(config).unwrap();

    // Create a dummy WAL system since recover requires it
    let wal_dir = tempdir().unwrap();
    let wal_config = aletheiadb::storage::wal::ConcurrentWalSystemConfig::new(wal_dir.path());
    let wal = aletheiadb::storage::wal::ConcurrentWalSystem::new(wal_config).unwrap();

    let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal).unwrap();

    // Check that every historical node version references a valid node in current storage
    let historical_node_versions = recovered_historical.get_all_node_versions();
    for (&node_id, _versions) in &historical_node_versions {
        let node = recovered_current.get_node(node_id);
        assert!(node.is_ok(), "Orphaned version detected! Node {} exists in historical but not in current.", node_id.as_u64());
    }"""

content = content.replace(old_todo, new_todo)

with open('tests/snapshot_race_condition.rs', 'w') as f:
    f.write(content)
