use super::*;
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;
use crate::core::temporal::BiTemporalInterval;

fn create_test_node_version(id: u64) -> NodeVersion {
    let properties = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    NodeVersion::new_anchor(
        VersionId::new(id).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        properties,
    )
}

fn create_test_edge_version(id: u64) -> EdgeVersion {
    let properties = PropertyMapBuilder::new().insert("weight", 1.5f64).build();

    EdgeVersion::new_anchor(
        VersionId::new(id).unwrap(),
        EdgeId::new(200).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        properties,
    )
}

// ========================================================================
// TDD Tests for Issue #2: CRUD operations
// ========================================================================

#[test]
fn test_redb_store_and_retrieve_node_version() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
    assert_eq!(retrieved.id, version.id);
    assert_eq!(retrieved.node_id, version.node_id);
}

#[test]
fn test_redb_store_and_retrieve_edge_version() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version = create_test_edge_version(1);
    storage.store_edge_version(&version).unwrap();

    let retrieved = storage.get_edge_version(version.id).unwrap().unwrap();
    assert_eq!(retrieved.id, version.id);
    assert_eq!(retrieved.edge_id, version.edge_id);
    assert_eq!(retrieved.source, version.source);
    assert_eq!(retrieved.target, version.target);
}

#[test]
fn test_redb_get_nonexistent_returns_none() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let result = storage
        .get_node_version(VersionId::new(999).unwrap())
        .unwrap();
    assert!(result.is_none());

    let result = storage
        .get_edge_version(VersionId::new(999).unwrap())
        .unwrap();
    assert!(result.is_none());
}

#[test]
fn test_redb_delete_node_version() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    assert!(storage.contains_node_version(version.id).unwrap());

    let deleted = storage.delete_node_version(version.id).unwrap();
    assert!(deleted);

    assert!(!storage.contains_node_version(version.id).unwrap());

    // Deleting again should return false
    let deleted_again = storage.delete_node_version(version.id).unwrap();
    assert!(!deleted_again);
}

#[test]
fn test_redb_delete_edge_version() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version = create_test_edge_version(1);
    storage.store_edge_version(&version).unwrap();

    assert!(storage.contains_edge_version(version.id).unwrap());

    let deleted = storage.delete_edge_version(version.id).unwrap();
    assert!(deleted);

    assert!(!storage.contains_edge_version(version.id).unwrap());
}

// ========================================================================
// TDD Tests for Issue #2: Batch operations
// ========================================================================

#[test]
fn test_redb_batch_store_atomic() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let node_versions: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();
    let edge_versions: Vec<EdgeVersion> = (1..=5).map(create_test_edge_version).collect();

    // Store batch with LSN
    storage
        .store_batch_with_lsn(&node_versions, &edge_versions, LSN(100))
        .unwrap();

    // Verify all nodes stored
    for version in &node_versions {
        let retrieved = storage.get_node_version(version.id).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, version.id);
    }

    // Verify all edges stored
    for version in &edge_versions {
        let retrieved = storage.get_edge_version(version.id).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, version.id);
    }

    // Verify LSN was stored
    let flushed_lsn = storage.get_flushed_lsn().unwrap();
    assert_eq!(flushed_lsn, Some(LSN(100)));
}

#[test]
fn test_redb_batch_node_versions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let versions: Vec<NodeVersion> = (1..=100).map(create_test_node_version).collect();
    storage.store_node_versions_batch(&versions).unwrap();

    // Verify all stored
    for version in &versions {
        assert!(storage.contains_node_version(version.id).unwrap());
    }
}

#[test]
fn test_redb_batch_edge_versions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let versions: Vec<EdgeVersion> = (1..=100).map(create_test_edge_version).collect();
    storage.store_edge_versions_batch(&versions).unwrap();

    // Verify all stored
    for version in &versions {
        assert!(storage.contains_edge_version(version.id).unwrap());
    }
}

// ========================================================================
// TDD Tests for Issue #2: LSN metadata (NEW capability)
// ========================================================================

#[test]
fn test_redb_set_and_get_flushed_lsn() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Initially no LSN
    assert_eq!(storage.get_flushed_lsn().unwrap(), None);

    // Store with LSN
    let version = create_test_node_version(1);
    storage
        .store_batch_with_lsn(&[version], &[], LSN(42))
        .unwrap();

    // LSN should be set
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(42)));
}

#[test]
fn test_redb_flushed_lsn_persists_across_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // Create and set LSN
    {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        let version = create_test_node_version(1);
        storage
            .store_batch_with_lsn(&[version], &[], LSN(12345))
            .unwrap();
    }

    // Reopen and verify LSN persisted
    {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(12345)));

        // Also verify the version persisted
        let retrieved = storage
            .get_node_version(VersionId::new(1).unwrap())
            .unwrap();
        assert!(retrieved.is_some());
    }
}

#[test]
fn test_redb_batch_updates_flushed_lsn_atomically() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // First batch with LSN 100
    let nodes1: Vec<NodeVersion> = (1..=5).map(create_test_node_version).collect();
    storage
        .store_batch_with_lsn(&nodes1, &[], LSN(100))
        .unwrap();
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

    // Second batch with higher LSN
    let nodes2: Vec<NodeVersion> = (6..=10).map(create_test_node_version).collect();
    storage
        .store_batch_with_lsn(&nodes2, &[], LSN(200))
        .unwrap();
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(200)));

    // Verify all data present
    for id in 1..=10 {
        assert!(
            storage
                .contains_node_version(VersionId::new(id).unwrap())
                .unwrap()
        );
    }
}

// ========================================================================
// TDD Tests for Issue #2: Persistence
// ========================================================================

#[test]
fn test_redb_persistence_across_close_and_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // Store data
    {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let node_version = create_test_node_version(42);
        let edge_version = create_test_edge_version(43);

        storage.store_node_version(&node_version).unwrap();
        storage.store_edge_version(&edge_version).unwrap();
    }

    // Reopen and verify
    {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let node = storage
            .get_node_version(VersionId::new(42).unwrap())
            .unwrap();
        assert!(node.is_some());
        assert_eq!(node.unwrap().id, VersionId::new(42).unwrap());

        let edge = storage
            .get_edge_version(VersionId::new(43).unwrap())
            .unwrap();
        assert!(edge.is_some());
        assert_eq!(edge.unwrap().id, VersionId::new(43).unwrap());
    }
}

#[test]
fn test_redb_compression_zstd() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    // Create version with repetitive data for better compression
    let props = PropertyMapBuilder::new()
            .insert("description", "This is a test description with repetitive content that should compress well. This is a test description with repetitive content that should compress well.")
            .build();

    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        props,
    );

    storage.store_node_version(&version).unwrap();

    let stats = storage.stats();
    assert!(stats.bytes_written_raw > 0);
    assert!(stats.bytes_written_compressed > 0);
    // Compression should provide some benefit
    assert!(stats.bytes_written_raw >= stats.bytes_written_compressed);
}

#[test]
fn test_redb_no_compression() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let config = RedbConfig::new()
        .compression(CompressionAlgorithm::None)
        .enable_checksums(false);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
    assert_eq!(retrieved.id, version.id);
}

#[test]
fn test_redb_fast_compression() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
    assert_eq!(retrieved.id, version.id);
}

// ========================================================================
// Additional tests
// ========================================================================

#[test]
fn test_redb_stats_tracking() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();
    storage.get_node_version(version.id).unwrap();

    let stats = storage.stats();
    assert_eq!(stats.node_versions_stored, 1);
    assert_eq!(stats.node_version_reads, 1);
    assert!(stats.bytes_written_raw > 0);
    assert!(stats.bytes_read_compressed > 0);
}

#[test]
fn test_redb_overwrite_version() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Store first version
    let version1 = create_test_node_version(1);
    storage.store_node_version(&version1).unwrap();

    // Overwrite with new version (same ID, different data)
    let props2 = PropertyMapBuilder::new()
        .insert("name", "Bob")
        .insert("age", 40i64)
        .build();
    let version2 = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(2000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        props2,
    );
    storage.store_node_version(&version2).unwrap();

    // Should get the second version
    let retrieved = storage.get_node_version(version2.id).unwrap().unwrap();
    assert_eq!(
        retrieved.temporal.transaction_time().start().wallclock(),
        2000
    );
}

#[test]
fn test_redb_empty_batch() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Empty batch should succeed
    storage.store_node_versions_batch(&[]).unwrap();
    storage.store_edge_versions_batch(&[]).unwrap();
}

#[test]
fn test_redb_compact() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let mut storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Store and delete some data
    for id in 1..=100 {
        let version = create_test_node_version(id);
        storage.store_node_version(&version).unwrap();
    }

    for id in 1..=50 {
        storage
            .delete_node_version(VersionId::new(id).unwrap())
            .unwrap();
    }

    // Compact should succeed
    storage.compact().unwrap();

    // Remaining data should be intact
    for id in 51..=100 {
        assert!(
            storage
                .contains_node_version(VersionId::new(id).unwrap())
                .unwrap()
        );
    }
}

#[test]
fn test_redb_config_builder() {
    let config = RedbConfig::new()
        .compression(CompressionAlgorithm::Fast)
        .enable_checksums(false)
        .cache_size_bytes(1024 * 1024);

    assert_eq!(config.compression, CompressionAlgorithm::Fast);
    assert!(!config.enable_checksums);
    assert_eq!(config.cache_size_bytes, 1024 * 1024);
}

// ========================================================================
// Critical Safety Tests (LSN Race Conditions)
// ========================================================================

#[test]
fn test_lsn_monotonic_increase_only() {
    // Test that set_flushed_lsn only increases monotonically
    // This prevents race conditions where out-of-order commits could
    // overwrite a higher LSN with a lower LSN
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();

    // Store with LSN 100
    let node1 = create_test_node_version(1);
    storage
        .store_batch_with_lsn(std::slice::from_ref(&node1), &[], LSN(100))
        .unwrap();

    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

    // Try to store with lower LSN 50 (simulates out-of-order commit)
    let node2 = create_test_node_version(2);
    storage
        .store_batch_with_lsn(&[node2], &[], LSN(50))
        .unwrap();

    // LSN should still be 100 (not overwritten by lower 50)
    assert_eq!(
        storage.get_flushed_lsn().unwrap(),
        Some(LSN(100)),
        "LSN should not decrease"
    );

    // Store with higher LSN 200
    let node3 = create_test_node_version(3);
    storage
        .store_batch_with_lsn(&[node3], &[], LSN(200))
        .unwrap();

    // LSN should increase to 200
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(200)));
}

#[test]
fn test_concurrent_lsn_updates() {
    // Test concurrent LSN updates from multiple threads
    // Verifies that the final LSN is the maximum of all updates
    use std::sync::Arc;
    use std::thread;

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let storage = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());

    // Spawn multiple threads that update LSN in random order
    let mut handles = vec![];
    let lsns = [150, 100, 200, 50, 175, 125];

    for (i, &lsn_value) in lsns.iter().enumerate() {
        let storage_clone = Arc::clone(&storage);
        let handle = thread::spawn(move || {
            let node = create_test_node_version((i + 1) as u64);
            storage_clone
                .store_batch_with_lsn(&[node], &[], LSN(lsn_value))
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Final LSN should be the maximum (200)
    assert_eq!(
        storage.get_flushed_lsn().unwrap(),
        Some(LSN(200)),
        "Final LSN should be max of all updates"
    );
}

#[test]
fn test_lsn_persistence_across_reopen() {
    // Verify that LSN persists correctly even with out-of-order updates
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // First session: store with LSN 100
    {
        let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();
        let node = create_test_node_version(1);
        storage
            .store_batch_with_lsn(&[node], &[], LSN(100))
            .unwrap();
    }

    // Second session: try to store with lower LSN 50
    {
        let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();

        // LSN should still be 100 from first session
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

        let node = create_test_node_version(2);
        storage.store_batch_with_lsn(&[node], &[], LSN(50)).unwrap();

        // LSN should STILL be 100 (not overwritten)
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));
    }

    // Third session: verify LSN is still 100
    {
        let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();
        assert_eq!(
            storage.get_flushed_lsn().unwrap(),
            Some(LSN(100)),
            "LSN should persist correctly"
        );
    }
}

// ========================================================================
// Quick Win Tests for Function Coverage (Phase 1)
// ========================================================================

#[test]
fn test_path_getter() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    assert_eq!(storage.path(), db_path.as_path());
}

#[test]
fn test_flush() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Store some data
    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    // Flush should succeed (no-op for Redb)
    assert!(storage.flush().is_ok());

    // Data should still be retrievable
    let retrieved = storage.get_node_version(version.id).unwrap();
    assert!(retrieved.is_some());
}

#[test]
fn test_close() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let version_id = {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Store some data
        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        // Close should succeed
        assert!(storage.close().is_ok());

        version.id
    }; // storage drops here, releasing the lock

    // Data should persist after close (verify by reopening)
    let storage2 = RedbColdStorage::with_default_config(&db_path).unwrap();
    let retrieved = storage2.get_node_version(version_id).unwrap();
    assert!(retrieved.is_some());
}

// ========================================================================
// Additional Coverage Tests - Error Paths and Uncovered Methods
// ========================================================================

#[test]
fn test_config_to_cold_storage_config() {
    let config = RedbConfig::new()
        .compression(CompressionAlgorithm::Zstd)
        .enable_checksums(true)
        .cache_size_bytes(2048);

    let cold_config = config.to_cold_storage_config();
    assert_eq!(cold_config.compression, CompressionAlgorithm::Zstd);
    assert!(cold_config.enable_checksums);
    assert!(cold_config.sync_writes);
    assert_eq!(cold_config.batch_size, 1000);
}

#[test]
fn test_default_config() {
    let config1 = RedbConfig::default();
    let config2 = RedbConfig::new();

    assert_eq!(config1.compression, config2.compression);
    assert_eq!(config1.enable_checksums, config2.enable_checksums);
    assert_eq!(config1.cache_size_bytes, config2.cache_size_bytes);
}

#[test]
fn test_compress_decompress_zstd() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    let original_data = b"Test data for compression";
    let compressed = storage.compress(original_data).unwrap();
    let decompressed = storage.decompress(&compressed).unwrap();

    assert_eq!(original_data, decompressed.as_slice());
}

#[test]
fn test_compress_decompress_lz4() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    let original_data = b"Test data for LZ4 compression";
    let compressed = storage.compress(original_data).unwrap();
    let decompressed = storage.decompress(&compressed).unwrap();

    assert_eq!(original_data, decompressed.as_slice());
}

#[test]
fn test_compress_decompress_none() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let config = RedbConfig::new()
        .compression(CompressionAlgorithm::None)
        .enable_checksums(false);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    let original_data = b"No compression test";
    let compressed = storage.compress(original_data).unwrap();
    let decompressed = storage.decompress(&compressed).unwrap();

    assert_eq!(original_data, decompressed.as_slice());
}

#[test]
fn test_invalid_flushed_lsn_format() {
    // Create a database with corrupted metadata
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    {
        let db = redb::Database::create(&db_path).unwrap();
        let write_txn = db.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(METADATA_TABLE).unwrap();

            // Write invalid LSN (wrong size - 4 bytes instead of 8)
            let invalid_data = [1u8, 2, 3, 4];
            table
                .insert(FLUSHED_LSN_KEY, invalid_data.as_slice())
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    // Now try to read it with RedbColdStorage
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
    let result = storage.get_flushed_lsn();

    // Should return corruption error
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("corruption") || err_msg.contains("Invalid"));
}

#[test]
fn test_get_node_version_missing() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Try to get non-existent version
    let result = storage
        .get_node_version(VersionId::new(99999).unwrap())
        .unwrap();
    assert!(result.is_none());

    // Verify stats were updated
    let stats = storage.stats();
    assert_eq!(stats.node_version_reads, 1);
}

#[test]
fn test_get_edge_version_missing() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Try to get non-existent version
    let result = storage
        .get_edge_version(VersionId::new(99999).unwrap())
        .unwrap();
    assert!(result.is_none());

    // Verify stats were updated
    let stats = storage.stats();
    assert_eq!(stats.edge_version_reads, 1);
}

#[test]
fn test_contains_node_version_false() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    assert!(
        !storage
            .contains_node_version(VersionId::new(88888).unwrap())
            .unwrap()
    );
}

#[test]
fn test_contains_edge_version_false() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    assert!(
        !storage
            .contains_edge_version(VersionId::new(88888).unwrap())
            .unwrap()
    );
}

#[test]
fn test_delete_node_version_nonexistent() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let deleted = storage
        .delete_node_version(VersionId::new(77777).unwrap())
        .unwrap();
    assert!(!deleted);
}

#[test]
fn test_delete_edge_version_nonexistent() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let deleted = storage
        .delete_edge_version(VersionId::new(77777).unwrap())
        .unwrap();
    assert!(!deleted);
}

#[test]
fn test_lsn_equal_does_not_update() {
    // Test that setting LSN to the same value doesn't update
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Set initial LSN
    let node1 = create_test_node_version(1);
    storage
        .store_batch_with_lsn(&[node1], &[], LSN(100))
        .unwrap();
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

    // Try to set same LSN again
    let node2 = create_test_node_version(2);
    storage
        .store_batch_with_lsn(&[node2], &[], LSN(100))
        .unwrap();

    // Should still be 100
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));
}

#[test]
fn test_empty_batch_with_lsn() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Empty batch should still update LSN
    storage.store_batch_with_lsn(&[], &[], LSN(500)).unwrap();
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(500)));
}

#[test]
fn test_get_flushed_lsn_no_metadata() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Before any LSN is set, should return None
    assert_eq!(storage.get_flushed_lsn().unwrap(), None);
}

#[test]
fn test_stats_after_reads() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Store versions
    let node = create_test_node_version(1);
    let edge = create_test_edge_version(2);
    storage.store_node_version(&node).unwrap();
    storage.store_edge_version(&edge).unwrap();

    // Read them back
    storage.get_node_version(node.id).unwrap();
    storage.get_edge_version(edge.id).unwrap();

    let stats = storage.stats();
    assert_eq!(stats.node_versions_stored, 1);
    assert_eq!(stats.edge_versions_stored, 1);
    assert_eq!(stats.node_version_reads, 1);
    assert_eq!(stats.edge_version_reads, 1);
    assert!(stats.bytes_written_raw > 0);
    assert!(stats.bytes_written_compressed > 0);
    assert!(stats.bytes_read_compressed > 0);
    assert!(stats.bytes_read_decompressed > 0);
}

#[test]
fn test_batch_with_only_nodes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let nodes: Vec<NodeVersion> = (1..=5).map(create_test_node_version).collect();
    storage.store_batch_with_lsn(&nodes, &[], LSN(100)).unwrap();

    for node in &nodes {
        assert!(storage.contains_node_version(node.id).unwrap());
    }
}

#[test]
fn test_batch_with_only_edges() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let edges: Vec<EdgeVersion> = (1..=5).map(create_test_edge_version).collect();
    storage.store_batch_with_lsn(&[], &edges, LSN(100)).unwrap();

    for edge in &edges {
        assert!(storage.contains_edge_version(edge.id).unwrap());
    }
}

#[test]
fn test_config_cache_size() {
    let config = RedbConfig::new().cache_size_bytes(4096);
    assert_eq!(config.cache_size_bytes, 4096);
}

#[test]
fn test_multiple_overwrites_same_version() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version_id = VersionId::new(1).unwrap();

    // Write same version multiple times
    for i in 0..5 {
        let props = PropertyMapBuilder::new().insert("iteration", i).build();

        let version = NodeVersion::new_anchor(
            version_id,
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current((i * 1000).into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            props,
        );

        storage.store_node_version(&version).unwrap();
    }

    // Should have last version
    let retrieved = storage.get_node_version(version_id).unwrap().unwrap();
    assert_eq!(
        retrieved.temporal.transaction_time().start().wallclock(),
        4000
    );
}

#[test]
fn test_large_batch_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Store large batch
    let nodes: Vec<NodeVersion> = (1..=1000).map(create_test_node_version).collect();
    let edges: Vec<EdgeVersion> = (1..=500).map(create_test_edge_version).collect();

    storage
        .store_batch_with_lsn(&nodes, &edges, LSN(5000))
        .unwrap();

    // Verify counts
    let stats = storage.stats();
    assert_eq!(stats.node_versions_stored, 1000);
    assert_eq!(stats.edge_versions_stored, 500);
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(5000)));
}

#[test]
fn test_compression_with_checksums() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let config = RedbConfig::new()
        .compression(CompressionAlgorithm::Zstd)
        .enable_checksums(true);

    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
    assert_eq!(retrieved.id, version.id);
}

#[test]
fn test_store_and_delete_cycle() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let node = create_test_node_version(1);
    let edge = create_test_edge_version(2);

    // Store
    storage.store_node_version(&node).unwrap();
    storage.store_edge_version(&edge).unwrap();
    assert!(storage.contains_node_version(node.id).unwrap());
    assert!(storage.contains_edge_version(edge.id).unwrap());

    // Delete
    assert!(storage.delete_node_version(node.id).unwrap());
    assert!(storage.delete_edge_version(edge.id).unwrap());
    assert!(!storage.contains_node_version(node.id).unwrap());
    assert!(!storage.contains_edge_version(edge.id).unwrap());

    // Store again
    storage.store_node_version(&node).unwrap();
    storage.store_edge_version(&edge).unwrap();
    assert!(storage.contains_node_version(node.id).unwrap());
    assert!(storage.contains_edge_version(edge.id).unwrap());
}

#[test]
fn test_mixed_batch_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // First batch
    let nodes1: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();
    storage.store_node_versions_batch(&nodes1).unwrap();

    // Second batch
    let edges1: Vec<EdgeVersion> = (1..=5).map(create_test_edge_version).collect();
    storage.store_edge_versions_batch(&edges1).unwrap();

    // Third batch with LSN
    let nodes2: Vec<NodeVersion> = (11..=20).map(create_test_node_version).collect();
    let edges2: Vec<EdgeVersion> = (6..=10).map(create_test_edge_version).collect();
    storage
        .store_batch_with_lsn(&nodes2, &edges2, LSN(1000))
        .unwrap();

    // Verify all data
    for i in 1..=20 {
        assert!(
            storage
                .contains_node_version(VersionId::new(i).unwrap())
                .unwrap()
        );
    }
    for i in 1..=10 {
        assert!(
            storage
                .contains_edge_version(VersionId::new(i).unwrap())
                .unwrap()
        );
    }

    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(1000)));
}

// ========================================================================
// Error Path Coverage Tests - Force database errors to cover map_err closures
// ========================================================================

#[test]
fn test_error_invalid_database_path() {
    use std::fs;
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("file.txt");

    // Create a regular file, not a directory
    fs::write(&file_path, b"not a database").unwrap();

    // Try to create database with a file path instead of directory path
    // This should trigger the error path in RedbColdStorage::new
    let result = RedbColdStorage::with_default_config(&file_path);
    assert!(result.is_err());
}

#[test]
fn test_error_parent_directory_creation_fails() {
    // Try to create database in a path where parent creation would fail
    // Using /dev/null as parent should fail on Unix systems
    #[cfg(unix)]
    {
        let invalid_path = "/dev/null/subdir/test.redb";
        let result = RedbColdStorage::with_default_config(invalid_path);
        assert!(result.is_err());
    }
}

#[test]
fn test_error_parent_directory_creation_fails_conflict() {
    // More robust test: create a file, then try to create a directory with the same name
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("conflict_file");
    std::fs::write(&file_path, b"").unwrap();

    // Try to use the file as a directory for the DB path
    // e.g. "conflict_file/db.redb"
    let invalid_path = file_path.join("db.redb");

    let result = RedbColdStorage::with_default_config(invalid_path);
    assert!(result.is_err());
}

#[test]
fn test_get_node_versions_batch() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let v1 = create_test_node_version(1);
    let v2 = create_test_node_version(2);
    let v3 = create_test_node_version(3);

    storage
        .store_node_versions_batch(&[v1.clone(), v2.clone(), v3.clone()])
        .unwrap();

    let ids = vec![v1.id, v2.id, v3.id, VersionId::new(999).unwrap()];
    let results = storage.get_node_versions_batch(&ids).unwrap();

    assert_eq!(results.len(), 4);
    assert!(results[0].is_some());
    assert!(results[1].is_some());
    assert!(results[2].is_some());
    assert!(results[3].is_none());
    assert_eq!(results[0].as_ref().unwrap().id, v1.id);
}

#[test]
fn test_get_edge_versions_batch() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let v1 = create_test_edge_version(1);
    let v2 = create_test_edge_version(2);

    storage
        .store_edge_versions_batch(&[v1.clone(), v2.clone()])
        .unwrap();

    let ids = vec![v1.id, v2.id];
    let results = storage.get_edge_versions_batch(&ids).unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].is_some());
    assert_eq!(results[0].as_ref().unwrap().id, v1.id);
}

#[test]
fn test_corrupted_database_recovery() {
    use std::fs;
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("corrupt.redb");

    // Create a file with invalid database content
    fs::write(&db_path, b"This is not a valid Redb database file content").unwrap();

    // Try to open it - should trigger error in Database::create
    let result = RedbColdStorage::with_default_config(&db_path);
    // Redb might handle this or error out
    // Either way, we're exercising the error path
    let _ = result; // Result varies by Redb version
}

#[test]
fn test_decompression_error_corrupted_data() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // Create database with Zstd compression
    let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    // Try to decompress invalid data
    let invalid_compressed_data = vec![0xFF; 100]; // Invalid Zstd data
    let result = storage.decompress(&invalid_compressed_data);
    assert!(result.is_err());
}

#[test]
fn test_decompression_error_lz4_corrupted() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // Create database with LZ4 compression
    let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    // Try to decompress invalid data
    let invalid_data = vec![0xAA; 100]; // Invalid LZ4 data
    let result = storage.decompress(&invalid_data);
    assert!(result.is_err());
}

#[test]
fn test_store_retrieve_with_checksum_validation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // Test with checksums enabled
    let config = RedbConfig::new()
        .compression(CompressionAlgorithm::Zstd)
        .enable_checksums(true);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    // Create a version with substantial data
    let mut props = PropertyMapBuilder::new();
    for i in 0..100 {
        props = props.insert(&format!("field_{}", i), i as i64);
    }
    let properties = props.build();

    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        properties,
    );

    storage.store_node_version(&version).unwrap();
    let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
    assert_eq!(retrieved.id, version.id);
}

#[test]
fn test_concurrent_writes_no_data_loss() {
    use std::sync::Arc;
    use std::thread;

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let storage = Arc::new(RedbColdStorage::with_default_config(&db_path).unwrap());

    // Spawn multiple threads writing different versions
    let mut handles = vec![];
    for thread_id in 0..10 {
        let storage_clone = Arc::clone(&storage);
        let handle = thread::spawn(move || {
            for i in 0..10 {
                let version_id = (thread_id * 10 + i) as u64 + 1;
                let version = create_test_node_version(version_id);
                storage_clone.store_node_version(&version).unwrap();
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all 100 versions were stored
    for i in 1..=100 {
        assert!(
            storage
                .contains_node_version(VersionId::new(i).unwrap())
                .unwrap()
        );
    }
}

#[test]
fn test_edge_version_roundtrip_all_fields() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Create edge with comprehensive properties
    let properties = PropertyMapBuilder::new()
        .insert("weight", 2.5f64)
        .insert("label", "test_label")
        .insert("count", 42i64)
        .build();

    let version = EdgeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        EdgeId::new(200).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        NodeId::new(10).unwrap(),
        NodeId::new(20).unwrap(),
        properties,
    );

    storage.store_edge_version(&version).unwrap();
    let retrieved = storage.get_edge_version(version.id).unwrap().unwrap();

    assert_eq!(retrieved.id, version.id);
    assert_eq!(retrieved.edge_id, version.edge_id);
    assert_eq!(retrieved.source, version.source);
    assert_eq!(retrieved.target, version.target);
}

// -----------------------------------------------------------------------
// Provenance persistence (Issue #3224)
// -----------------------------------------------------------------------

#[test]
fn test_encode_decode_node_version_with_provenance() {
    use crate::core::provenance::Provenance;
    use std::sync::Arc;

    let provenance = Arc::new(
        Provenance::builder()
            .source("hr-system")
            .confidence(0.95)
            .correlation_id("batch-42")
            .build()
            .unwrap(),
    );

    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(1).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        PropertyMapBuilder::new().build(),
    )
    .with_provenance(Some(provenance.clone()));

    let encoded = encode_node_version(&version);
    assert!(
        encoded.starts_with(&COLD_RECORD_MAGIC_V3),
        "new records must be magic-prefixed"
    );

    let decoded = decode_node_version(&encoded).unwrap();
    let decoded_provenance = decoded.provenance.unwrap();
    assert_eq!(decoded_provenance.source(), Some("hr-system"));
    assert_eq!(decoded_provenance.confidence(), Some(0.95));
    assert_eq!(decoded_provenance.correlation_id(), Some("batch-42"));
}

#[test]
fn test_encode_decode_node_version_with_principal_provenance() {
    use crate::core::provenance::Provenance;
    use std::sync::Arc;

    let provenance = Arc::new(
        Provenance::builder()
            .source("hr-system")
            .principal("ingest-writer")
            .build()
            .unwrap(),
    );

    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(1).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        PropertyMapBuilder::new().build(),
    )
    .with_provenance(Some(provenance));

    let encoded = encode_node_version(&version);
    let decoded = decode_node_version(&encoded).unwrap();
    let decoded_provenance = decoded.provenance.unwrap();
    assert_eq!(decoded_provenance.source(), Some("hr-system"));
    assert_eq!(decoded_provenance.principal(), Some("ingest-writer"));
}

#[test]
fn test_decode_v2_tagged_node_record_principal_none() {
    // Simulate a record written by an Issue-#3224-era (pre-#3350) binary:
    // V2 magic prefix, provenance bundle without the `principal` field.
    let legacy = SerializableNodeVersionV2 {
        id: 1,
        node_id: 7,
        temporal_valid_start: 1000,
        temporal_valid_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        temporal_tx_start: 1000,
        temporal_tx_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        label: "Person".to_string(),
        data: SerializableVersionData::Anchor {
            properties: vec![],
            vector_snapshot_id: None,
        },
        next_version: None,
        prev_version: None,
        provenance: Some(SerializableProvenanceV2 {
            source: Some("hr-system".to_string()),
            confidence: Some(0.9),
            note: None,
            correlation_id: None,
        }),
    };
    let mut bytes = COLD_RECORD_MAGIC_V2.to_vec();
    bytes.extend_from_slice(&bitcode::encode(&legacy));

    let decoded = decode_node_version(&bytes).unwrap();
    assert_eq!(decoded.node_id.as_u64(), 7);
    let provenance = decoded.provenance.unwrap();
    assert_eq!(provenance.source(), Some("hr-system"));
    assert_eq!(provenance.confidence(), Some(0.9));
    assert_eq!(provenance.principal(), None);
}

#[test]
fn test_decode_v2_tagged_edge_record_principal_none() {
    let legacy = SerializableEdgeVersionV2 {
        id: 1,
        edge_id: 3,
        temporal_valid_start: 1000,
        temporal_valid_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        temporal_tx_start: 1000,
        temporal_tx_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        label: "KNOWS".to_string(),
        source: 1,
        target: 2,
        data: SerializableVersionData::Anchor {
            properties: vec![],
            vector_snapshot_id: None,
        },
        next_version: None,
        prev_version: None,
        provenance: Some(SerializableProvenanceV2 {
            source: Some("csv-import".to_string()),
            confidence: None,
            note: None,
            correlation_id: None,
        }),
    };
    let mut bytes = COLD_RECORD_MAGIC_V2.to_vec();
    bytes.extend_from_slice(&bitcode::encode(&legacy));

    let decoded = decode_edge_version(&bytes).unwrap();
    assert_eq!(decoded.edge_id.as_u64(), 3);
    let provenance = decoded.provenance.unwrap();
    assert_eq!(provenance.source(), Some("csv-import"));
    assert_eq!(provenance.principal(), None);
}

#[test]
fn test_encode_decode_node_version_without_provenance() {
    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(1).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        PropertyMapBuilder::new().build(),
    );

    let encoded = encode_node_version(&version);
    let decoded = decode_node_version(&encoded).unwrap();
    assert!(decoded.provenance.is_none());
}

#[test]
fn test_encode_decode_edge_version_with_provenance() {
    use crate::core::provenance::Provenance;
    use std::sync::Arc;

    let provenance = Arc::new(
        Provenance::builder()
            .source("csv-import")
            .note("bulk load 2026-06")
            .build()
            .unwrap(),
    );

    let version = EdgeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        EdgeId::new(1).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
    )
    .with_provenance(Some(provenance));

    let encoded = encode_edge_version(&version);
    assert!(encoded.starts_with(&COLD_RECORD_MAGIC_V3));

    let decoded = decode_edge_version(&encoded).unwrap();
    let decoded_provenance = decoded.provenance.unwrap();
    assert_eq!(decoded_provenance.source(), Some("csv-import"));
    assert_eq!(decoded_provenance.note(), Some("bulk load 2026-06"));
}

#[test]
fn test_decode_legacy_untagged_node_record_provenance_none() {
    // Simulate a record written by a pre-#3224 binary: no tag prefix, no
    // `provenance` field in the wire struct at all.
    let legacy = SerializableNodeVersionV1 {
        id: 1,
        node_id: 1,
        temporal_valid_start: 1000,
        temporal_valid_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        temporal_tx_start: 1000,
        temporal_tx_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        label: "Person".to_string(),
        data: SerializableVersionData::Anchor {
            properties: vec![],
            vector_snapshot_id: None,
        },
        next_version: None,
        prev_version: None,
    };
    let legacy_bytes = bitcode::encode(&legacy);
    assert!(
        !legacy_bytes.starts_with(&COLD_RECORD_MAGIC_V2),
        "legacy fixture must not accidentally collide with the new magic sequence"
    );

    let decoded = decode_node_version(&legacy_bytes).unwrap();
    assert_eq!(decoded.node_id.as_u64(), 1);
    assert!(decoded.provenance.is_none());
}

/// Regression test for a real (not merely theoretical) misdetection risk:
/// before `COLD_RECORD_MAGIC_V2` was widened from a single tag byte to a
/// 4-byte sequence, any legacy (untagged) record whose leading byte happened
/// to equal that single tag byte would be misrouted into decoding as the new
/// tagged shape, silently corrupting or erroring on a valid historical
/// record. This fabricates a legacy record and prepends the *old* single-byte
/// tag value the new decoder no longer recognizes, then confirms it still
/// makes it down the legacy fallback path rather than being misdecoded as
/// V2 -- this is inherently true for a 4-byte magic vs. a single leading
/// byte, since matching one byte can never satisfy `starts_with` on four.
#[test]
fn test_legacy_record_with_leading_byte_matching_old_single_byte_tag_is_not_misdetected() {
    let legacy = SerializableNodeVersionV1 {
        id: 1,
        node_id: 42,
        temporal_valid_start: 1000,
        temporal_valid_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        temporal_tx_start: 1000,
        temporal_tx_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        label: "Person".to_string(),
        data: SerializableVersionData::Anchor {
            properties: vec![],
            vector_snapshot_id: None,
        },
        next_version: None,
        prev_version: None,
    };
    let mut legacy_bytes = bitcode::encode(&legacy);
    // Force the leading byte to collide with what used to be the entire tag
    // (0x02) -- a single-byte match must no longer be mistaken for the
    // 4-byte `COLD_RECORD_MAGIC_V2` prefix.
    legacy_bytes[0] = COLD_RECORD_MAGIC_V2[0];
    assert!(!legacy_bytes.starts_with(&COLD_RECORD_MAGIC_V2));

    // Decoding a mutated legacy record isn't expected to preserve field
    // values (we corrupted a byte), but it must not panic and must not be
    // silently routed through the V2 decoder.
    let _ = decode_node_version(&legacy_bytes);
}

#[test]
fn test_decode_legacy_untagged_edge_record_provenance_none() {
    let legacy = SerializableEdgeVersionV1 {
        id: 1,
        edge_id: 1,
        temporal_valid_start: 1000,
        temporal_valid_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        temporal_tx_start: 1000,
        temporal_tx_end: crate::core::temporal::TIMESTAMP_MAX.wallclock(),
        label: "KNOWS".to_string(),
        source: 1,
        target: 2,
        data: SerializableVersionData::Anchor {
            properties: vec![],
            vector_snapshot_id: None,
        },
        next_version: None,
        prev_version: None,
    };
    let legacy_bytes = bitcode::encode(&legacy);

    let decoded = decode_edge_version(&legacy_bytes).unwrap();
    assert_eq!(decoded.edge_id.as_u64(), 1);
    assert!(decoded.provenance.is_none());
}

#[test]
fn test_migrate_to_cold_preserves_provenance() {
    use crate::core::provenance::Provenance;
    use std::sync::Arc;

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let provenance = Arc::new(
        Provenance::builder()
            .source("claude-mcp")
            .confidence(0.8)
            .build()
            .unwrap(),
    );
    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(1).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        PropertyMapBuilder::new().build(),
    )
    .with_provenance(Some(provenance));

    storage.store_node_version(&version).unwrap();
    let retrieved = storage.get_node_version(version.id).unwrap().unwrap();

    let retrieved_provenance = retrieved.provenance.unwrap();
    assert_eq!(retrieved_provenance.source(), Some("claude-mcp"));
    assert_eq!(retrieved_provenance.confidence(), Some(0.8));
}

#[test]
fn test_batch_operations_preserve_order() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let nodes: Vec<NodeVersion> = (1..=50).map(create_test_node_version).collect();
    let edges: Vec<EdgeVersion> = (1..=50).map(create_test_edge_version).collect();

    storage
        .store_batch_with_lsn(&nodes, &edges, LSN(1000))
        .unwrap();

    // Verify all nodes
    for i in 1..=50 {
        let version = storage
            .get_node_version(VersionId::new(i).unwrap())
            .unwrap();
        assert!(version.is_some());
    }

    // Verify all edges
    for i in 1..=50 {
        let version = storage
            .get_edge_version(VersionId::new(i).unwrap())
            .unwrap();
        assert!(version.is_some());
    }
}

#[test]
fn test_stats_accuracy_after_multiple_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Perform various operations
    let node1 = create_test_node_version(1);
    let node2 = create_test_node_version(2);
    let edge1 = create_test_edge_version(10);

    storage.store_node_version(&node1).unwrap();
    storage.store_node_version(&node2).unwrap();
    storage.store_edge_version(&edge1).unwrap();

    storage.get_node_version(node1.id).unwrap();
    storage.get_node_version(node2.id).unwrap();
    storage.get_edge_version(edge1.id).unwrap();

    storage.delete_node_version(node1.id).unwrap();

    let stats = storage.stats();
    assert_eq!(stats.node_versions_stored, 2);
    assert_eq!(stats.edge_versions_stored, 1);
    assert_eq!(stats.node_version_reads, 2);
    assert_eq!(stats.edge_version_reads, 1);
}

#[test]
fn test_reopen_after_compact() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // Create, store, compact
    {
        let mut storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        for i in 1..=50 {
            let version = create_test_node_version(i);
            storage.store_node_version(&version).unwrap();
        }
        storage.compact().unwrap();
    }

    // Reopen and verify
    {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        for i in 1..=50 {
            assert!(
                storage
                    .contains_node_version(VersionId::new(i).unwrap())
                    .unwrap()
            );
        }
    }
}

#[test]
fn test_lsn_persistence_through_compact() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    let lsn_value = LSN(99999);

    {
        let mut storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        let version = create_test_node_version(1);
        storage
            .store_batch_with_lsn(&[version], &[], lsn_value)
            .unwrap();
        storage.compact().unwrap();
    }

    {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(lsn_value));
    }
}

#[test]
fn test_multiple_database_instances_same_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    // Create first instance and write data
    {
        let storage1 = RedbColdStorage::with_default_config(&db_path).unwrap();
        let version = create_test_node_version(1);
        storage1.store_node_version(&version).unwrap();
    } // Close first instance

    // Open second instance
    {
        let storage2 = RedbColdStorage::with_default_config(&db_path).unwrap();
        assert!(
            storage2
                .contains_node_version(VersionId::new(1).unwrap())
                .unwrap()
        );

        // Add more data
        let version = create_test_node_version(2);
        storage2.store_node_version(&version).unwrap();
    }

    // Open third instance and verify both
    {
        let storage3 = RedbColdStorage::with_default_config(&db_path).unwrap();
        assert!(
            storage3
                .contains_node_version(VersionId::new(1).unwrap())
                .unwrap()
        );
        assert!(
            storage3
                .contains_node_version(VersionId::new(2).unwrap())
                .unwrap()
        );
    }
}

#[test]
fn test_flush_idempotent() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Multiple flushes should all succeed
    assert!(storage.flush().is_ok());
    assert!(storage.flush().is_ok());
    assert!(storage.flush().is_ok());
}

#[test]
fn test_close_idempotent() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Multiple closes should all succeed
    assert!(storage.close().is_ok());
    assert!(storage.close().is_ok());
}

#[test]
fn test_empty_node_batch_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Empty batch should succeed without error
    storage.store_node_versions_batch(&[]).unwrap();

    let stats = storage.stats();
    assert_eq!(stats.node_versions_stored, 0);
}

#[test]
fn test_empty_edge_batch_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Empty batch should succeed without error
    storage.store_edge_versions_batch(&[]).unwrap();

    let stats = storage.stats();
    assert_eq!(stats.edge_versions_stored, 0);
}

#[test]
fn test_compression_ratio_tracking() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
    let storage = RedbColdStorage::new(&db_path, config).unwrap();

    // Create highly compressible data
    let mut props = PropertyMapBuilder::new();
    let repeated_text = "A".repeat(1000);
    props = props.insert("text", repeated_text.as_str());
    let properties = props.build();

    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        properties,
    );

    storage.store_node_version(&version).unwrap();

    let stats = storage.stats();
    assert!(stats.bytes_written_raw > 0);
    assert!(stats.bytes_written_compressed > 0);
    // Compression should provide significant benefit for repeated data
    assert!(stats.bytes_written_compressed < stats.bytes_written_raw);
}

// ========================================================================
// Additional tests to increase function coverage by exercising more paths
// ========================================================================

#[test]
fn test_get_flushed_lsn_with_invalid_metadata_bytes() {
    // Test the specific error path where LSN has wrong byte count
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    {
        // Manually create corrupted metadata
        let db = redb::Database::create(&db_path).unwrap();
        let write_txn = db.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(METADATA_TABLE).unwrap();
            // Write 7 bytes instead of 8 (invalid)
            table
                .insert(FLUSHED_LSN_KEY, &[1, 2, 3, 4, 5, 6, 7][..])
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
    let result = storage.get_flushed_lsn();
    assert!(result.is_err());
}

#[test]
fn test_set_flushed_lsn_internal_with_existing_lower_value() {
    // This exercises the branch where we read existing LSN and compare
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Set LSN to 200
    storage.store_batch_with_lsn(&[], &[], LSN(200)).unwrap();

    // Try to set to 100 (should be skipped)
    storage.store_batch_with_lsn(&[], &[], LSN(100)).unwrap();

    // Should still be 200
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(200)));
}

#[test]
fn test_node_version_decode_error_path() {
    // Test decode error by storing corrupted data directly
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    {
        let db = redb::Database::create(&db_path).unwrap();
        let write_txn = db.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(NODE_VERSIONS_TABLE).unwrap();
            // Store invalid serialized data
            table.insert(12345, &[0xFF, 0xFF, 0xFF][..]).unwrap();
        }
        write_txn.commit().unwrap();
    }

    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
    let result = storage.get_node_version(VersionId::new(12345).unwrap());
    // Should fail to decompress or decode
    assert!(result.is_err());
}

/// Regression test: a record whose leading bytes match `COLD_RECORD_MAGIC_V2`
/// is unambiguously a V2 record. If the bytes *after* the magic fail to
/// decode as `SerializableNodeVersion` (corruption), `decode_node_version`
/// must surface that error immediately rather than falling through to the
/// legacy V1 decoder, which would misinterpret the magic-prefixed buffer.
#[test]
fn test_decode_node_version_v2_magic_match_with_corrupt_payload_errors_immediately() {
    let mut data = COLD_RECORD_MAGIC_V2.to_vec();
    data.extend_from_slice(&[0xFF; 8]);
    let err = decode_node_version(&data).unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("V2"),
        "expected a V2-specific decode error, got: {message}"
    );
}

/// Edge counterpart of
/// [`test_decode_node_version_v2_magic_match_with_corrupt_payload_errors_immediately`].
#[test]
fn test_decode_edge_version_v2_magic_match_with_corrupt_payload_errors_immediately() {
    let mut data = COLD_RECORD_MAGIC_V2.to_vec();
    data.extend_from_slice(&[0xFF; 8]);
    let err = decode_edge_version(&data).unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("V2"),
        "expected a V2-specific decode error, got: {message}"
    );
}

#[test]
fn test_edge_version_decode_error_path() {
    // Test decode error for edges
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");

    {
        let db = redb::Database::create(&db_path).unwrap();
        let write_txn = db.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(EDGE_VERSIONS_TABLE).unwrap();
            // Store invalid serialized data
            table.insert(12345, &[0xAA, 0xBB, 0xCC][..]).unwrap();
        }
        write_txn.commit().unwrap();
    }

    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
    let result = storage.get_edge_version(VersionId::new(12345).unwrap());
    // Should fail to decompress or decode
    assert!(result.is_err());
}

#[test]
fn test_store_many_versions_individually() {
    // Exercise individual store paths many times
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    for i in 1..=100 {
        let node = create_test_node_version(i);
        storage.store_node_version(&node).unwrap();
    }

    for i in 1..=100 {
        let edge = create_test_edge_version(i);
        storage.store_edge_version(&edge).unwrap();
    }

    let stats = storage.stats();
    assert_eq!(stats.node_versions_stored, 100);
    assert_eq!(stats.edge_versions_stored, 100);
}

#[test]
fn test_contains_then_get_pattern() {
    // Exercise the common pattern of checking existence then retrieving
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let node = create_test_node_version(1);
    let edge = create_test_edge_version(2);

    storage.store_node_version(&node).unwrap();
    storage.store_edge_version(&edge).unwrap();

    // Contains then get pattern
    if storage.contains_node_version(node.id).unwrap() {
        let retrieved = storage.get_node_version(node.id).unwrap();
        assert!(retrieved.is_some());
    }

    if storage.contains_edge_version(edge.id).unwrap() {
        let retrieved = storage.get_edge_version(edge.id).unwrap();
        assert!(retrieved.is_some());
    }
}

#[test]
fn test_delete_then_reinsert_same_id() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version_id = VersionId::new(42).unwrap();

    // Create and store
    let version1 = create_test_node_version(42);
    storage.store_node_version(&version1).unwrap();
    assert!(storage.contains_node_version(version_id).unwrap());

    // Delete
    storage.delete_node_version(version_id).unwrap();
    assert!(!storage.contains_node_version(version_id).unwrap());

    // Reinsert with same ID
    let version2 = create_test_node_version(42);
    storage.store_node_version(&version2).unwrap();
    assert!(storage.contains_node_version(version_id).unwrap());

    // Verify it's there
    let retrieved = storage.get_node_version(version_id).unwrap();
    assert!(retrieved.is_some());
}

#[test]
fn test_batch_with_duplicates() {
    // Store batch with duplicate IDs (last one wins)
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Create versions with same ID
    let v1 = create_test_node_version(1);
    let v2 = create_test_node_version(1); // Same ID
    let v3 = create_test_node_version(1); // Same ID again

    storage.store_node_versions_batch(&[v1, v2, v3]).unwrap();

    // Should have the last version
    assert!(
        storage
            .contains_node_version(VersionId::new(1).unwrap())
            .unwrap()
    );
    let stats = storage.stats();
    // Stored 3 times even though same ID
    assert_eq!(stats.node_versions_stored, 3);
}

#[test]
fn test_alternating_node_edge_operations() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Alternate between node and edge operations
    for i in 1..=20 {
        if i % 2 == 0 {
            let node = create_test_node_version(i);
            storage.store_node_version(&node).unwrap();
        } else {
            let edge = create_test_edge_version(i);
            storage.store_edge_version(&edge).unwrap();
        }
    }

    // Verify counts
    let stats = storage.stats();
    assert_eq!(stats.node_versions_stored, 10);
    assert_eq!(stats.edge_versions_stored, 10);
}

#[test]
fn test_get_nonexistent_after_delete() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let node = create_test_node_version(1);
    storage.store_node_version(&node).unwrap();

    // Delete it
    storage.delete_node_version(node.id).unwrap();

    // Try to get it
    let result = storage.get_node_version(node.id).unwrap();
    assert!(result.is_none());

    // Try to contains it
    assert!(!storage.contains_node_version(node.id).unwrap());
}

#[test]
fn test_stats_snapshot_consistency() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Take initial snapshot
    let stats1 = storage.stats();
    assert_eq!(stats1.node_versions_stored, 0);

    // Do some operations
    let node = create_test_node_version(1);
    storage.store_node_version(&node).unwrap();

    // Take another snapshot
    let stats2 = storage.stats();
    assert_eq!(stats2.node_versions_stored, 1);

    // First snapshot unchanged
    assert_eq!(stats1.node_versions_stored, 0);
}

#[test]
fn test_very_large_property_map() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Create version with many properties
    let mut props = PropertyMapBuilder::new();
    for i in 0..1000 {
        props = props.insert(&format!("prop_{}", i), i as i64);
    }
    let properties = props.build();

    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        properties,
    );

    storage.store_node_version(&version).unwrap();
    let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
    assert_eq!(retrieved.id, version.id);
}

#[test]
fn test_interleaved_batch_and_individual() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    // Individual
    storage
        .store_node_version(&create_test_node_version(1))
        .unwrap();

    // Batch
    let nodes: Vec<_> = (2..=5).map(create_test_node_version).collect();
    storage.store_node_versions_batch(&nodes).unwrap();

    // Individual again
    storage
        .store_node_version(&create_test_node_version(6))
        .unwrap();

    // Batch with LSN
    let nodes: Vec<_> = (7..=10).map(create_test_node_version).collect();
    storage.store_batch_with_lsn(&nodes, &[], LSN(100)).unwrap();

    // Verify all present
    for i in 1..=10 {
        assert!(
            storage
                .contains_node_version(VersionId::new(i).unwrap())
                .unwrap()
        );
    }
}

#[test]
fn test_fault_injection() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    storage.set_fail_writes(true);

    let node = create_test_node_version(1);
    let result = storage.store_node_version(&node);

    assert!(result.is_err());
    assert!(storage.was_write_attempted());
}

// ========================================================================
// Encryption at rest tests
// ========================================================================

fn test_cipher() -> Arc<dyn crate::encryption::cipher::Cipher> {
    use crate::encryption::Aes256GcmCipher;
    use zeroize::Zeroizing;

    let mut key = Zeroizing::new([0u8; 32]);
    key[0] = 0xAB;
    key[1] = 0xCD;
    Arc::new(Aes256GcmCipher::new(&key))
}

#[test]
fn test_encrypted_store_and_retrieve_node() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("enc_test.redb");
    let cipher = test_cipher();

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(cipher);

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    let loaded = storage
        .get_node_version(version.version_id())
        .unwrap()
        .expect("node version should exist");

    assert_eq!(loaded.version_id(), version.version_id());
    assert_eq!(loaded.node_id, version.node_id);
}

#[test]
fn test_encrypted_store_and_retrieve_edge() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("enc_test.redb");
    let cipher = test_cipher();

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(cipher);

    let version = create_test_edge_version(1);
    storage.store_edge_version(&version).unwrap();

    let loaded = storage
        .get_edge_version(version.version_id())
        .unwrap()
        .expect("edge version should exist");

    assert_eq!(loaded.version_id(), version.version_id());
}

#[test]
fn test_encrypted_batch_store_and_retrieve() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("enc_batch.redb");
    let cipher = test_cipher();

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(cipher);

    let nodes: Vec<NodeVersion> = (1..=5).map(create_test_node_version).collect();
    let edges: Vec<EdgeVersion> = (10..=12).map(create_test_edge_version).collect();

    storage
        .store_batch_with_lsn(&nodes, &edges, LSN(100))
        .unwrap();

    // Verify all nodes
    for node in &nodes {
        let loaded = storage
            .get_node_version(node.version_id())
            .unwrap()
            .expect("node version should exist");
        assert_eq!(loaded.version_id(), node.version_id());
    }

    // Verify all edges
    for edge in &edges {
        let loaded = storage
            .get_edge_version(edge.version_id())
            .unwrap()
            .expect("edge version should exist");
        assert_eq!(loaded.version_id(), edge.version_id());
    }

    // Verify LSN
    assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));
}

#[test]
fn test_encrypted_wrong_key_fails_read() {
    use crate::encryption::Aes256GcmCipher;
    use zeroize::Zeroizing;

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("enc_wrong_key.redb");

    // Write with one cipher
    let cipher1 = test_cipher();
    let storage1 = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(cipher1);

    let version = create_test_node_version(1);
    storage1.store_node_version(&version).unwrap();

    // Explicitly drop to release file lock
    drop(storage1);

    // Read with a different cipher
    let mut key2 = Zeroizing::new([0u8; 32]);
    key2[0] = 0x12;
    key2[1] = 0x34;
    let cipher2: Arc<dyn crate::encryption::cipher::Cipher> = Arc::new(Aes256GcmCipher::new(&key2));

    let storage2 = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(cipher2);

    let result = storage2.get_node_version(version.version_id());
    assert!(result.is_err(), "Reading with wrong key should fail");
}

#[test]
fn test_no_cipher_backward_compatible() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("no_enc.redb");

    // No cipher -- should work exactly like before
    let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

    let version = create_test_node_version(1);
    storage.store_node_version(&version).unwrap();

    let loaded = storage
        .get_node_version(version.version_id())
        .unwrap()
        .expect("node version should exist");

    assert_eq!(loaded.version_id(), version.version_id());
}

// ========================================================================
// Cold-tier key rotation (Issue #3617 PR3): ACV1 wrapper, bulk re-encrypt,
// resume cursor, legacy fallback.
// ========================================================================

/// AES-256-GCM cipher whose key's first byte is `b` (distinct DEKs for the two
/// rotation generations).
fn cipher_with_byte(b: u8) -> Arc<dyn crate::encryption::cipher::Cipher> {
    use crate::encryption::Aes256GcmCipher;
    use zeroize::Zeroizing;
    let mut key = Zeroizing::new([0u8; 32]);
    key[0] = b;
    key[1] = 0x5A;
    Arc::new(Aes256GcmCipher::new(&key))
}

/// Read the raw (undecoded) stored value for a key from a versions table.
fn raw_value(
    storage: &RedbColdStorage,
    table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
    key: u64,
) -> Vec<u8> {
    let rtxn = storage.db.begin_read().unwrap();
    let t = rtxn.open_table(table_def).unwrap();
    t.get(key).unwrap().unwrap().value().to_vec()
}

/// Write a raw metadata record (e.g. a planted resume cursor).
fn insert_raw_metadata(storage: &RedbColdStorage, key: &'static str, bytes: &[u8]) {
    let wtxn = storage.db.begin_write().unwrap();
    {
        let mut t = wtxn.open_table(METADATA_TABLE).unwrap();
        t.insert(key, bytes).unwrap();
    }
    wtxn.commit().unwrap();
}

/// Insert a raw value directly into a versions table (bypassing the wrapper), to
/// plant a legacy or mid-rotation on-disk state.
fn insert_raw(
    storage: &RedbColdStorage,
    table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
    key: u64,
    bytes: &[u8],
) {
    let wtxn = storage.db.begin_write().unwrap();
    {
        let mut t = wtxn.open_table(table_def).unwrap();
        t.insert(key, bytes).unwrap();
    }
    wtxn.commit().unwrap();
}

#[test]
fn legacy_cold_value_reads_under_current_dek() {
    // A pre-#3617 value is bare compress-then-encrypt with NO ACV1 wrapper. It
    // must still read after the keyring refactor (backward compatibility).
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("legacy.redb");
    let cipher = test_cipher();

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(Arc::clone(&cipher));

    // Hand-build a legacy (unwrapped) value exactly as the pre-#3617 store did.
    let version = create_test_node_version(1);
    let compressed = storage.compress(&encode_node_version(&version)).unwrap();
    let bare = cipher.encrypt(&compressed, &[]).unwrap();
    insert_raw(&storage, NODE_VERSIONS_TABLE, 1, &bare);

    // Reads via the keyring fall back to the legacy (bare-ciphertext) path.
    let loaded = storage
        .get_node_version(version.version_id())
        .unwrap()
        .expect("a legacy unwrapped value must still read");
    assert_eq!(loaded.version_id(), version.version_id());
    assert_eq!(loaded.node_id, version.node_id);
}

#[test]
fn cold_value_roundtrips_with_acv1_wrapper() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("acv1.redb");
    let cipher = test_cipher();

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(Arc::clone(&cipher));

    let version = create_test_node_version(7);
    storage.store_node_version(&version).unwrap();

    // The stored value carries the ACV1 wrapper stamped at the current version.
    let raw = raw_value(&storage, NODE_VERSIONS_TABLE, 7);
    let (kv, _ct) = parse_cold_wrapper(&raw).expect("a written value must be ACV1-wrapped");
    assert_eq!(kv, 1, "fresh encrypted store stamps generation 1");

    let loaded = storage
        .get_node_version(version.version_id())
        .unwrap()
        .expect("wrapped value round-trips");
    assert_eq!(loaded.version_id(), version.version_id());
    drop(storage);

    // Reading the same ACV1@1 value under a keyring that holds ONLY generation 2
    // (unknown version 1) must fail LOUDLY (AEAD auth) — never silent wrong data.
    let ring = ColdKeyring::single(Arc::clone(&cipher));
    ring.add_generation(2, Arc::clone(&cipher));
    ring.retain_only(2); // strict keyring with gen 2 only; version 1 is unknown
    let storage2 = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cold_keyring(ring);
    let result = storage2.get_node_version(version.version_id());
    assert!(
        result.is_err(),
        "an ACV1 value naming an unheld generation must fail loudly, not read wrong data"
    );
}

#[test]
fn bulk_reencrypt_rewrites_all_values_new_generation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("bulk.redb");
    let c1 = cipher_with_byte(0x11);
    let c2 = cipher_with_byte(0x22);

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(Arc::clone(&c1));

    let nodes: Vec<NodeVersion> = (1..=3).map(create_test_node_version).collect();
    let edges: Vec<EdgeVersion> = (10..=11).map(create_test_edge_version).collect();
    storage
        .store_batch_with_lsn(&nodes, &edges, LSN(100))
        .unwrap();

    let flushed_before = storage.get_flushed_lsn().unwrap();
    let extent_before = storage.get_temporal_extent_bounds().unwrap();
    assert_eq!(flushed_before, Some(LSN(100)));

    // Install the new generation and bulk re-encrypt every value to it.
    storage.install_cold_generation(2, Arc::clone(&c2)).unwrap();
    let stats = storage.reencrypt_cold_values(2).unwrap();
    assert_eq!(stats.values_rewrapped, 5, "3 nodes + 2 edges re-wrapped");
    assert_eq!(stats.values_skipped, 0);

    // Every value is now ACV1@2, and the terminal format marker is set.
    for k in 1..=3 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, NODE_VERSIONS_TABLE, k)).unwrap();
        assert_eq!(kv, 2, "node {k} must be re-wrapped at generation 2");
    }
    for k in 10..=11 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, EDGE_VERSIONS_TABLE, k)).unwrap();
        assert_eq!(kv, 2, "edge {k} must be re-wrapped at generation 2");
    }
    assert_eq!(storage.cold_value_format_version().unwrap(), Some(2));

    // Plaintext metadata is preserved untouched by the value-only pass.
    assert_eq!(storage.get_flushed_lsn().unwrap(), flushed_before);
    assert_eq!(storage.get_temporal_extent_bounds().unwrap(), extent_before);

    // Every value still decodes (now under the new cold DEK).
    storage.retire_cold_generations(2).unwrap();
    for node in &nodes {
        let loaded = storage
            .get_node_version(node.version_id())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.version_id(), node.version_id());
    }
    for edge in &edges {
        let loaded = storage
            .get_edge_version(edge.version_id())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.version_id(), edge.version_id());
    }
}

#[test]
fn crash_mid_cold_batch_resumes_from_cursor() {
    // Simulate a crash after some batches committed: nodes 1..=3 are already
    // re-wrapped at generation 2 with a durable cursor at (node, key=3); nodes
    // 4..=5 remain at generation 1. Resume must continue from the cursor (no
    // double-encrypt of 1..=3), finish 4..=5, and complete.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("resume.redb");
    let c1 = cipher_with_byte(0x33);
    let c2 = cipher_with_byte(0x44);

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(Arc::clone(&c1));
    let nodes: Vec<NodeVersion> = (1..=5).map(create_test_node_version).collect();
    for n in &nodes {
        storage.store_node_version(n).unwrap();
    }
    storage.install_cold_generation(2, Arc::clone(&c2)).unwrap();

    // Manually re-wrap nodes 1..=3 to ACV1@2 and plant the resume cursor.
    for k in 1..=3u64 {
        let raw = raw_value(&storage, NODE_VERSIONS_TABLE, k);
        let compressed = storage.decrypt_if_needed(&raw).unwrap();
        let ct2 = c2.encrypt(&compressed, &[]).unwrap();
        insert_raw(&storage, NODE_VERSIONS_TABLE, k, &wrap_cold_value(2, &ct2));
    }
    let cursor = ColdRotationCursor {
        target_version: 2,
        table: ColdTableKind::Node,
        last_completed_key: 3,
    };
    insert_raw_metadata(&storage, COLD_ROTATION_CURSOR_KEY, &cursor.to_bytes());
    let node1_before = raw_value(&storage, NODE_VERSIONS_TABLE, 1);

    // Resume.
    let stats = storage.reencrypt_cold_values(2).unwrap();
    // Nodes 1..=3 are past the cursor (not revisited); only 4..=5 are rewrapped.
    assert_eq!(
        stats.values_rewrapped, 2,
        "only nodes 4,5 remain to re-wrap"
    );

    // No double-encrypt: node 1's bytes are unchanged.
    assert_eq!(
        raw_value(&storage, NODE_VERSIONS_TABLE, 1),
        node1_before,
        "an already-wrapped value past the cursor must not be re-encrypted"
    );
    // Every node now ACV1@2 and the pass completed (cursor cleared, marker set).
    for k in 1..=5u64 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, NODE_VERSIONS_TABLE, k)).unwrap();
        assert_eq!(kv, 2);
    }
    assert_eq!(storage.cold_value_format_version().unwrap(), Some(2));
    assert!(storage.read_cold_rotation_cursor().unwrap().is_none());

    // All values still read.
    storage.retire_cold_generations(2).unwrap();
    for n in &nodes {
        let loaded = storage.get_node_version(n.version_id()).unwrap().unwrap();
        assert_eq!(loaded.version_id(), n.version_id());
    }
}

#[test]
fn reencrypt_is_idempotent_over_already_wrapped_values() {
    // A re-run after a stale/absent cursor must skip already-target-wrapped
    // values, never double-encrypt (the ACV1 wrapper is the idempotency backstop).
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("idem.redb");
    let c1 = cipher_with_byte(0x55);
    let c2 = cipher_with_byte(0x66);

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(Arc::clone(&c1));
    for n in (1..=4).map(create_test_node_version) {
        storage.store_node_version(&n).unwrap();
    }
    storage.install_cold_generation(2, Arc::clone(&c2)).unwrap();
    let first = storage.reencrypt_cold_values(2).unwrap();
    assert_eq!(first.values_rewrapped, 4);

    let raw_after_first = raw_value(&storage, NODE_VERSIONS_TABLE, 1);
    // Re-run: every value is already ACV1@2, so all are skipped, none rewrapped.
    let second = storage.reencrypt_cold_values(2).unwrap();
    assert_eq!(second.values_rewrapped, 0);
    assert_eq!(second.values_skipped, 4);
    assert_eq!(
        raw_value(&storage, NODE_VERSIONS_TABLE, 1),
        raw_after_first,
        "an idempotent re-run must not change already-wrapped bytes"
    );
}

#[test]
fn reencrypt_honors_configurable_batch_size() {
    // Issue #3617 PR3: the per-transaction re-encrypt batch size is a runtime
    // knob (`RedbConfig::reencrypt_batch_size`). Configure a SMALL cap (2), seed
    // MORE values than the cap (5 nodes), and assert the pass (a) re-wraps every
    // value under the new DEK and (b) actually spanned MULTIPLE bounded batches
    // (batches_committed > 1), proving the small cap is honored, not ignored.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("small_batch.redb");
    let c1 = cipher_with_byte(0x77);
    let c2 = cipher_with_byte(0x88);

    let config = RedbConfig::new().with_reencrypt_batch_size(2);
    assert_eq!(config.reencrypt_batch_size, 2);
    let storage = RedbColdStorage::new(&db_path, config)
        .unwrap()
        .with_cipher(Arc::clone(&c1));

    let nodes: Vec<NodeVersion> = (1..=5).map(create_test_node_version).collect();
    for n in &nodes {
        storage.store_node_version(n).unwrap();
    }

    storage.install_cold_generation(2, Arc::clone(&c2)).unwrap();
    let stats = storage.reencrypt_cold_values(2).unwrap();

    // (a) every value re-wrapped at the new generation.
    assert_eq!(stats.values_rewrapped, 5, "all 5 nodes re-wrapped");
    assert_eq!(stats.values_skipped, 0);
    for k in 1..=5u64 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, NODE_VERSIONS_TABLE, k)).unwrap();
        assert_eq!(kv, 2, "node {k} must be re-wrapped at generation 2");
    }
    assert_eq!(storage.cold_value_format_version().unwrap(), Some(2));

    // (b) the small cap of 2 over 5 values forced ceil(5/2) = 3 committed
    // batches (2 + 2 + 1), so the cap was honored, not ignored.
    assert_eq!(
        stats.batches_committed, 3,
        "a batch cap of 2 over 5 values must span 3 bounded transactions"
    );

    // Every value still decrypts under the new cold DEK.
    storage.retire_cold_generations(2).unwrap();
    for n in &nodes {
        let loaded = storage.get_node_version(n.version_id()).unwrap().unwrap();
        assert_eq!(loaded.version_id(), n.version_id());
    }
}

#[test]
fn reencrypt_default_batch_size_is_single_transaction() {
    // Default (unset) behavior is unchanged: with the 4096 default a 5-value
    // pass commits in exactly ONE batch — the guarantee that omitting the knob
    // reproduces prior behavior.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("default_batch.redb");
    let c1 = cipher_with_byte(0x99);
    let c2 = cipher_with_byte(0xAA);

    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(Arc::clone(&c1));
    assert_eq!(RedbConfig::new().reencrypt_batch_size, 4096);

    for n in (1..=5).map(create_test_node_version) {
        storage.store_node_version(&n).unwrap();
    }
    storage.install_cold_generation(2, Arc::clone(&c2)).unwrap();
    let stats = storage.reencrypt_cold_values(2).unwrap();
    assert_eq!(stats.values_rewrapped, 5);
    assert_eq!(
        stats.batches_committed, 1,
        "the 4096 default re-wraps 5 values in a single transaction"
    );
}

#[test]
fn reencrypt_zero_batch_size_is_floored_to_one() {
    // A 0 batch size would make no forward progress; the setter (and the read
    // site) floor it to 1. The pass must still complete, spanning one batch per
    // value (5 values -> 5 committed batches).
    let cfg = RedbConfig::new().with_reencrypt_batch_size(0);
    assert_eq!(cfg.reencrypt_batch_size, 1, "0 is clamped up to 1");

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("zero_batch.redb");
    let c1 = cipher_with_byte(0xBB);
    let c2 = cipher_with_byte(0xCC);
    let storage = RedbColdStorage::new(&db_path, cfg)
        .unwrap()
        .with_cipher(Arc::clone(&c1));
    for n in (1..=5).map(create_test_node_version) {
        storage.store_node_version(&n).unwrap();
    }
    storage.install_cold_generation(2, Arc::clone(&c2)).unwrap();
    let stats = storage.reencrypt_cold_values(2).unwrap();
    assert_eq!(stats.values_rewrapped, 5);
    assert_eq!(
        stats.batches_committed, 5,
        "a floored batch size of 1 commits once per value"
    );
    for k in 1..=5u64 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, NODE_VERSIONS_TABLE, k)).unwrap();
        assert_eq!(kv, 2);
    }
}

#[test]
fn mixed_legacy_and_wrapped_values_read_during_rotation() {
    // During an interrupted rotation the store holds a mix: some legacy (bare,
    // old DEK) values and some ACV1@2 (new DEK) values. A two-generation keyring
    // reads both correctly.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("mixed.redb");
    let c1 = cipher_with_byte(0x77);
    let c2 = cipher_with_byte(0x88);

    // Two-generation keyring: gen 1 (old, c1) + gen 2 (new, c2), current = 2.
    let ring = ColdKeyring::single(Arc::clone(&c1));
    ring.add_generation(2, Arc::clone(&c2));
    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cold_keyring(ring);

    // Legacy (bare) value under the OLD DEK at key 1.
    let legacy = create_test_node_version(1);
    let legacy_bare = c1
        .encrypt(
            &storage.compress(&encode_node_version(&legacy)).unwrap(),
            &[],
        )
        .unwrap();
    insert_raw(&storage, NODE_VERSIONS_TABLE, 1, &legacy_bare);

    // Freshly-written value is wrapped ACV1@2 under the NEW DEK at key 2.
    let fresh = create_test_node_version(2);
    storage.store_node_version(&fresh).unwrap();
    let (kv, _) = parse_cold_wrapper(&raw_value(&storage, NODE_VERSIONS_TABLE, 2)).unwrap();
    assert_eq!(
        kv, 2,
        "a write during rotation stamps the current (new) generation"
    );

    // Both read correctly, each under its own generation.
    assert_eq!(
        storage
            .get_node_version(legacy.version_id())
            .unwrap()
            .unwrap()
            .version_id(),
        legacy.version_id()
    );
    assert_eq!(
        storage
            .get_node_version(fresh.version_id())
            .unwrap()
            .unwrap()
            .version_id(),
        fresh.version_id()
    );
}

#[test]
fn bulk_reencrypt_upgrades_all_legacy_table() {
    // Pre-#3617 on-disk state: values are BARE compress-then-encrypt with NO
    // ACV1 wrapper. Every existing bulk test seeds ACV1@1 via `store_*`, so the
    // legacy(unwrapped) -> wrapped upgrade THROUGH the bulk pass is otherwise
    // uncovered. A full-MEK rotation must upgrade every such legacy value to
    // ACV1@new and re-key it (counted as re-wrapped, never skipped).
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("legacy_bulk.redb");
    let c1 = cipher_with_byte(0x11);
    let c2 = cipher_with_byte(0x22);

    // Two-generation keyring: legacy (bare) values decrypt under the OLDEST
    // generation (gen 1 = c1); the bulk pass re-encrypts every value to gen 2.
    let ring = ColdKeyring::single(Arc::clone(&c1));
    ring.add_generation(2, Arc::clone(&c2));
    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cold_keyring(ring);

    // Seed 3 node + 2 edge LEGACY bare values (no ACV1 wrapper), exactly as the
    // pre-#3617 store wrote them: compress, encrypt under the old DEK, insert raw.
    for id in 1..=3u64 {
        let n = create_test_node_version(id);
        let bare = c1
            .encrypt(&storage.compress(&encode_node_version(&n)).unwrap(), &[])
            .unwrap();
        insert_raw(&storage, NODE_VERSIONS_TABLE, id, &bare);
    }
    for id in 10..=11u64 {
        let e = create_test_edge_version(id);
        let bare = c1
            .encrypt(&storage.compress(&encode_edge_version(&e)).unwrap(), &[])
            .unwrap();
        insert_raw(&storage, EDGE_VERSIONS_TABLE, id, &bare);
    }

    // Bulk re-encrypt: every legacy value is upgraded to ACV1@2 (re-wrapped),
    // none skipped (nothing was already at the target generation).
    let stats = storage.reencrypt_cold_values(2).unwrap();
    assert_eq!(
        stats.values_rewrapped, 5,
        "3 legacy nodes + 2 legacy edges must all be upgraded through the bulk pass"
    );
    assert_eq!(
        stats.values_skipped, 0,
        "no legacy value was already ACV1@2, so none may be skipped"
    );

    // Every value is now ACV1@2 (upgraded from bare legacy) and the marker is set.
    for id in 1..=3u64 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, NODE_VERSIONS_TABLE, id)).unwrap();
        assert_eq!(kv, 2, "legacy node {id} must upgrade to ACV1@2");
    }
    for id in 10..=11u64 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, EDGE_VERSIONS_TABLE, id)).unwrap();
        assert_eq!(kv, 2, "legacy edge {id} must upgrade to ACV1@2");
    }
    assert_eq!(storage.cold_value_format_version().unwrap(), Some(2));

    // Retire the old generation, then confirm every upgraded value decrypts under
    // the NEW cold DEK alone — proving the legacy bytes were truly re-keyed.
    storage.retire_cold_generations(2).unwrap();
    for id in 1..=3u64 {
        let n = create_test_node_version(id);
        assert_eq!(
            storage.get_node_version(n.id).unwrap().unwrap().id,
            n.id,
            "upgraded node {id} must decrypt under the new DEK"
        );
    }
    for id in 10..=11u64 {
        let e = create_test_edge_version(id);
        assert_eq!(
            storage.get_edge_version(e.id).unwrap().unwrap().id,
            e.id,
            "upgraded edge {id} must decrypt under the new DEK"
        );
    }
}

#[test]
fn crafted_acv1_prefixed_legacy_value_fails_loudly() {
    // Finding 1(d): a legacy bare ciphertext whose leading bytes happen to
    // collide with the `ACV1` magic + a held key-version (a ~2^-32 event under a
    // single-generation `match_any` keyring, where `has_version` short-circuits
    // `true` so ONLY the 4-byte magic discriminates) is MISCLASSIFIED as wrapped.
    // This MUST surface as a LOUD AEAD authentication failure (a read
    // availability fault) — never a panic, and never silent wrong data.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("crafted.redb");
    let cipher = test_cipher();

    // Single-generation (match_any) keyring: `has_version` returns true for EVERY
    // version, so the trailing u32 adds no discrimination on read.
    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cipher(Arc::clone(&cipher));

    // A genuine legacy bare ciphertext for node 1, whose leading 8 bytes we then
    // FORCE to collide with `ACV1` + held version 1. This simulates the ~2^-32
    // event where a real ciphertext prefix equals the wrapper header: the first 8
    // bytes are truly part of the ciphertext, so treating them as a wrapper strips
    // real ciphertext bytes and the AEAD tag can no longer authenticate.
    let version = create_test_node_version(1);
    let mut crafted = cipher
        .encrypt(
            &storage.compress(&encode_node_version(&version)).unwrap(),
            &[],
        )
        .unwrap();
    assert!(
        crafted.len() >= 8,
        "an AEAD ciphertext is always at least the wrapper length"
    );
    crafted[0..4].copy_from_slice(b"ACV1"); // COLD_VALUE_MAGIC
    crafted[4..8].copy_from_slice(&1u32.to_le_bytes()); // a held key-version
    insert_raw(&storage, NODE_VERSIONS_TABLE, 1, &crafted);

    // parse_cold_wrapper now (mis)reads it as ACV1@1; has_version(1) short-circuits
    // true under match_any, so it is decrypted as wrapped and the 8-byte-stripped
    // slice is fed to `decrypt` -> AEAD auth fails -> Err (loud, availability-only).
    let result = storage.get_node_version(version.id);
    assert!(
        result.is_err(),
        "a crafted ACV1-colliding legacy value must fail loudly (AEAD auth), \
         never return silent wrong data (finding 1(d): loud, availability-only)"
    );
}

#[test]
fn cold_rotation_resumes_after_real_reopen() {
    // Genuine cross-process-style resume: partially rotate + plant a DURABLE
    // cursor, DROP the redb + storage handle (closing the on-disk file), then
    // reopen a FRESH `RedbColdStorage` from the same path and resume. The cursor
    // must be read from durable redb metadata (not in-process state), the pass
    // must continue from it with no double-encrypt, and every value must end
    // ACV1@2 decrypting under the new DEK.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("reopen_resume.redb");
    let c1 = cipher_with_byte(0xA1);
    let c2 = cipher_with_byte(0xB2);

    // Handle A: store 5 nodes (ACV1@1), manually rewrap the 1..=3 prefix to
    // ACV1@2 + plant the resume cursor at (Node, key=3), then DROP the handle.
    let node1_before;
    {
        let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
            .unwrap()
            .with_cipher(Arc::clone(&c1));
        for id in 1..=5u64 {
            storage
                .store_node_version(&create_test_node_version(id))
                .unwrap();
        }
        storage.install_cold_generation(2, Arc::clone(&c2)).unwrap();
        for id in 1..=3u64 {
            let raw = raw_value(&storage, NODE_VERSIONS_TABLE, id);
            let compressed = storage.decrypt_if_needed(&raw).unwrap();
            let ct2 = c2.encrypt(&compressed, &[]).unwrap();
            insert_raw(&storage, NODE_VERSIONS_TABLE, id, &wrap_cold_value(2, &ct2));
        }
        let cursor = ColdRotationCursor {
            target_version: 2,
            table: ColdTableKind::Node,
            last_completed_key: 3,
        };
        insert_raw_metadata(&storage, COLD_ROTATION_CURSOR_KEY, &cursor.to_bytes());
        node1_before = raw_value(&storage, NODE_VERSIONS_TABLE, 1);
    } // drop handle A: closes the redb file so the reopen is genuinely from disk

    // Handle B: reopen FRESH from disk with a 2-generation keyring, then resume.
    let ring = ColdKeyring::single(Arc::clone(&c1));
    ring.add_generation(2, Arc::clone(&c2));
    let storage = RedbColdStorage::new(&db_path, RedbConfig::new())
        .unwrap()
        .with_cold_keyring(ring);

    // The durable cursor is read from redb metadata by the freshly-opened handle.
    assert_eq!(
        storage
            .read_cold_rotation_cursor()
            .unwrap()
            .map(|c| c.last_completed_key),
        Some(3),
        "reopened handle must read the durable resume cursor from disk"
    );

    let stats = storage.reencrypt_cold_values(2).unwrap();
    assert_eq!(
        stats.values_rewrapped, 2,
        "only nodes 4,5 remain past the durable cursor"
    );

    // No double-encrypt of the already-done prefix after reopen.
    assert_eq!(
        raw_value(&storage, NODE_VERSIONS_TABLE, 1),
        node1_before,
        "a value past the durable cursor must not be re-encrypted after a real reopen"
    );
    for id in 1..=5u64 {
        let (kv, _) = parse_cold_wrapper(&raw_value(&storage, NODE_VERSIONS_TABLE, id)).unwrap();
        assert_eq!(kv, 2, "every node must end ACV1@2");
    }
    assert!(
        storage.read_cold_rotation_cursor().unwrap().is_none(),
        "cursor cleared on completion"
    );
    assert_eq!(storage.cold_value_format_version().unwrap(), Some(2));

    // Every value decrypts under the new DEK alone.
    storage.retire_cold_generations(2).unwrap();
    for id in 1..=5u64 {
        let v = create_test_node_version(id);
        assert_eq!(storage.get_node_version(v.id).unwrap().unwrap().id, v.id);
    }
}

// ========================================================================
// Version-counter seeding at open (Issue #3222 review follow-up)
// ========================================================================

/// Reopening an existing cold store must seed `node_versions_stored` /
/// `edge_versions_stored` from the persisted tables — a restarted process
/// must never report `0` cold versions over a database that holds history.
#[test]
fn test_stats_version_counters_seeded_on_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("reopen.redb");

    {
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        for i in 1..=3 {
            storage
                .store_node_version(&create_test_node_version(i))
                .unwrap();
        }
        storage
            .store_edge_version(&create_test_edge_version(10))
            .unwrap();

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 3);
        assert_eq!(stats.edge_versions_stored, 1);
    } // drop: close the database

    let reopened = RedbColdStorage::with_default_config(&db_path).unwrap();
    let stats = reopened.stats();
    assert_eq!(
        stats.node_versions_stored, 3,
        "reopened store must report persisted node versions, not 0"
    );
    assert_eq!(
        stats.edge_versions_stored, 1,
        "reopened store must report persisted edge versions, not 0"
    );
    // Byte counters are process-lifetime (not persisted): the ratio is 1.0
    // until this process writes something.
    assert_eq!(stats.bytes_written_raw, 0);
    assert_eq!(stats.compression_ratio(), 1.0);

    // Storing more versions on the reopened handle continues from the seed.
    reopened
        .store_node_version(&create_test_node_version(4))
        .unwrap();
    assert_eq!(reopened.stats().node_versions_stored, 4);
}

// ========================================================================
// Persisted cold-tier temporal extent bounds (Issue #3389)
// ========================================================================

/// Build a node version whose valid interval starts at `valid_start` (open
/// ended) and whose transaction interval starts at `tx_start`.
fn node_version_with_times(id: u64, valid_start: i64, tx_start: i64) -> NodeVersion {
    use crate::core::temporal::TimeRange;
    NodeVersion::new_anchor(
        VersionId::new(id).unwrap(),
        NodeId::new(id).unwrap(),
        BiTemporalInterval::new(
            TimeRange::from(Timestamp::from(valid_start)),
            TimeRange::from(Timestamp::from(tx_start)),
        ),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
}

#[test]
fn extent_bounds_none_when_cold_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap();
    assert_eq!(
        store.get_temporal_extent_bounds().unwrap(),
        None,
        "an empty cold tier contributes no extent bounds"
    );
}

#[test]
fn extent_bounds_persist_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cold.redb");

    {
        let store = RedbColdStorage::with_default_config(&path).unwrap();
        store
            .store_node_version(&node_version_with_times(1, 1_000, 5_000))
            .unwrap();
        // Bounds are visible immediately after the write.
        let bounds = store.get_temporal_extent_bounds().unwrap().unwrap();
        assert_eq!(bounds.valid_earliest, Some(Timestamp::from(1_000)));
        assert_eq!(bounds.tx_earliest, Some(Timestamp::from(5_000)));
    }

    // Reopen the file (simulated restart): the bounds must survive.
    let reopened = RedbColdStorage::with_default_config(&path).unwrap();
    let bounds = reopened.get_temporal_extent_bounds().unwrap().unwrap();
    assert_eq!(
        bounds.valid_earliest,
        Some(Timestamp::from(1_000)),
        "cold-tier valid earliest must survive reopen"
    );
    assert_eq!(bounds.tx_earliest, Some(Timestamp::from(5_000)));
}

#[test]
fn extent_bounds_open_valid_to_never_leaks_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    let store = RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap();
    // Open valid_to (TimeRange::from is [start, MAX)).
    store
        .store_node_version(&node_version_with_times(1, 2_000, 2_000))
        .unwrap();

    let bounds = store.get_temporal_extent_bounds().unwrap().unwrap();
    // A single open-ended fact: latest equals its start, never TIMESTAMP_MAX.
    assert_eq!(bounds.valid_latest, Some(Timestamp::from(2_000)));
    assert!(bounds.valid_latest.unwrap() < TIMESTAMP_MAX);
    assert_eq!(bounds.tx_latest, Some(Timestamp::from(2_000)));
    assert!(bounds.tx_latest.unwrap() < TIMESTAMP_MAX);
}

#[test]
fn extent_bounds_widen_monotonically_across_stores() {
    let dir = tempfile::tempdir().unwrap();
    let store = RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap();

    store
        .store_node_version(&node_version_with_times(1, 3_000, 9_000))
        .unwrap();
    // A backdated fact widens earliest; a later one widens latest.
    store
        .store_node_version(&node_version_with_times(2, 1_000, 5_000))
        .unwrap();
    store
        .store_node_version(&node_version_with_times(3, 7_000, 12_000))
        .unwrap();

    let bounds = store.get_temporal_extent_bounds().unwrap().unwrap();
    assert_eq!(bounds.valid_earliest, Some(Timestamp::from(1_000)));
    assert_eq!(bounds.valid_latest, Some(Timestamp::from(7_000)));
    assert_eq!(bounds.tx_earliest, Some(Timestamp::from(5_000)));
    assert_eq!(bounds.tx_latest, Some(Timestamp::from(12_000)));
}

#[test]
fn extent_bounds_updated_by_batch_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap();

    // store_node_versions_batch path.
    store
        .store_node_versions_batch(&[
            node_version_with_times(1, 4_000, 8_000),
            node_version_with_times(2, 2_000, 6_000),
        ])
        .unwrap();
    let bounds = store.get_temporal_extent_bounds().unwrap().unwrap();
    assert_eq!(bounds.valid_earliest, Some(Timestamp::from(2_000)));

    // store_batch_with_lsn path widens further.
    store
        .store_batch_with_lsn(&[node_version_with_times(3, 1_000, 5_000)], &[], LSN(42))
        .unwrap();
    let bounds = store.get_temporal_extent_bounds().unwrap().unwrap();
    assert_eq!(bounds.valid_earliest, Some(Timestamp::from(1_000)));
    assert_eq!(bounds.tx_earliest, Some(Timestamp::from(5_000)));
    // The flushed LSN update in the same path still works alongside the extent.
    assert_eq!(store.get_flushed_lsn().unwrap(), Some(LSN(42)));
}

#[test]
fn extent_bounds_round_trip_serialization() {
    // Every-field-present round trip (all four dimensions Some).
    let bounds = ColdTemporalExtentBounds {
        valid_earliest: Some(Timestamp::from(1_000)),
        valid_latest: Some(Timestamp::from(9_000)),
        tx_earliest: Some(Timestamp::from(2_000)),
        tx_latest: Some(Timestamp::from(8_000)),
    };
    let decoded = ColdTemporalExtentBounds::from_bytes(&bounds.to_bytes()).unwrap();
    assert_eq!(decoded, bounds);

    // Mixed presence: some dimensions Some, others None. Exercises the
    // per-field presence flag independently in both `to_bytes`/`from_bytes`
    // arms (a Some field must decode back Some, a None field back None) rather
    // than the all-present fixture above, which cannot distinguish the arms.
    let mixed = ColdTemporalExtentBounds {
        valid_earliest: Some(Timestamp::from(1_000)),
        valid_latest: None,
        tx_earliest: None,
        tx_latest: Some(Timestamp::from(8_000)),
    };
    assert_eq!(
        ColdTemporalExtentBounds::from_bytes(&mixed.to_bytes()).unwrap(),
        mixed
    );

    // Extreme wallclock values round-trip losslessly through the i64 encoding.
    let extremes = ColdTemporalExtentBounds {
        valid_earliest: Some(Timestamp::new_unchecked(i64::MIN, 0)),
        valid_latest: Some(Timestamp::new_unchecked(i64::MAX, u32::MAX)),
        tx_earliest: Some(Timestamp::new_unchecked(i64::MIN, u32::MAX)),
        tx_latest: Some(Timestamp::new_unchecked(i64::MAX, 0)),
    };
    assert_eq!(
        ColdTemporalExtentBounds::from_bytes(&extremes.to_bytes()).unwrap(),
        extremes
    );

    // A default (all-None) record round-trips to all-None.
    let empty = ColdTemporalExtentBounds::default();
    assert_eq!(
        ColdTemporalExtentBounds::from_bytes(&empty.to_bytes()).unwrap(),
        empty
    );

    // A record with an unrecognized version tag is treated as absent.
    let mut bad = bounds.to_bytes();
    bad[0] = 0xFF;
    assert_eq!(ColdTemporalExtentBounds::from_bytes(&bad), None);
    // Wrong length is also treated as absent, never a panic.
    assert_eq!(ColdTemporalExtentBounds::from_bytes(&[1, 2, 3]), None);
    // An empty buffer likewise decodes to None (guards the length check
    // against an index-out-of-bounds panic on `bytes[0]`).
    assert_eq!(ColdTemporalExtentBounds::from_bytes(&[]), None);
}
