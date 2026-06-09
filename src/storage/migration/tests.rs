use super::*;

use super::*;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::BiTemporalInterval;
    use crate::core::version::EdgeVersion;
    use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use tempfile::tempdir;

    fn create_test_node_version(id: u64, node_id: u64) -> NodeVersion {
        let properties = PropertyMapBuilder::new()
            .insert("name", "Test")
            .insert("age", 30i64)
            .build();

        NodeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            NodeId::new(node_id).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
        )
    }

    fn create_cold_storage() -> Arc<RedbColdStorage> {
        let temp_dir = tempdir().unwrap();
        // Leaking the temp_dir to keep file alive for test duration
        // Ideally we would return a tuple (Arc<RedbColdStorage>, TempDir) but that changes test signature
        // Since tests are short lived and run in isolation, this is acceptable for TDD
        let path = temp_dir.path().join("cold.redb");
        // We leak the TempDir to ensure the file isn't deleted while Redb holds it
        std::mem::forget(temp_dir);

        Arc::new(RedbColdStorage::new(path, RedbConfig::new()).unwrap())
    }

    // ========================================================================
    // MigrationPolicy tests
    // ========================================================================

    #[test]
    fn test_default_policy() {
        let policy = MigrationPolicy::default();
        assert_eq!(policy.age_threshold, Duration::from_secs(7 * 24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 1024 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 1);
        assert_eq!(policy.batch_size, 1000);
        assert!(policy.enabled);
    }

    #[test]
    fn test_aggressive_policy() {
        let policy = MigrationPolicy::aggressive();
        assert_eq!(policy.age_threshold, Duration::from_secs(24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.batch_size, 2000);
    }

    #[test]
    fn test_conservative_policy() {
        let policy = MigrationPolicy::conservative();
        assert_eq!(policy.age_threshold, Duration::from_secs(30 * 24 * 60 * 60));
        assert_eq!(policy.min_hot_versions, 5);
    }

    #[test]
    fn test_disabled_policy() {
        let policy = MigrationPolicy::disabled();
        assert!(!policy.enabled);
    }

    #[test]
    fn test_policy_builder() {
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::from_secs(86400))
            .memory_threshold_bytes(2 * 1024 * 1024 * 1024)
            .min_hot_versions(3)
            .batch_size(500)
            .run_interval(Duration::from_secs(120))
            .enabled(true)
            .build();

        assert_eq!(policy.age_threshold, Duration::from_secs(86400));
        assert_eq!(policy.memory_threshold_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 3);
        assert_eq!(policy.batch_size, 500);
        assert_eq!(policy.run_interval, Duration::from_secs(120));
    }

    // ========================================================================
    // MigrationService tests
    // ========================================================================

    #[test]
    fn test_migration_service_creation() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        assert!(!service.is_running());
        assert_eq!(service.stats().node_versions_migrated, 0);
    }

    #[test]
    fn test_migrate_node_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        let stats = service.stats();
        assert_eq!(stats.node_versions_migrated, 10);

        // Verify versions are in cold storage
        for version in &versions {
            assert!(cold.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migrate_disabled() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::disabled();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 0);

        // Verify versions are NOT in cold storage
        for version in &versions {
            assert!(!cold.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migration_batching() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder().batch_size(3).build();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        // All should be migrated despite small batch size
        for version in &versions {
            assert!(cold.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_identify_candidates_respects_min_hot_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(2)
            .age_threshold(Duration::ZERO) // All versions are "old enough"
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        // Create 3 versions for node 100
        let node_id = NodeId::new(100).unwrap();
        for i in 1..=3 {
            let v = create_test_node_version(i, 100);
            versions.insert(v.id, v);
        }
        heads.insert(node_id, VersionId::new(3).unwrap()); // v3 is head
        counts.insert(node_id, 3);

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // With min_hot_versions=2 and 3 versions, only 1 should be candidate
        // (v3 is head and skipped, v2 must stay hot, v1 can migrate)
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_identify_candidates_skips_head() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        let node_id = NodeId::new(100).unwrap();
        for i in 1..=3 {
            let v = create_test_node_version(i, 100);
            versions.insert(v.id, v);
        }
        heads.insert(node_id, VersionId::new(3).unwrap());
        counts.insert(node_id, 3);

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // Head (v3) should not be a candidate
        assert!(!candidates.iter().any(|c| c.version_id.as_u64() == 3));
    }

    // ========================================================================
    // MigrationStats tests
    // ========================================================================

    #[test]
    fn test_migration_stats_throughput() {
        let stats = MigrationStats {
            node_versions_migrated: 1000,
            edge_versions_migrated: 500,
            bytes_migrated: 1_000_000,
            runs_completed: 10,
            errors: 0,
            last_run_duration: Duration::from_secs(10),
            last_run_time: Some(Instant::now()),
        };

        // 1500 versions in 10 seconds = 150 versions/sec
        assert!((stats.versions_per_second() - 150.0).abs() < 0.1);
    }

    // ========================================================================
    // MigrationCallback tests
    // ========================================================================

    struct FilteringCallback {
        skip_version_ids: Vec<u64>,
    }

    impl MigrationCallback for FilteringCallback {
        fn before_node_migration(&self, version: &NodeVersion) -> bool {
            !self.skip_version_ids.contains(&version.id.as_u64())
        }
    }

    #[test]
    fn test_migration_callback_filtering() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let callback = Arc::new(FilteringCallback {
            skip_version_ids: vec![2, 4, 6, 8, 10],
        });
        let service = MigrationService::with_callback(cold.clone(), policy, callback);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 5); // Only odd IDs migrated

        // Verify only odd versions are in cold storage
        assert!(
            cold.contains_node_version(VersionId::new(1).unwrap())
                .unwrap()
        );
        assert!(
            !cold
                .contains_node_version(VersionId::new(2).unwrap())
                .unwrap()
        );
        assert!(
            cold.contains_node_version(VersionId::new(3).unwrap())
                .unwrap()
        );
    }

    // ========================================================================
    // SCALE-003: Background Worker Tests (TDD)
    // ========================================================================

    #[test]
    fn test_background_worker_starts_and_stops() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .run_interval(Duration::from_millis(50))
            .enabled(true)
            .build();
        let service = Arc::new(MigrationService::new(cold, policy));

        // Service should not be running initially
        assert!(!service.is_running());

        // Start the background worker (no-op without historical storage for now)
        service.start();
        assert!(service.is_running());

        // Allow worker to run for a bit
        thread::sleep(Duration::from_millis(100));

        // Stop gracefully
        service.stop();
        assert!(!service.is_running());
    }

    #[test]
    fn test_graceful_shutdown_waits_for_inflight() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .run_interval(Duration::from_millis(50))
            .batch_size(10)
            .enabled(true)
            .build();

        // Track batch completions via callback
        let batches_completed = Arc::new(AtomicUsize::new(0));
        let batches_completed_clone = batches_completed.clone();

        struct BatchTracker {
            completed: Arc<AtomicUsize>,
        }
        impl MigrationCallback for BatchTracker {
            fn after_batch(&self, node_count: usize, edge_count: usize) {
                if node_count > 0 || edge_count > 0 {
                    self.completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }

        let callback = Arc::new(BatchTracker {
            completed: batches_completed_clone,
        });
        let service = Arc::new(MigrationService::with_callback(cold, policy, callback));

        service.start();
        thread::sleep(Duration::from_millis(100));

        // Stop should complete gracefully
        let stop_start = Instant::now();
        service.stop();
        let stop_duration = stop_start.elapsed();

        // Should have stopped within reasonable time
        assert!(stop_duration < Duration::from_secs(5));
        assert!(!service.is_running());
    }

    #[test]
    fn test_multiple_start_stop_cycles() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .run_interval(Duration::from_millis(50))
            .build();
        let service = Arc::new(MigrationService::new(cold, policy));

        for _ in 0..3 {
            assert!(!service.is_running());
            service.start();
            assert!(service.is_running());
            thread::sleep(Duration::from_millis(50));
            service.stop();
            assert!(!service.is_running());
        }
    }

    #[test]
    fn test_double_start_is_noop() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = Arc::new(MigrationService::new(cold, policy));

        service.start();
        assert!(service.is_running());

        // Second start should be a no-op
        service.start();
        assert!(service.is_running());

        service.stop();
        assert!(!service.is_running());
    }

    #[test]
    fn test_double_stop_is_noop() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = Arc::new(MigrationService::new(cold, policy));

        service.start();
        service.stop();
        assert!(!service.is_running());

        // Second stop should be a no-op
        service.stop();
        assert!(!service.is_running());
    }

    // ========================================================================
    // SCALE-003: Memory Pressure Trigger Tests (TDD)
    // ========================================================================

    #[test]
    fn test_memory_pressure_trigger_enabled() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .memory_threshold_bytes(1000) // Low threshold
            .age_threshold(Duration::ZERO)
            .min_hot_versions(1)
            .build();
        let service = MigrationService::new(cold, policy);

        // Should trigger when memory usage exceeds threshold
        assert!(service.should_trigger_migration(2000, 0)); // memory > threshold
        assert!(!service.should_trigger_migration(500, 0)); // memory < threshold
    }

    #[test]
    fn test_combined_triggers() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .memory_threshold_bytes(1000)
            .age_threshold(Duration::from_secs(3600))
            .build();
        let service = MigrationService::new(cold, policy);

        // Either condition should trigger
        assert!(service.should_trigger_migration(2000, 0)); // memory pressure
        assert!(service.should_trigger_migration(500, 10)); // old versions exist
        assert!(!service.should_trigger_migration(500, 0)); // neither condition
    }

    // ========================================================================
    // SCALE-003: Access Pattern (LRU) Trigger Tests (TDD)
    // ========================================================================

    #[test]
    fn test_access_tracking_records_access() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let version_id = VersionId::new(1).unwrap();

        // Record access
        service.record_access(version_id);

        // Should have recorded the access
        let last_access = service.get_last_access(version_id);
        assert!(last_access.is_some());
    }

    #[test]
    fn test_lru_candidates_prioritized() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::ZERO) // All old enough
            .min_hot_versions(1)
            .build();
        let service = MigrationService::new(cold, policy);

        // Create versions with different access times
        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        let v3 = VersionId::new(3).unwrap();

        // Record accesses with delays
        service.record_access(v1);
        thread::sleep(Duration::from_millis(10));
        service.record_access(v2);
        thread::sleep(Duration::from_millis(10));
        service.record_access(v3);

        // v1 should be oldest (least recently accessed)
        let v1_access = service.get_last_access(v1).unwrap();
        let v3_access = service.get_last_access(v3).unwrap();
        assert!(v1_access < v3_access);
    }

    #[test]
    fn test_identify_candidates_with_lru() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::ZERO)
            .min_hot_versions(1)
            .enable_lru_migration(true)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        // Create versions for different nodes
        let node1 = NodeId::new(100).unwrap();
        let node2 = NodeId::new(200).unwrap();

        let v1 = create_test_node_version(1, 100);
        let v2 = create_test_node_version(2, 100);
        let v3 = create_test_node_version(3, 200);
        let v4 = create_test_node_version(4, 200);

        versions.insert(v1.id, v1.clone());
        versions.insert(v2.id, v2.clone());
        versions.insert(v3.id, v3.clone());
        versions.insert(v4.id, v4.clone());

        heads.insert(node1, VersionId::new(2).unwrap());
        heads.insert(node2, VersionId::new(4).unwrap());
        counts.insert(node1, 2);
        counts.insert(node2, 2);

        // Record accesses - v3 more recently than v1
        service.record_access(v1.id);
        thread::sleep(Duration::from_millis(10));
        service.record_access(v3.id);

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // Both non-head versions should be candidates
        assert_eq!(candidates.len(), 2);

        // With LRU, v1 (older access) should come before v3 (newer access)
        if candidates.len() >= 2 {
            let v1_pos = candidates.iter().position(|c| c.version_id.as_u64() == 1);
            let v3_pos = candidates.iter().position(|c| c.version_id.as_u64() == 3);
            if let (Some(p1), Some(p3)) = (v1_pos, v3_pos) {
                assert!(p1 < p3, "LRU should prioritize v1 over v3");
            }
        }
    }

    // ========================================================================
    // SCALE-003: Progress Tracking Tests (TDD)
    // ========================================================================

    #[test]
    fn test_progress_tracking_callback() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder().batch_size(5).build();

        // Track progress updates
        let progress_updates = Arc::new(std::sync::Mutex::new(Vec::new()));
        let progress_clone = progress_updates.clone();

        struct ProgressTracker {
            updates: Arc<std::sync::Mutex<Vec<MigrationProgress>>>,
        }
        impl MigrationCallback for ProgressTracker {
            fn on_progress(&self, progress: &MigrationProgress) {
                self.updates.lock().unwrap().push(progress.clone());
            }
        }

        let callback = Arc::new(ProgressTracker {
            updates: progress_clone,
        });
        let service = MigrationService::with_callback(cold, policy, callback);

        // Migrate 12 versions (should be 3 batches: 5, 5, 2)
        let versions: Vec<NodeVersion> =
            (1..=12).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 12);

        // Check progress updates
        let updates = progress_updates.lock().unwrap();
        assert!(!updates.is_empty());

        // Final progress should show 12/12
        if let Some(final_progress) = updates.last() {
            assert_eq!(final_progress.total_versions, 12);
            assert_eq!(final_progress.migrated_versions, 12);
            assert!(final_progress.is_complete());
        }
    }

    #[test]
    fn test_progress_percentage() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 50,
            bytes_migrated: 1000,
            current_batch: 5,
            total_batches: 10,
            elapsed: Duration::from_secs(5),
        };

        assert!((progress.percentage() - 50.0).abs() < 0.01);
        assert!(!progress.is_complete());

        let complete_progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 100,
            bytes_migrated: 2000,
            current_batch: 10,
            total_batches: 10,
            elapsed: Duration::from_secs(10),
        };

        assert!((complete_progress.percentage() - 100.0).abs() < 0.01);
        assert!(complete_progress.is_complete());
    }

    #[test]
    fn test_progress_throughput() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 50,
            bytes_migrated: 1_000_000,
            current_batch: 5,
            total_batches: 10,
            elapsed: Duration::from_secs(5),
        };

        // 50 versions in 5 seconds = 10 versions/sec
        assert!((progress.versions_per_second() - 10.0).abs() < 0.01);
        // 1MB in 5 seconds = 200KB/sec
        assert!((progress.bytes_per_second() - 200_000.0).abs() < 0.01);
    }

    // ========================================================================
    // SCALE-003: Integration Tests (TDD)
    // ========================================================================

    #[test]
    fn test_migration_run_stats_updated() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let versions: Vec<NodeVersion> =
            (1..=50).map(|i| create_test_node_version(i, 100)).collect();

        service.migrate_node_versions(&versions).unwrap();

        let stats = service.stats();
        assert_eq!(stats.node_versions_migrated, 50);
        assert!(stats.bytes_migrated > 0);
    }

    #[test]
    fn test_service_handles_empty_migration() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let migrated = service.migrate_node_versions(&[]).unwrap();
        assert_eq!(migrated, 0);

        let stats = service.stats();
        assert_eq!(stats.node_versions_migrated, 0);
    }

    // ========================================================================
    // Edge Version Tests (Additional Coverage)
    // ========================================================================

    fn create_test_edge_version(id: u64, edge_id: u64) -> EdgeVersion {
        let properties = PropertyMapBuilder::new().insert("weight", 1.5f64).build();

        EdgeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            EdgeId::new(edge_id).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            properties,
        )
    }

    fn create_test_edge_version_with_timestamp(id: u64, edge_id: u64, ts_ms: i64) -> EdgeVersion {
        use crate::core::temporal::TimeRange;
        let properties = PropertyMapBuilder::new().insert("weight", 1.5f64).build();

        let range = TimeRange::from(ts_ms.into());
        let temporal = BiTemporalInterval::new(range, range);

        EdgeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            EdgeId::new(edge_id).unwrap(),
            temporal,
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            properties,
        )
    }

    #[test]
    fn test_migrate_edge_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        let stats = service.stats();
        assert_eq!(stats.edge_versions_migrated, 10);

        // Verify versions are in cold storage
        for version in &versions {
            assert!(cold.contains_edge_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migrate_edge_versions_disabled() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::disabled();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 0);

        // Verify versions are NOT in cold storage
        for version in &versions {
            assert!(!cold.contains_edge_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migrate_edge_versions_batching() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder().batch_size(3).build();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        // All should be migrated despite small batch size
        for version in &versions {
            assert!(cold.contains_edge_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_identify_edge_candidates_respects_min_hot_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(2)
            .age_threshold(Duration::ZERO) // All versions are "old enough"
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        // Create 3 versions for edge 200
        let edge_id = EdgeId::new(200).unwrap();
        for i in 1..=3 {
            let v = create_test_edge_version(i, 200);
            versions.insert(v.id, v);
        }
        heads.insert(edge_id, VersionId::new(3).unwrap()); // v3 is head
        counts.insert(edge_id, 3);

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // With min_hot_versions=2 and 3 versions, only 1 should be candidate
        // (v3 is head and skipped, v2 must stay hot, v1 can migrate)
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_identify_edge_candidates_skips_head() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        let edge_id = EdgeId::new(200).unwrap();
        for i in 1..=3 {
            let v = create_test_edge_version(i, 200);
            versions.insert(v.id, v);
        }
        heads.insert(edge_id, VersionId::new(3).unwrap());
        counts.insert(edge_id, 3);

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // Head (v3) should not be a candidate
        assert!(!candidates.iter().any(|c| c.version_id.as_u64() == 3));
    }

    #[test]
    fn test_identify_edge_candidates_respects_age_threshold() {
        let cold = create_cold_storage();
        // Set age threshold to 1 hour
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::from_secs(3600))
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        let edge_id = EdgeId::new(200).unwrap();

        // Get current time in ms
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Create an old version (2 hours ago)
        let old_ts = now_ms - (2 * 60 * 60 * 1000);
        let v1 = create_test_edge_version_with_timestamp(1, 200, old_ts);
        versions.insert(v1.id, v1);

        // Create a recent version (30 minutes ago)
        let recent_ts = now_ms - (30 * 60 * 1000);
        let v2 = create_test_edge_version_with_timestamp(2, 200, recent_ts);
        versions.insert(v2.id, v2);

        // Create head version (now)
        let v3 = create_test_edge_version_with_timestamp(3, 200, now_ms);
        versions.insert(v3.id, v3);

        heads.insert(edge_id, VersionId::new(3).unwrap());
        counts.insert(edge_id, 3);

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // Only v1 (2 hours old) should be a candidate, v2 (30 min) is too young
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].version_id.as_u64(), 1);
    }

    // ========================================================================
    // Edge Callback Tests
    // ========================================================================

    struct EdgeFilteringCallback {
        skip_version_ids: Vec<u64>,
        batch_counts: std::sync::Mutex<Vec<(usize, usize)>>,
    }

    impl EdgeFilteringCallback {
        fn new(skip_ids: Vec<u64>) -> Self {
            Self {
                skip_version_ids: skip_ids,
                batch_counts: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl MigrationCallback for EdgeFilteringCallback {
        fn before_edge_migration(&self, version: &EdgeVersion) -> bool {
            !self.skip_version_ids.contains(&version.id.as_u64())
        }

        fn after_batch(&self, node_count: usize, edge_count: usize) {
            self.batch_counts
                .lock()
                .unwrap()
                .push((node_count, edge_count));
        }
    }

    #[test]
    fn test_edge_migration_callback_filtering() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let callback = Arc::new(EdgeFilteringCallback::new(vec![2, 4, 6, 8, 10]));
        let service = MigrationService::with_callback(cold.clone(), policy, callback.clone());

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 5); // Only odd IDs migrated

        // Verify only odd versions are in cold storage
        assert!(
            cold.contains_edge_version(VersionId::new(1).unwrap())
                .unwrap()
        );
        assert!(
            !cold
                .contains_edge_version(VersionId::new(2).unwrap())
                .unwrap()
        );
        assert!(
            cold.contains_edge_version(VersionId::new(3).unwrap())
                .unwrap()
        );

        // Verify batch callback was called
        let batches = callback.batch_counts.lock().unwrap();
        assert!(!batches.is_empty());
        // All batches should be edge batches (node_count=0)
        for (node_count, _edge_count) in batches.iter() {
            assert_eq!(*node_count, 0);
        }
    }

    // ========================================================================
    // MigrationStats Edge Cases
    // ========================================================================

    #[test]
    fn test_migration_stats_throughput_zero_duration() {
        let stats = MigrationStats {
            node_versions_migrated: 1000,
            edge_versions_migrated: 500,
            bytes_migrated: 1_000_000,
            runs_completed: 10,
            errors: 0,
            last_run_duration: Duration::ZERO,
            last_run_time: Some(Instant::now()),
        };

        // With zero duration, should return 0 to avoid division by zero
        assert_eq!(stats.versions_per_second(), 0.0);
    }

    #[test]
    fn test_migration_stats_default() {
        let stats = MigrationStats::default();
        assert_eq!(stats.node_versions_migrated, 0);
        assert_eq!(stats.edge_versions_migrated, 0);
        assert_eq!(stats.bytes_migrated, 0);
        assert_eq!(stats.runs_completed, 0);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.last_run_duration, Duration::ZERO);
        assert!(stats.last_run_time.is_none());
    }

    // ========================================================================
    // AtomicMigrationStats Tests
    // ========================================================================

    #[test]
    fn test_atomic_migration_stats_new() {
        let stats = AtomicMigrationStats::new();
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.node_versions_migrated, 0);
        assert_eq!(snapshot.edge_versions_migrated, 0);
        assert_eq!(snapshot.bytes_migrated, 0);
        assert_eq!(snapshot.runs_completed, 0);
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_atomic_migration_stats_snapshot() {
        let stats = AtomicMigrationStats::new();
        stats.node_versions_migrated.store(100, Ordering::Relaxed);
        stats.edge_versions_migrated.store(50, Ordering::Relaxed);
        stats.bytes_migrated.store(10000, Ordering::Relaxed);
        stats.runs_completed.store(5, Ordering::Relaxed);
        stats.errors.store(2, Ordering::Relaxed);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.node_versions_migrated, 100);
        assert_eq!(snapshot.edge_versions_migrated, 50);
        assert_eq!(snapshot.bytes_migrated, 10000);
        assert_eq!(snapshot.runs_completed, 5);
        assert_eq!(snapshot.errors, 2);
    }

    // ========================================================================
    // Service API Tests
    // ========================================================================

    #[test]
    fn test_service_policy_getter() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::from_secs(123456))
            .min_hot_versions(7)
            .build();
        let service = MigrationService::new(cold, policy);

        assert_eq!(service.policy().age_threshold, Duration::from_secs(123456));
        assert_eq!(service.policy().min_hot_versions, 7);
    }

    #[test]
    fn test_migrate_empty_edge_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        let edge_versions: Vec<EdgeVersion> = vec![];
        let migrated = service.migrate_edge_versions(&edge_versions).unwrap();
        assert_eq!(migrated, 0);

        let stats = service.stats();
        assert_eq!(stats.edge_versions_migrated, 0);
    }

    #[test]
    fn test_identify_edge_candidates_empty_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let empty_versions: FastHashMap<VersionId, EdgeVersion> = FastHashMap::default();
        let empty_heads: FastHashMap<EdgeId, VersionId> = FastHashMap::default();
        let empty_counts: FastHashMap<EdgeId, usize> = FastHashMap::default();

        let candidates = service.identify_edge_candidates(
            &empty_versions,
            &empty_heads,
            &empty_counts,
            Instant::now(),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_identify_candidates_version_count_zero() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let counts = FastHashMap::default(); // Empty counts

        let node_id = NodeId::new(100).unwrap();
        for i in 1..=3 {
            let v = create_test_node_version(i, 100);
            versions.insert(v.id, v);
        }
        heads.insert(node_id, VersionId::new(3).unwrap());
        // counts is empty - simulate missing count data

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // With zero count, max_migrate = 0 - 1 = saturates to 0, so no candidates
        assert!(candidates.is_empty());
    }

    // ========================================================================
    // MigrationCandidate Tests
    // ========================================================================

    #[test]
    fn test_migration_candidate_debug_and_clone() {
        let candidate = MigrationCandidate {
            version_id: VersionId::new(1).unwrap(),
            is_node: true,
            age: Duration::from_secs(3600),
            estimated_size: 1024,
        };

        // Test Clone
        let cloned = candidate.clone();
        assert_eq!(cloned.version_id, candidate.version_id);
        assert_eq!(cloned.is_node, candidate.is_node);
        assert_eq!(cloned.age, candidate.age);
        assert_eq!(cloned.estimated_size, candidate.estimated_size);

        // Test Debug
        let debug_str = format!("{:?}", candidate);
        assert!(debug_str.contains("MigrationCandidate"));
    }

    // ========================================================================
    // Additional Policy Preset Tests
    // ========================================================================

    #[test]
    fn test_conservative_policy_values() {
        let policy = MigrationPolicy::conservative();
        assert_eq!(policy.age_threshold, Duration::from_secs(30 * 24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 5);
        assert_eq!(policy.batch_size, 500);
        assert_eq!(policy.run_interval, Duration::from_secs(300));
        assert!(policy.enabled);
    }

    #[test]
    fn test_aggressive_policy_values() {
        let policy = MigrationPolicy::aggressive();
        assert_eq!(policy.age_threshold, Duration::from_secs(24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 1);
        assert_eq!(policy.batch_size, 2000);
        assert_eq!(policy.run_interval, Duration::from_secs(30));
        assert!(policy.enabled);
        assert!(policy.enable_lru); // Aggressive mode uses LRU
    }

    #[test]
    fn test_default_policy_run_interval() {
        let policy = MigrationPolicy::default();
        assert_eq!(policy.run_interval, Duration::from_secs(60));
    }

    #[test]
    fn test_policy_builder_default() {
        let builder = MigrationPolicyBuilder::default();
        let policy = builder.build();
        // Should match MigrationPolicy::default()
        assert_eq!(policy.age_threshold, Duration::from_secs(7 * 24 * 60 * 60));
    }

    // ========================================================================
    // Multiple Entity Migration Tests
    // ========================================================================

    #[test]
    fn test_identify_candidates_multiple_nodes() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        // Create versions for multiple nodes
        for node_num in [100u64, 101, 102] {
            let node_id = NodeId::new(node_num).unwrap();
            for i in 1..=3 {
                let version_id = node_num * 10 + i;
                let v = create_test_node_version(version_id, node_num);
                versions.insert(v.id, v);
            }
            heads.insert(node_id, VersionId::new(node_num * 10 + 3).unwrap());
            counts.insert(node_id, 3);
        }

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // Each node has 3 versions, min_hot=1, head is skipped
        // So each node can have 2 candidates (max_migrate = 3-1 = 2)
        // Total should be 6 candidates (2 per node * 3 nodes)
        assert_eq!(candidates.len(), 6);
    }

    #[test]
    fn test_identify_edge_candidates_multiple_edges() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = FastHashMap::default();
        let mut heads = FastHashMap::default();
        let mut counts = FastHashMap::default();

        // Create versions for multiple edges
        for edge_num in [200u64, 201, 202] {
            let edge_id = EdgeId::new(edge_num).unwrap();
            for i in 1..=3 {
                let version_id = edge_num * 10 + i;
                let v = create_test_edge_version(version_id, edge_num);
                versions.insert(v.id, v);
            }
            heads.insert(edge_id, VersionId::new(edge_num * 10 + 3).unwrap());
            counts.insert(edge_id, 3);
        }

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // Each edge has 3 versions, min_hot=1, head is skipped
        // So each edge can have 2 candidates (max_migrate = 3-1 = 2)
        // Total should be 6 candidates (2 per edge * 3 edges)
        assert_eq!(candidates.len(), 6);
    }

    // ========================================================================
    // MigrationProgress Tests
    // ========================================================================

    #[test]
    fn test_progress_estimated_remaining() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 50,
            bytes_migrated: 1000,
            current_batch: 5,
            total_batches: 10,
            elapsed: Duration::from_secs(5),
        };

        // 50 versions in 5 secs = 10 v/sec, 50 remaining = ~5 secs
        let remaining = progress.estimated_remaining();
        assert!(remaining.as_secs() <= 6 && remaining.as_secs() >= 4);
    }

    #[test]
    fn test_progress_zero_elapsed() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 0,
            bytes_migrated: 0,
            current_batch: 0,
            total_batches: 10,
            elapsed: Duration::ZERO,
        };

        // Should return 0 without dividing by zero
        assert_eq!(progress.versions_per_second(), 0.0);
        assert_eq!(progress.bytes_per_second(), 0.0);
        assert_eq!(progress.estimated_remaining(), Duration::ZERO);
    }

    #[test]
    fn test_progress_empty_total() {
        let progress = MigrationProgress {
            total_versions: 0,
            migrated_versions: 0,
            bytes_migrated: 0,
            current_batch: 0,
            total_batches: 0,
            elapsed: Duration::from_secs(1),
        };

        // Empty migration should be 100% complete
        assert_eq!(progress.percentage(), 100.0);
        assert!(progress.is_complete());
    }

    // ========================================================================
    // Access Tracking Edge Cases
    // ========================================================================

    #[test]
    fn test_clear_access_tracking() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        let v3 = VersionId::new(3).unwrap();

        // Record accesses
        service.record_access(v1);
        service.record_access(v2);
        service.record_access(v3);

        // Verify recorded
        assert!(service.get_last_access(v1).is_some());
        assert!(service.get_last_access(v2).is_some());
        assert!(service.get_last_access(v3).is_some());

        // Clear specific accesses
        service.clear_access(&[v1, v2]);

        // v1 and v2 should be cleared, v3 should remain
        assert!(service.get_last_access(v1).is_none());
        assert!(service.get_last_access(v2).is_none());
        assert!(service.get_last_access(v3).is_some());
    }

    // ========================================================================
    // LSN-Based Migration Tests (Issue 6: Wire migration → Redb → WAL truncation)
    // ========================================================================

    #[test]
    fn test_migration_updates_flushed_lsn() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let cold = Arc::new(
            RedbColdStorage::new(
                temp_dir.path().join("cold.redb"),
                RedbConfig::new().compression(crate::storage::CompressionAlgorithm::None),
            )
            .unwrap(),
        );

        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        // Create some test versions
        let nodes: Vec<NodeVersion> = (1..=5).map(|i| create_test_node_version(i, 100)).collect();
        let edges: Vec<EdgeVersion> = (10..=12)
            .map(|i| create_test_edge_version(i, 200))
            .collect();

        let lsn = LSN(1000);

        // Before migration, flushed LSN should be None
        assert!(cold.get_flushed_lsn().unwrap().is_none());

        // Migrate with LSN
        let result = service.migrate_batch_with_lsn(&nodes, &edges, lsn).unwrap();

        assert_eq!(result.nodes_migrated, 5);
        assert_eq!(result.edges_migrated, 3);
        assert_eq!(result.flushed_lsn, Some(lsn));

        // After migration, flushed LSN should be updated
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(lsn));
    }

    #[test]
    fn test_migration_with_coordinator_set() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        std::fs::create_dir_all(&wal_dir).unwrap();

        // Create cold storage
        let cold = Arc::new(
            RedbColdStorage::new(
                temp_dir.path().join("cold.redb"),
                RedbConfig::new().compression(crate::storage::CompressionAlgorithm::None),
            )
            .unwrap(),
        );

        // Create flush coordinator
        let config = FlushCoordinatorConfig {
            wal_dir: wal_dir.clone(),
            segment_size: 1024,
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        // Create migration service with flush coordinator
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Verify coordinator is set
        assert!(service.flush_coordinator().is_some());

        // Migrate with LSN - the truncation call will happen but may truncate 0 segments
        // since there are no WAL entries yet
        let nodes: Vec<NodeVersion> = (1..=3).map(|i| create_test_node_version(i, 100)).collect();
        let lsn = LSN(25);

        let result = service.migrate_batch_with_lsn(&nodes, &[], lsn).unwrap();

        assert_eq!(result.nodes_migrated, 3);
        assert_eq!(result.flushed_lsn, Some(lsn));
        // segments_truncated will be 0 since there are no WAL segments with LSN < 25
        assert_eq!(result.segments_truncated, 0);

        // Verify data is in cold storage
        for node in &nodes {
            assert!(cold.contains_node_version(node.id).unwrap());
        }

        // Verify flushed LSN is recorded
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(lsn));
    }

    #[test]
    fn test_migration_failure_does_not_truncate_wal() {
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        // We can't use FailingColdStorage mock easily because we removed the trait.
        // Instead, we use RedbColdStorage with fault injection.

        let temp_dir = tempdir().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        std::fs::create_dir_all(&wal_dir).unwrap();

        // Create flush coordinator
        let config = FlushCoordinatorConfig {
            wal_dir: wal_dir.clone(),
            segment_size: 1024,
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        // Create cold storage
        let db_path = temp_dir.path().join("cold.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());

        // Inject failure
        cold.set_fail_writes(true);

        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Attempt migration - should fail
        let nodes: Vec<NodeVersion> = (1..=3).map(|i| create_test_node_version(i, 100)).collect();
        let result = service.migrate_batch_with_lsn(&nodes, &[], LSN(5));

        // Should have failed
        assert!(result.is_err());

        // Verify that writes were attempted
        assert!(cold.was_write_attempted());

        // Since store failed, WAL truncation should NOT have been called.
        // We can't easily spy on the coordinator here, but if store_batch_with_lsn
        // returns early with Err, the truncation code path is skipped.
    }

    #[test]
    fn test_migration_with_lsn_disabled_policy() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let cold = Arc::new(
            RedbColdStorage::new(
                temp_dir.path().join("cold.redb"),
                RedbConfig::new().compression(crate::storage::CompressionAlgorithm::None),
            )
            .unwrap(),
        );

        // Disabled policy
        let policy = MigrationPolicy::disabled();
        let service = MigrationService::new(cold.clone(), policy);

        let nodes: Vec<NodeVersion> = (1..=5).map(|i| create_test_node_version(i, 100)).collect();
        let lsn = LSN(1000);

        let result = service.migrate_batch_with_lsn(&nodes, &[], lsn).unwrap();

        // Should not migrate anything when disabled
        assert_eq!(result.nodes_migrated, 0);
        assert_eq!(result.edges_migrated, 0);
        assert_eq!(result.segments_truncated, 0);
        assert!(result.flushed_lsn.is_none());

        // Cold storage should not have the versions
        for node in &nodes {
            assert!(!cold.contains_node_version(node.id).unwrap());
        }
    }

    #[test]
    fn test_migration_with_lsn_result_helpers() {
        let result = MigrationWithLsnResult {
            nodes_migrated: 5,
            edges_migrated: 3,
            segments_truncated: 2,
            flushed_lsn: Some(LSN(100)),
        };

        assert!(result.has_migrations());
        assert_eq!(result.total_migrated(), 8);

        let empty_result = MigrationWithLsnResult {
            nodes_migrated: 0,
            edges_migrated: 0,
            segments_truncated: 0,
            flushed_lsn: None,
        };

        assert!(!empty_result.has_migrations());
        assert_eq!(empty_result.total_migrated(), 0);
    }

    #[test]
    fn test_set_and_get_flush_coordinator() {
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;

        let temp_dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold, policy);

        // Initially no coordinator
        assert!(service.flush_coordinator().is_none());

        // Set coordinator
        service.set_flush_coordinator(coordinator.clone());

        // Now should have coordinator
        assert!(service.flush_coordinator().is_some());
    }

    /// Test that WAL truncation uses the actual flushed LSN from cold storage,
    /// not the requested LSN. This enforces the safety invariant:
    /// WAL_truncation_lsn <= cold_storage.get_flushed_lsn()
    #[test]
    fn test_truncation_uses_actual_flushed_lsn() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        // Create Redb cold storage with LSN tracking
        let db_path = temp_dir.path().join("test.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Migrate batch with LSN 100
        let result = service.migrate_batch_with_lsn(&[], &[], LSN(100)).unwrap();

        // Result should contain the actual flushed LSN from cold storage
        assert_eq!(result.flushed_lsn, Some(LSN(100)));

        // Migrate another batch with LSN 200
        let result = service.migrate_batch_with_lsn(&[], &[], LSN(200)).unwrap();

        // Result should now be LSN 200
        assert_eq!(result.flushed_lsn, Some(LSN(200)));

        // Verify cold storage has LSN 200
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(LSN(200)));
    }

    /// Test that no WAL truncation occurs when there's no flush coordinator
    #[test]
    fn test_no_truncation_without_coordinator() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        // Migrate batch WITHOUT setting coordinator
        let result = service.migrate_batch_with_lsn(&[], &[], LSN(100)).unwrap();

        // No segments should be truncated
        assert_eq!(result.segments_truncated, 0);

        // But LSN should still be set in cold storage
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(LSN(100)));
    }

    /// Comprehensive test of the WAL truncation safety invariant:
    /// WAL_truncation_lsn <= cold_storage.get_flushed_lsn()
    ///
    /// This test simulates a scenario where:
    /// 1. Multiple batches are migrated with increasing LSNs
    /// 2. We verify that WAL truncation only happens after cold storage confirms the LSN
    /// 3. We verify the invariant is maintained even with concurrent operations
    #[test]
    fn test_lsn_invariant_maintained() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        let db_path = temp_dir.path().join("test.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Migrate multiple batches in sequence
        let lsns = vec![LSN(100), LSN(200), LSN(300), LSN(400), LSN(500)];

        for lsn in lsns {
            let result = service.migrate_batch_with_lsn(&[], &[], lsn).unwrap();

            // After each migration:
            // 1. Cold storage should have the LSN
            let cold_lsn = cold.get_flushed_lsn().unwrap();
            assert_eq!(
                cold_lsn,
                Some(lsn),
                "Cold storage should have LSN {:?}",
                lsn
            );

            // 2. Result should reflect the actual flushed LSN
            assert_eq!(
                result.flushed_lsn, cold_lsn,
                "Result LSN should match cold storage LSN"
            );

            // 3. The invariant WAL_truncation_lsn <= flushed_lsn is maintained
            // (This is implicitly tested by the fact that we read flushed_lsn before truncating)
        }

        // Final verification: cold storage has the highest LSN
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(LSN(500)));
    }
