#![cfg(test)]

//! Tests for version chain reconstruction with tiered storage.
//!
//! These tests verify that HistoricalStorage can reconstruct properties
//! from version chains even when some versions have been migrated to cold storage.

use gallifreydb::PropertyMapBuilder;
use gallifreydb::core::GLOBAL_INTERNER;
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::temporal::time;
use gallifreydb::storage::historical::HistoricalStorage;
use gallifreydb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
use gallifreydb::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
use std::sync::Arc;
use tempfile::TempDir;

/// Test that version chain reconstruction works when middle versions are in cold storage.
///
/// This is the core issue: when versions in the middle of a chain are migrated to cold,
/// reconstruct_node_properties should still be able to walk the chain and reconstruct properties.
///
/// Setup:
/// 1. Create a node with 5 versions (v1 -> v2 -> v3 -> v4 -> v5)
/// 2. Migrate v2, v3, v4 to cold storage (keep v1 and v5 in hot)
/// 3. Attempt to reconstruct properties for v5
///
/// Expected: Should successfully reconstruct by walking chain through cold storage
/// Actual (before fix): Fails with "Delta version has no previous version"
#[test]
fn test_reconstruct_properties_with_cold_versions_in_chain() {
    let temp_dir = TempDir::new().unwrap();
    let cold_path = temp_dir.path().join("cold.redb");

    // Create cold storage and tiered storage
    let cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
    let tiered_config = TieredStorageConfig::default();
    let tiered = Arc::new(TieredStorage::new(tiered_config, Box::new(cold)));

    // Create historical storage with tiered storage enabled
    let mut historical = HistoricalStorage::new();
    historical.set_tiered_storage(tiered.clone());

    let node_id = NodeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();

    // Create version 1 (anchor) - name: "Alice"
    let v1_id = VersionId::new(1).unwrap();
    let props_v1 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    historical
        .add_node_version(
            node_id,
            v1_id,
            time::now(),
            time::now(),
            label,
            props_v1.clone(),
            false, // not a tombstone
        )
        .unwrap();

    // Create version 2 (delta) - name: "Alice", age: 31
    let v2_id = VersionId::new(2).unwrap();
    let props_v2 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 31i64)
        .build();
    historical
        .add_node_version(
            node_id,
            v2_id,
            time::now(),
            time::now(),
            label,
            props_v2.clone(),
            false, // not a tombstone
        )
        .unwrap();

    // Create version 3 (delta) - name: "Alice Smith", age: 31
    let v3_id = VersionId::new(3).unwrap();
    let props_v3 = PropertyMapBuilder::new()
        .insert("name", "Alice Smith")
        .insert("age", 31i64)
        .build();
    historical
        .add_node_version(
            node_id,
            v3_id,
            time::now(),
            time::now(),
            label,
            props_v3.clone(),
            false, // not a tombstone
        )
        .unwrap();

    // Create version 4 (delta) - name: "Alice Smith", age: 32
    let v4_id = VersionId::new(4).unwrap();
    let props_v4 = PropertyMapBuilder::new()
        .insert("name", "Alice Smith")
        .insert("age", 32i64)
        .build();
    historical
        .add_node_version(
            node_id,
            v4_id,
            time::now(),
            time::now(),
            label,
            props_v4.clone(),
            false, // not a tombstone
        )
        .unwrap();

    // Create version 5 (delta) - name: "Alice Smith-Jones", age: 32
    let v5_id = VersionId::new(5).unwrap();
    let props_v5 = PropertyMapBuilder::new()
        .insert("name", "Alice Smith-Jones")
        .insert("age", 32i64)
        .build();
    historical
        .add_node_version(
            node_id,
            v5_id,
            time::now(),
            time::now(),
            label,
            props_v5.clone(),
            false, // not a tombstone
        )
        .unwrap();

    // Skip the before-migration reconstruction to avoid populating the cache
    // (The cache would make the test pass incorrectly)

    // Now migrate v2, v3, v4 to cold storage
    // In a real scenario, this would be done by MigrationService,
    // but we'll manually move them to cold storage and remove from hot
    let v2 = historical.get_node_version(v2_id).unwrap().clone();
    let v3 = historical.get_node_version(v3_id).unwrap().clone();
    let v4 = historical.get_node_version(v4_id).unwrap().clone();

    // Store in cold storage
    tiered.cold_storage().store_node_version(&v2).unwrap();
    tiered.cold_storage().store_node_version(&v3).unwrap();
    tiered.cold_storage().store_node_version(&v4).unwrap();

    // Remove from hot storage to simulate migration
    println!(
        "Before removal: v4 in hot = {}",
        historical.get_node_version(v4_id).is_some()
    );
    historical.__test_remove_node_version(v2_id);
    historical.__test_remove_node_version(v3_id);
    historical.__test_remove_node_version(v4_id);
    println!(
        "After removal: v4 in hot = {}",
        historical.get_node_version(v4_id).is_some()
    );

    // Verify they're not in hot storage
    assert!(
        historical.get_node_version(v2_id).is_none(),
        "v2 should not be in hot storage"
    );
    assert!(
        historical.get_node_version(v3_id).is_none(),
        "v3 should not be in hot storage"
    );
    assert!(
        historical.get_node_version(v4_id).is_none(),
        "v4 should not be in hot storage"
    );

    // Verify they're in cold storage via tiered access
    assert!(historical.get_node_version_tiered(v2_id).unwrap().is_some());
    assert!(historical.get_node_version_tiered(v3_id).unwrap().is_some());
    assert!(historical.get_node_version_tiered(v4_id).unwrap().is_some());

    // Clear the property cache to force actual reconstruction (not using cached values)
    println!("Clearing property cache to force chain traversal");
    historical.__test_clear_property_cache();

    // THIS IS THE KEY TEST: Reconstruct v5 properties with cold versions in chain
    // Should walk: v5 (hot) -> v4 (cold) -> v3 (cold) -> v2 (cold) -> v1 (hot, anchor)
    // The bug: reconstruct_node_properties only looks in hot storage when walking the chain
    println!("About to reconstruct v5 - this SHOULD fail because v4 is not in hot storage");

    let reconstructed = historical.reconstruct_node_properties(v5_id);

    // GREEN PHASE: After the fix, reconstruction should succeed!
    let reconstructed = reconstructed.unwrap();
    assert_eq!(
        reconstructed.get("name").unwrap().as_str().unwrap(),
        "Alice Smith-Jones",
        "Name should be reconstructed correctly from version chain spanning cold storage"
    );
    assert_eq!(
        reconstructed.get("age").unwrap().as_int().unwrap(),
        32,
        "Age should be reconstructed correctly from version chain spanning cold storage"
    );
}

/// Test edge case: reconstruct when the requested version itself is in cold storage.
#[test]
fn test_reconstruct_properties_for_version_in_cold_storage() {
    let temp_dir = TempDir::new().unwrap();
    let cold_path = temp_dir.path().join("cold.redb");

    let cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
    let tiered = Arc::new(TieredStorage::new(
        TieredStorageConfig::default(),
        Box::new(cold),
    ));

    let mut historical = HistoricalStorage::new();
    historical.set_tiered_storage(tiered.clone());

    let node_id = NodeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();

    // Create anchor
    let v1_id = VersionId::new(1).unwrap();
    let props_v1 = PropertyMapBuilder::new().insert("name", "Bob").build();
    historical
        .add_node_version(
            node_id,
            v1_id,
            time::now(),
            time::now(),
            label,
            props_v1.clone(),
            false, // not a tombstone
        )
        .unwrap();

    // Create delta
    let v2_id = VersionId::new(2).unwrap();
    let props_v2 = PropertyMapBuilder::new().insert("name", "Robert").build();
    historical
        .add_node_version(
            node_id,
            v2_id,
            time::now(),
            time::now(),
            label,
            props_v2.clone(),
            false, // not a tombstone
        )
        .unwrap();

    // Migrate v2 itself to cold storage
    let v2 = historical.get_node_version(v2_id).unwrap().clone();
    tiered.cold_storage().store_node_version(&v2).unwrap();
    historical.__test_remove_node_version(v2_id);

    // Reconstruct v2 (which is now in cold storage)
    let reconstructed = historical.reconstruct_node_properties(v2_id).unwrap();
    assert_eq!(
        reconstructed.get("name").unwrap().as_str().unwrap(),
        "Robert"
    );
}
