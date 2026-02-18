use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::BiTemporalInterval;
use aletheiadb::core::version::NodeVersion;
use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
use aletheiadb::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
use std::sync::Arc;

fn create_version_chain(length: usize) -> Vec<NodeVersion> {
    let mut versions = Vec::new();
    let v1 = {
        let props = PropertyMapBuilder::new().insert("step", 1i64).build();
        NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Test").unwrap(),
            props,
        )
    };
    versions.push(v1);

    for i in 2..=length {
        let old_props = PropertyMapBuilder::new()
            .insert("step", (i - 1) as i64)
            .build();
        let new_props = PropertyMapBuilder::new().insert("step", i as i64).build();
        let v = NodeVersion::new_delta(
            VersionId::new(i as u64).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(((1000 + i * 100) as i64).into()),
            GLOBAL_INTERNER.intern("Test").unwrap(),
            &old_props,
            &new_props,
            VersionId::new((i - 1) as u64).unwrap(),
        );
        versions.push(v);
    }
    versions
}

#[test]
fn test_double_fetch_avoidance() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

    let config = TieredStorageConfig {
        warm_cache_size: 100,
        enable_prefetch: true,
        prefetch_depth: 5,
    };
    let tiered = TieredStorage::new(config, Arc::new(cold));

    // Create a chain v1 <- v2
    let versions = create_version_chain(2);
    for v in &versions {
        tiered.store_node_version(v).unwrap();
    }

    // Access v2 (head). This should prefetch v1.
    let _ = tiered.get_node_version_cold(VersionId::new(2).unwrap()).unwrap();

    let metrics_after_v2 = tiered.metrics();
    assert_eq!(metrics_after_v2.cold_hits, 1, "Should have 1 cold hit for v2");
    assert_eq!(metrics_after_v2.prefetches, 1, "Should have prefetched v1");

    // Access v1. It should be in warm cache because of prefetch.
    let _ = tiered.get_node_version_cold(VersionId::new(1).unwrap()).unwrap();

    let metrics_final = tiered.metrics();
    assert_eq!(metrics_final.cold_hits, 1, "Should NOT have fetched v1 from cold");
    assert_eq!(metrics_final.warm_hits, 1, "Should have found v1 in warm cache");
}

#[test]
fn test_prefetch_recursion_limit() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.redb");
    let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

    let config = TieredStorageConfig {
        warm_cache_size: 100,
        enable_prefetch: true,
        prefetch_depth: 2, // Limit depth to 2
    };
    let tiered = TieredStorage::new(config, Arc::new(cold));

    // Create chain v1..v5
    let versions = create_version_chain(5);
    for v in &versions {
        tiered.store_node_version(v).unwrap();
    }

    // Access v5.
    // Should fetch v5 (cold).
    // Prefetch v4 (depth 1).
    // Prefetch v3 (depth 2).
    // Stop. v2 and v1 should NOT be prefetched.
    let _ = tiered.get_node_version_cold(VersionId::new(5).unwrap()).unwrap();

    let metrics = tiered.metrics();
    assert_eq!(metrics.prefetches, 2, "Should stop at depth 2");

    // Verify v3 is warm
    let _ = tiered.get_node_version_cold(VersionId::new(3).unwrap()).unwrap();
    assert_eq!(tiered.metrics().warm_hits, 1);

    // Verify v2 is cold (was not prefetched)
    let _ = tiered.get_node_version_cold(VersionId::new(2).unwrap()).unwrap();
    // Total cold hits: 1 (v5) + 1 (v2) = 2
    assert_eq!(tiered.metrics().cold_hits, 2);
}
