use super::*;
use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
use tempfile::tempdir;
use std::sync::Arc;

#[test]
fn test_cold_storage_accessor_coverage() {
    let mut historical = HistoricalStorage::new();

    // Initially no cold storage
    assert!(!historical.has_cold_storage());
    assert!(historical.cold_storage().is_none());

    // Set cold storage
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());

    historical.set_cold_storage(cold.clone());

    // Verify it's set
    assert!(historical.has_cold_storage());
    assert!(historical.cold_storage().is_some());
}

#[test]
fn test_get_version_tiered_fallback_coverage() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());

    let mut historical = HistoricalStorage::new();
    historical.set_cold_storage(cold.clone());

    let node_id = NodeId::new(1).unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let node_ver_id = VersionId::new(100).unwrap();
    let edge_ver_id = VersionId::new(200).unwrap();
    let label = GLOBAL_INTERNER.intern("Test").unwrap();

    // Create versions directly in cold storage (bypassing hot)
    let node_ver = NodeVersion::new_anchor(
        node_ver_id,
        node_id,
        BiTemporalInterval::current(1000.into()),
        label,
        PropertyMap::new(),
    );
    cold.store_node_version(&node_ver).unwrap();

    let edge_ver = EdgeVersion::new_anchor(
        edge_ver_id,
        edge_id,
        BiTemporalInterval::current(1000.into()),
        label,
        NodeId::new(2).unwrap(),
        NodeId::new(3).unwrap(),
        PropertyMap::new(),
    );
    cold.store_edge_version(&edge_ver).unwrap();

    // Verify get_node_version_tiered falls back to cold
    let retrieved_node = historical.get_node_version_tiered(node_ver_id).unwrap();
    assert!(retrieved_node.is_some());
    assert_eq!(retrieved_node.unwrap().id, node_ver_id);

    // Verify get_edge_version_tiered falls back to cold
    let retrieved_edge = historical.get_edge_version_tiered(edge_ver_id).unwrap();
    assert!(retrieved_edge.is_some());
    assert_eq!(retrieved_edge.unwrap().id, edge_ver_id);

    // Verify missing version returns None
    assert!(historical.get_node_version_tiered(VersionId::new(999).unwrap()).unwrap().is_none());
    assert!(historical.get_edge_version_tiered(VersionId::new(999).unwrap()).unwrap().is_none());
}

#[test]
fn test_migrate_to_cold_without_storage_coverage() {
    let mut historical = HistoricalStorage::new();

    // Should return 0 immediately because cold storage is not configured
    // This covers the early return at the start of migrate_to_cold
    let policy = crate::storage::migration::MigrationPolicy::default();
    // We need a dummy cold storage to create the service, even if historical doesn't use it
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("dummy.redb");
    let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());

    let service = crate::storage::migration::MigrationService::new(cold, policy);

    let result = historical.migrate_to_cold(&service).unwrap();
    assert_eq!(result, 0);
}
