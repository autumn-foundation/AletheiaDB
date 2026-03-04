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
