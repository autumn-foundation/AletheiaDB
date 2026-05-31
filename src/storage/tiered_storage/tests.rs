// use super::*;
    use super::*;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::BiTemporalInterval;
    use crate::storage::redb_cold_storage::RedbColdStorage;

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
        let properties = PropertyMapBuilder::new().insert("weight", 1.0f64).build();

        EdgeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            EdgeId::new(200).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(100).unwrap(),
            NodeId::new(101).unwrap(),
            properties,
        )
    }

    fn create_test_version_chain() -> Vec<NodeVersion> {
        // Create a chain of versions: v5 -> v4 -> v3 -> v2 -> v1
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

        for i in 2..=5 {
            let old_props = PropertyMapBuilder::new()
                .insert("step", (i - 1) as i64)
                .build();
            let new_props = PropertyMapBuilder::new().insert("step", i as i64).build();
            let v = NodeVersion::new_delta(
                VersionId::new(i).unwrap(),
                NodeId::new(100).unwrap(),
                BiTemporalInterval::current(((1000 + i * 100) as i64).into()),
                GLOBAL_INTERNER.intern("Test").unwrap(),
                &old_props,
                &new_props,
                VersionId::new(i - 1).unwrap(),
            );
            versions.push(v);
        }

        versions
    }

    #[test]
    fn test_tiered_storage_cold_lookup() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Store a version in cold storage
        let version = create_test_node_version(1);
        tiered.store_node_version(&version).unwrap();

        // Retrieve from cold (first access)
        let retrieved = tiered.get_node_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 0);
    }

    #[test]
    fn test_tiered_storage_edge_cold_lookup() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Store a version in cold storage
        let version = create_test_edge_version(1);
        tiered.store_edge_version(&version).unwrap();

        // Retrieve from cold (first access)
        let retrieved = tiered.get_edge_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 0);
    }

    #[test]
    fn test_tiered_storage_edge_warm_cache() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        let version = create_test_edge_version(1);
        tiered.store_edge_version(&version).unwrap();

        // First access (cold)
        let _ = tiered.get_edge_version_cold(version.id).unwrap();

        // Second access (warm cache)
        let retrieved = tiered.get_edge_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 1);
    }

    #[test]
    fn test_tiered_storage_warm_cache() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        let version = create_test_node_version(1);
        tiered.store_node_version(&version).unwrap();

        // First access (cold)
        let _ = tiered.get_node_version_cold(version.id).unwrap();

        // Second access (warm cache)
        let retrieved = tiered.get_node_version_cold(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);

        // Check metrics
        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 1);
        assert_eq!(metrics.warm_hits, 1);
    }

    #[test]
    fn test_tiered_storage_miss() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        let result = tiered
            .get_node_version_cold(VersionId::new(999).unwrap())
            .unwrap();
        assert!(result.is_none());

        let metrics = tiered.metrics();
        assert_eq!(metrics.misses, 1);
    }

    #[test]
    fn test_tiered_storage_prefetch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: true,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        // Store a version chain in cold storage
        let versions = create_test_version_chain();
        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        // Access the head version (v5)
        let _ = tiered
            .get_node_version_cold(VersionId::new(5).unwrap())
            .unwrap();

        // Check that prefetching occurred
        let metrics = tiered.metrics();
        assert!(metrics.prefetches > 0);
        assert!(metrics.prefetches <= 3); // Up to prefetch_depth
    }

    #[test]
    fn test_tiered_storage_no_prefetch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: false,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        let versions = create_test_version_chain();
        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        let _ = tiered
            .get_node_version_cold(VersionId::new(5).unwrap())
            .unwrap();

        let metrics = tiered.metrics();
        assert_eq!(metrics.prefetches, 0);
    }

    #[test]
    fn test_tiered_storage_hot_hit_recording() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Record some hot hits
        tiered.record_hot_hit();
        tiered.record_hot_hit();
        tiered.record_hot_hit();

        let metrics = tiered.metrics();
        assert_eq!(metrics.hot_hits, 3);
    }

    #[test]
    fn test_metrics_ratios() {
        let metrics = TieredStorageMetrics {
            hot_hits: 70,
            warm_hits: 20,
            cold_hits: 10,
            misses: 0,
            prefetches: 5,
            cold_latency: LatencyPercentiles::default(),
        };

        assert!((metrics.hot_ratio() - 0.7).abs() < 0.001);
        assert!((metrics.warm_ratio() - 0.666).abs() < 0.01); // 20/(20+10)
        assert!((metrics.cache_hit_ratio() - 0.9).abs() < 0.001); // (70+20)/100
    }

    #[test]
    fn test_batch_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        let versions: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();

        tiered.store_node_versions_batch(&versions).unwrap();

        // Verify all versions are stored
        for v in &versions {
            assert!(tiered.contains_node_version(v.id).unwrap());
        }
    }

    #[test]
    fn test_default_config() {
        let config = TieredStorageConfig::default();
        assert_eq!(config.warm_cache_size, 10_000);
        assert!(config.enable_prefetch);
        assert_eq!(config.prefetch_depth, 5);
    }

    // ========================================================================
    // Latency tracking tests
    // ========================================================================

    #[test]
    fn test_latency_tracker_empty() {
        let tracker = LatencyTracker::new(100);
        let percentiles = tracker.percentiles();

        assert_eq!(percentiles.sample_count, 0);
        assert_eq!(percentiles.p50_us, 0);
        assert_eq!(percentiles.p95_us, 0);
        assert_eq!(percentiles.p99_us, 0);
    }

    #[test]
    fn test_latency_tracker_zero_max_samples() {
        let tracker = LatencyTracker::new(0);
        tracker.record(Duration::from_micros(500));

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 0);
        assert_eq!(percentiles.p50_us, 0);
    }

    #[test]
    fn test_latency_percentiles_even_samples() {
        // Verify behavior for even number of samples (e.g. 4)
        // Values: 10, 20, 30, 40
        // Median (p50):
        // Index = 4 * 0.5 = 2.0 -> index 2 -> value 30.
        // Usually median is (20+30)/2 = 25, but this implementation picks upper middle.

        let tracker = LatencyTracker::new(10);
        tracker.record(Duration::from_micros(10));
        tracker.record(Duration::from_micros(20));
        tracker.record(Duration::from_micros(30));
        tracker.record(Duration::from_micros(40));

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 4);
        assert_eq!(percentiles.p50_us, 30);
    }

    #[test]
    fn test_latency_tracker_single_sample() {
        let tracker = LatencyTracker::new(100);
        tracker.record(Duration::from_micros(500));

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 1);
        assert_eq!(percentiles.p50_us, 500);
        assert_eq!(percentiles.min_us, 500);
        assert_eq!(percentiles.max_us, 500);
    }

    #[test]
    fn test_latency_tracker_percentiles() {
        let tracker = LatencyTracker::new(100);

        // Add 100 samples with values 1-100 microseconds
        for i in 1..=100 {
            tracker.record(Duration::from_micros(i));
        }

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 100);
        assert_eq!(percentiles.min_us, 1);
        assert_eq!(percentiles.max_us, 100);
        // p50 at index 50 (0-indexed) = value 51
        assert_eq!(percentiles.p50_us, 51);
        // p95 at index 95 = value 96
        assert_eq!(percentiles.p95_us, 96);
        // p99 at index 99 = value 100
        assert_eq!(percentiles.p99_us, 100);
        // avg = sum(1..=100)/100 = 5050/100 = 50
        assert_eq!(percentiles.avg_us, 50);
    }

    #[test]
    fn test_latency_tracker_circular_buffer() {
        let tracker = LatencyTracker::new(10);

        // Add 20 samples, only last 10 should be kept
        for i in 1..=20 {
            tracker.record(Duration::from_micros(i));
        }

        let percentiles = tracker.percentiles();
        assert_eq!(percentiles.sample_count, 10);
        assert_eq!(percentiles.min_us, 11); // Oldest kept is 11
        assert_eq!(percentiles.max_us, 20);
    }

    #[test]
    fn test_latency_percentiles_meets_target() {
        let good = LatencyPercentiles {
            p50_us: 500,
            p95_us: 900,
            p99_us: 950,
            min_us: 100,
            max_us: 1000,
            avg_us: 500,
            sample_count: 100,
        };
        assert!(good.meets_target());

        let bad = LatencyPercentiles {
            p50_us: 1500, // Exceeds 1ms target
            p95_us: 2000,
            p99_us: 3000,
            min_us: 500,
            max_us: 5000,
            avg_us: 1500,
            sample_count: 100,
        };
        assert!(!bad.meets_target());
    }

    #[test]
    fn test_cold_latency_tracking() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Store some versions
        for i in 1..=10 {
            let version = create_test_node_version(i);
            tiered.store_node_version(&version).unwrap();
        }

        // Access them from cold storage (first access bypasses warm cache)
        for i in 1..=10 {
            let _ = tiered
                .get_node_version_cold(VersionId::new(i).unwrap())
                .unwrap();
        }

        let metrics = tiered.metrics();
        assert_eq!(metrics.cold_hits, 10);
        assert!(metrics.cold_latency.sample_count > 0);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        // Wrap TieredStorage in Arc for thread safety
        let tiered = Arc::new(TieredStorage::with_default_config(Arc::new(cold)));

        // Store a version first so threads have something to read
        let version = create_test_node_version(1);
        tiered.store_node_version(&version).unwrap();

        let mut handles = vec![];

        // Reduce concurrency to avoid CI resource exhaustion (especially on Windows)
        for _ in 0..4 {
            let tiered_clone = Arc::clone(&tiered);
            let handle = thread::spawn(move || {
                for _ in 0..50 {
                    // All threads hammer the same version
                    let _ = tiered_clone
                        .get_node_version_cold(VersionId::new(1).unwrap())
                        .unwrap();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let metrics = tiered.metrics();
        // Total operations = 4 threads * 50 loops = 200
        // One cold hit (first access), rest warm hits
        // Note: Due to race conditions on initial cache population, multiple threads might
        // miss the cache and hit cold storage simultaneously.
        assert_eq!(metrics.cold_hits + metrics.warm_hits, 200);
        assert!(metrics.cold_hits >= 1);

        // Verify latency tracker recorded samples safely
        assert!(metrics.cold_latency.sample_count > 0);
    }

    #[test]
    fn test_prefetch_limits() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Chain of 10 versions
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

        for i in 2..=10 {
            let old_props = PropertyMapBuilder::new()
                .insert("step", (i - 1) as i64)
                .build();
            let new_props = PropertyMapBuilder::new().insert("step", i as i64).build();
            let v = NodeVersion::new_delta(
                VersionId::new(i).unwrap(),
                NodeId::new(100).unwrap(),
                BiTemporalInterval::current(((1000 + i * 100) as i64).into()),
                GLOBAL_INTERNER.intern("Test").unwrap(),
                &old_props,
                &new_props,
                VersionId::new(i - 1).unwrap(),
            );
            versions.push(v);
        }

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: true,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        // Fetch head (v10)
        let _ = tiered
            .get_node_version_cold(VersionId::new(10).unwrap())
            .unwrap();

        let metrics = tiered.metrics();
        // Prefetches should be exactly 3: v9, v8, v7
        assert_eq!(metrics.prefetches, 3);

        // Verify what is in cache implicitly by checking hit counts

        // Fetch v9 -> warm hit (was prefetched)
        let _ = tiered
            .get_node_version_cold(VersionId::new(9).unwrap())
            .unwrap();
        assert_eq!(tiered.metrics().warm_hits, 1);

        // Fetch v7 -> warm hit (was prefetched)
        let _ = tiered
            .get_node_version_cold(VersionId::new(7).unwrap())
            .unwrap();
        assert_eq!(tiered.metrics().warm_hits, 2);

        // Fetch v6 -> cold hit (was NOT prefetched because depth=3 from v10 covers v9, v8, v7)
        let _ = tiered
            .get_node_version_cold(VersionId::new(6).unwrap())
            .unwrap();

        let final_metrics = tiered.metrics();
        assert_eq!(final_metrics.warm_hits, 2); // Unchanged
        // Initial v10 + v6 = 2 cold hits
        assert_eq!(final_metrics.cold_hits, 2);
    }

    #[test]
    fn test_cache_eviction() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Use a small cache
        let config = TieredStorageConfig {
            warm_cache_size: 5,
            enable_prefetch: false,
            prefetch_depth: 0,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        // Store 10 versions
        for i in 1..=10 {
            let version = create_test_node_version(i);
            tiered.store_node_version(&version).unwrap();
        }

        // Access all 10 versions to populate cache
        // This will cause 10 cold hits.
        for i in 1..=10 {
            tiered
                .get_node_version_cold(VersionId::new(i).unwrap())
                .unwrap();
        }

        // Now access them again. Since cache size is 5, we expect at most 5 warm hits
        // (the recently accessed ones), and at least 5 cold hits (the evicted ones).
        let metrics_before = tiered.metrics();

        for i in 1..=10 {
            tiered
                .get_node_version_cold(VersionId::new(i).unwrap())
                .unwrap();
        }

        let metrics_after = tiered.metrics();
        let warm_hits = metrics_after.warm_hits - metrics_before.warm_hits;
        let cold_hits = metrics_after.cold_hits - metrics_before.cold_hits;

        assert!(
            warm_hits <= 5,
            "Expected at most 5 warm hits, got {}",
            warm_hits
        );
        assert!(
            cold_hits >= 5,
            "Expected at least 5 cold hits, got {}",
            cold_hits
        );
    }

    #[test]
    fn test_error_propagation_writes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();
        let tiered = TieredStorage::with_default_config(Arc::new(cold));

        // Enable write failure injection
        tiered.cold_storage().set_fail_writes(true);

        let version = create_test_node_version(1);
        let result = tiered.store_node_version(&version);

        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("Simulated write failure"));

        // Also test batch store
        let result = tiered.store_node_versions_batch(&[version]);
        assert!(result.is_err());
    }

    #[test]
    fn test_prefetch_latency_metric_behavior() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = RedbColdStorage::with_default_config(&db_path).unwrap();

        let config = TieredStorageConfig {
            warm_cache_size: 100,
            enable_prefetch: true,
            prefetch_depth: 3,
        };
        let tiered = TieredStorage::new(config, Arc::new(cold));

        // Create a chain of versions: v4 -> v3 -> v2 -> v1
        let mut versions = Vec::new();
        let v1 = create_test_node_version(1);
        versions.push(v1);

        for i in 2..=4 {
            let v = NodeVersion::new_delta(
                VersionId::new(i).unwrap(),
                NodeId::new(100).unwrap(),
                BiTemporalInterval::current(((1000 + i * 100) as i64).into()),
                GLOBAL_INTERNER.intern("Test").unwrap(),
                &PropertyMapBuilder::new().build(),
                &PropertyMapBuilder::new().build(),
                VersionId::new(i - 1).unwrap(),
            );
            versions.push(v);
        }

        // Store all in cold storage
        for v in &versions {
            tiered.store_node_version(v).unwrap();
        }

        // Access the head version (v4)
        // This should trigger:
        // 1. Fetch v4 (cold miss) -> records latency
        // 2. Prefetch v3 (cold) -> DOES NOT record latency in cold_latency metric
        // 3. Prefetch v2 (cold) -> DOES NOT record latency
        // 4. Prefetch v1 (cold) -> DOES NOT record latency
        let _ = tiered
            .get_node_version_cold(VersionId::new(4).unwrap())
            .unwrap();

        let metrics = tiered.metrics();

        // Verify we only have 1 latency sample despite doing 4 disk reads
        assert_eq!(
            metrics.cold_latency.sample_count, 1,
            "Should record latency only for the requested item, not prefetches"
        );

        // Verify we did prefetch
        assert_eq!(metrics.prefetches, 3, "Should have prefetched 3 items");

        // Verify cache state: all 4 should be in warm cache
        for i in 1..=4 {
            // Since we just accessed them (or prefetched them), they should be in warm cache
            // Note: get_node_version_cold increments warm_hits if found
            // We expect returns Ok(Some(_))
            assert!(
                tiered
                    .get_node_version_cold(VersionId::new(i).unwrap())
                    .unwrap()
                    .is_some()
            );
        }

        let final_metrics = tiered.metrics();
        // Initial cold hit (v4) + 4 warm hits (checking v1..v4)
        // Total cold hits: 1 (v4 initial)
        // Total warm hits: 4 (subsequent checks)
        assert_eq!(final_metrics.cold_hits, 1);
        assert_eq!(final_metrics.warm_hits, 4);
    }
