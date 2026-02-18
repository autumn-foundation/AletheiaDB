#![cfg(test)]

//! Elenchus ⚔️ - Tiered Storage Audits
//!
//! These tests verify the correctness and performance characteristics of the tiered storage implementation,
//! specifically focusing on cold path reconstruction, cache warming behavior, and edge cases.

use aletheiadb::PropertyMapBuilder;
use aletheiadb::config::HistoricalConfigBuilder;
use aletheiadb::core::GLOBAL_INTERNER;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::temporal::time;
use aletheiadb::storage::historical::HistoricalStorage;
use aletheiadb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
use aletheiadb::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
use std::sync::Arc;
use tempfile::TempDir;

/// Verify that property reconstruction works correctly when intermediate versions are in cold storage,
/// WITHOUT accidentally pre-warming the cache via assertions.
///
/// This test proves that the system can actually pull from disk during reconstruction, not just from memory.
#[test]
fn test_cold_reconstruction_without_warm_cache() {
    let temp_dir = TempDir::new().unwrap();
    let cold_path = temp_dir.path().join("cold.redb");

    // Create cold storage and tiered storage
    // Disable prefetching to simplify metric analysis (ensure exactly 1 cold hit per cold version)
    let tiered_config = TieredStorageConfig {
        warm_cache_size: 100,
        enable_prefetch: false,
        prefetch_depth: 0,
    };
    let cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
    let tiered = Arc::new(TieredStorage::new(tiered_config, Arc::new(cold)));

    // Create historical storage with tiered storage enabled
    let mut historical = HistoricalStorage::new();
    historical.set_tiered_storage(tiered.clone());

    let node_id = NodeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("TestNode").unwrap();

    // Create version 1 (anchor) - "step": 1 (Hot)
    let v1_id = VersionId::new(1).unwrap();
    let props_v1 = PropertyMapBuilder::new().insert("step", 1i64).build();
    historical
        .add_node_version(
            node_id,
            v1_id,
            time::now(),
            time::now(),
            label,
            props_v1.clone(),
            false,
        )
        .unwrap();

    // Create version 2 (delta) - "step": 2 (will be Cold)
    let v2_id = VersionId::new(2).unwrap();
    let props_v2 = PropertyMapBuilder::new().insert("step", 2i64).build();
    historical
        .add_node_version(
            node_id,
            v2_id,
            time::now(),
            time::now(),
            label,
            props_v2.clone(),
            false,
        )
        .unwrap();

    // Create version 3 (delta) - "step": 3 (will be Cold)
    let v3_id = VersionId::new(3).unwrap();
    let props_v3 = PropertyMapBuilder::new().insert("step", 3i64).build();
    historical
        .add_node_version(
            node_id,
            v3_id,
            time::now(),
            time::now(),
            label,
            props_v3.clone(),
            false,
        )
        .unwrap();

    // Create version 4 (delta) - "step": 4 (Hot)
    let v4_id = VersionId::new(4).unwrap();
    let props_v4 = PropertyMapBuilder::new().insert("step", 4i64).build();
    historical
        .add_node_version(
            node_id,
            v4_id,
            time::now(),
            time::now(),
            label,
            props_v4.clone(),
            false,
        )
        .unwrap();

    // Migrate v2 and v3 to cold storage
    let v2 = historical.get_node_version(v2_id).unwrap().clone();
    let v3 = historical.get_node_version(v3_id).unwrap().clone();

    tiered.cold_storage().store_node_version(&v2).unwrap();
    tiered.cold_storage().store_node_version(&v3).unwrap();

    historical.__test_remove_node_version(v2_id);
    historical.__test_remove_node_version(v3_id);

    // Verify state: v2, v3 are gone from hot
    assert!(historical.get_node_version(v2_id).is_none());
    assert!(historical.get_node_version(v3_id).is_none());

    // Force clear property cache in HistoricalStorage to ensure reconstruction happens
    historical.__test_clear_property_cache();

    // Record metrics before
    let metrics_before = tiered.metrics();

    // Reconstruct v4 properties
    // Path: v4(Hot) -> v3(Cold) -> v2(Cold) -> v1(Hot)
    let reconstructed = historical.reconstruct_node_properties(v4_id).unwrap();

    // Assert correctness
    assert_eq!(reconstructed.get("step").unwrap().as_int().unwrap(), 4);

    // Verify metrics
    let metrics_after = tiered.metrics();
    let cold_hits = metrics_after.cold_hits - metrics_before.cold_hits;
    let warm_hits = metrics_after.warm_hits - metrics_before.warm_hits;

    // ELENCHUS FINDING: Double-Fetch Behavior
    // `reconstruct_node_properties_iterative` fetches versions TWICE:
    // 1. Backward walk (collect IDs) -> Triggers Cold Hit -> Populates Warm Cache
    // 2. Forward walk (apply deltas) -> Triggers Warm Hit

    // Expected Cold Hits: 2 (v3, v2 accessed during backward walk)
    assert_eq!(cold_hits, 2, "Expected exactly 2 cold hits for v3 and v2");

    // Expected Warm Hits: 2 (v3, v2 accessed during forward walk)
    // The previous assertion expected 0, failing to account for the second pass.
    assert_eq!(
        warm_hits, 2,
        "Expected 2 warm hits due to double-fetch implementation"
    );
}

/// Verify that prefetching actually works during reconstruction.
#[test]
fn test_prefetch_effectiveness_during_reconstruction() {
    let temp_dir = TempDir::new().unwrap();
    let cold_path = temp_dir.path().join("cold_prefetch.redb");

    // Enable prefetching
    let tiered_config = TieredStorageConfig {
        warm_cache_size: 100,
        enable_prefetch: true,
        prefetch_depth: 3,
    };
    let cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
    let tiered = Arc::new(TieredStorage::new(tiered_config, Arc::new(cold)));

    let mut historical = HistoricalStorage::new();
    historical.set_tiered_storage(tiered.clone());

    let node_id = NodeId::new(10).unwrap();
    let label = GLOBAL_INTERNER.intern("PrefetchTest").unwrap();

    // Create a chain of 5 versions (all cold)
    // v1 -> v2 -> v3 -> v4 -> v5
    let mut versions = Vec::new();

    // v1 (Anchor)
    let v1_id = VersionId::new(1).unwrap();
    let props_v1 = PropertyMapBuilder::new().insert("val", 1i64).build();
    historical
        .add_node_version(
            node_id,
            v1_id,
            time::now(),
            time::now(),
            label,
            props_v1.clone(),
            false,
        )
        .unwrap();
    versions.push(v1_id);

    // v2..v5 (Deltas)
    for i in 2..=5 {
        let vid = VersionId::new(i as u64).unwrap();
        let props = PropertyMapBuilder::new().insert("val", i as i64).build();
        historical
            .add_node_version(
                node_id,
                vid,
                time::now(),
                time::now(),
                label,
                props.clone(),
                false,
            )
            .unwrap();
        versions.push(vid);
    }

    // Migrate ALL to cold storage
    for vid in &versions {
        let v = historical.get_node_version(*vid).unwrap().clone();
        tiered.cold_storage().store_node_version(&v).unwrap();
        historical.__test_remove_node_version(*vid);
    }

    historical.__test_clear_property_cache();

    // Reconstruct v5
    // Chain: v5(Cold) -> v4(Cold) -> v3(Cold) -> v2(Cold) -> v1(Cold)

    let metrics_before = tiered.metrics();
    let _reconstructed = historical
        .reconstruct_node_properties(VersionId::new(5).unwrap())
        .unwrap();
    let metrics_after = tiered.metrics();

    let cold_hits = metrics_after.cold_hits - metrics_before.cold_hits;
    let warm_hits = metrics_after.warm_hits - metrics_before.warm_hits;
    let prefetches = metrics_after.prefetches - metrics_before.prefetches;

    // ELENCHUS FINDING:
    // Cold Hits: 2 (v5 initial, v1 fell off prefetch)
    // Warm Hits: 8 (4 from double-fetch forward pass + 3 from backward pass hits + 1 anchor backward hit?)
    // Actually, backward pass:
    // v5 (Cold) -> Prefetch v4,v3,v2.
    // v4 (Warm) -> Stop prefetch.
    // v3 (Warm).
    // v2 (Warm).
    // v1 (Cold).
    // Total Backward: 2 Cold, 3 Warm.
    // Forward pass: v1(Warm), v2(Warm), v3(Warm), v4(Warm), v5(Warm).
    // Total Forward: 5 Warm.
    // Grand Total: 2 Cold, 8 Warm.

    assert_eq!(cold_hits, 2, "Expected 2 cold hits (v5, v1)");
    assert_eq!(
        warm_hits, 8,
        "Expected 8 warm hits (3 backward + 5 forward)"
    );
    assert_eq!(
        prefetches, 3,
        "Expected 3 prefetches triggered by v5 access"
    );
}

/// Verify that reconstruction fails gracefully when MaxDepth is exceeded,
/// even with tiered storage involved.
#[test]
fn test_reconstruction_max_depth_exceeded_forced() {
    let temp_dir = TempDir::new().unwrap();
    let cold_path = temp_dir.path().join("cold_depth_forced.redb");

    let cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
    let tiered = Arc::new(TieredStorage::new(
        TieredStorageConfig::default(),
        Arc::new(cold),
    ));

    let config = HistoricalConfigBuilder::new()
        .max_reconstruction_depth(5)
        .unwrap()
        .anchor_interval(100)
        .unwrap() // Large interval to force long delta chains
        .build();

    let mut historical = HistoricalStorage::from_unified_config(config);
    historical.set_tiered_storage(tiered.clone());

    let node_id = NodeId::new(200).unwrap();
    let label = GLOBAL_INTERNER.intern("DepthForce").unwrap();

    // Create 10 versions (v1 anchor, v2..v10 deltas)
    // Chain length for v10 is 9 (v10->v9->...->v1)

    // Create ALL versions in hot storage FIRST
    for i in 1..=10 {
        let vid = VersionId::new(i as u64).unwrap();
        let props = PropertyMapBuilder::new().insert("val", i as i64).build();
        historical
            .add_node_version(
                node_id,
                vid,
                time::now(),
                time::now(),
                label,
                props.clone(),
                false,
            )
            .unwrap();
    }

    // Then migrate evens to cold storage
    for i in 2..=10 {
        if i % 2 == 0 {
            let vid = VersionId::new(i as u64).unwrap();
            let v = historical.get_node_version(vid).unwrap().clone();
            tiered.cold_storage().store_node_version(&v).unwrap();
            historical.__test_remove_node_version(vid);
        }
    }

    // Explicitly flush cold storage to ensure persistence
    tiered.flush().unwrap();

    historical.__test_clear_property_cache();

    // Reconstruct v10
    // Depth is ~9. Limit is 5. Should fail with MaxDepthExceeded.
    let result = historical.reconstruct_node_properties(VersionId::new(10).unwrap());

    if let Err(e) = &result {
        println!("Reconstruction failed with: {:?}", e);
    }

    assert!(
        result.is_err(),
        "Reconstruction should fail due to depth limit"
    );
    let err = result.unwrap_err();

    // If we get VersionNotFound, it implies max_reconstruction_depth was ignored
    // and we fell off the cliff looking for a version that should have been skipped.
    // However, allowing VersionNotFound would mask the bug.
    // We strictly expect MaxDepthExceeded.
    let err_str = format!("{:?}", err);
    if err_str.contains("VersionNotFound") {
        panic!(
            "FAILED: MaxDepthExceeded check failed! System attempted to fetch deep version {:?} instead of aborting. This implies max_reconstruction_depth config is ignored.",
            err
        );
    }

    assert!(
        err_str.contains("MaxDepthExceeded"),
        "Expected MaxDepthExceeded, got: {:?}",
        err
    );
}
