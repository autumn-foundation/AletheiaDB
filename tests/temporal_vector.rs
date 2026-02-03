//! Integration tests for temporal vector support (Phase 3).
//!
//! Tests end-to-end temporal vector functionality including:
//! - Temporal vector index creation and configuration
//! - Snapshot creation with different strategies
//! - Point-in-time vector queries
//! - Time-range vector queries
//! - Snapshot pruning with retention policies

use gallifreydb::core::id::NodeId;
use gallifreydb::core::temporal::time;
use gallifreydb::core::temporal::{TimeRange, Timestamp};
use gallifreydb::index::vector::temporal::{
    RetentionPolicy, SnapshotStrategy, TemporalVectorConfig, TemporalVectorIndex,
};
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::utils::Result;
use std::time::Duration;

/// Helper to create a test temporal vector index with default config and fixed base time
fn create_test_index() -> Result<TemporalVectorIndex> {
    let base_time: Timestamp = 1_000_000.into();
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(hnsw_config),
    };
    TemporalVectorIndex::new_at(config, base_time)
}

#[test]
fn test_temporal_index_creation() -> Result<()> {
    let index = create_test_index()?;
    assert_eq!(index.dimensions(), 4);
    assert_eq!(index.distance_metric(), DistanceMetric::Cosine);
    assert_eq!(index.snapshot_count(), 0);
    Ok(())
}

#[test]
fn test_snapshot_creation_with_transaction_interval() -> Result<()> {
    let index = create_test_index()?;

    // Add vectors
    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    let vec2 = vec![0.0, 1.0, 0.0, 0.0];

    index.add(node1, &vec1, 1000.into())?;
    index.add(node2, &vec2, 2000.into())?;

    assert_eq!(index.snapshot_count(), 0);

    // Trigger snapshot (transaction interval = 2)
    index.on_transaction_at(2000.into())?;
    assert_eq!(index.snapshot_count(), 0); // Not yet

    index.on_transaction_at(2000.into())?;
    assert_eq!(index.snapshot_count(), 1); // Now created

    Ok(())
}

#[test]
fn test_point_in_time_vector_query() -> Result<()> {
    let index = create_test_index()?;

    // Add vectors at different times
    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();

    let timestamp1: Timestamp = 1_000_000.into();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], timestamp1)?;
    index.add(node2, &[0.9, 0.1, 0.0, 0.0], timestamp1)?;

    // Create snapshot with deterministic timestamp
    index.on_transaction_at(timestamp1)?;
    index.on_transaction_at(timestamp1)?;
    assert_eq!(index.snapshot_count(), 1);

    // Add more vectors after snapshot
    let timestamp2 = (1000 + timestamp1.wallclock()).into();
    index.add(node3, &[0.0, 0.0, 1.0, 0.0], timestamp2)?;

    // Query at snapshot time (should not include node3)
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 10, timestamp1)?;

    // Should only find nodes from the snapshot
    assert!(!results.is_empty()); // Ensure we actually found something
    assert!(results.len() <= 2);
    assert!(results.iter().all(|(id, _)| *id == node1 || *id == node2));

    Ok(())
}

#[test]
fn test_time_range_vector_query() -> Result<()> {
    let index = create_test_index()?;

    let base_time: Timestamp = 1_000_000.into();

    // Create multiple snapshots with vectors
    for i in 0..3 {
        let node_id = NodeId::new(i).unwrap();
        let timestamp = ((i as i64 * 1000) + base_time.wallclock()).into();
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;

        // Use deterministic timestamps to avoid CI flakiness
        index.on_transaction_at(timestamp)?;
        index.on_transaction_at(timestamp)?;
    }

    assert!(index.snapshot_count() >= 2);

    // Query across time range
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::between(base_time, (3000 + base_time.wallclock()).into()).unwrap();
    let results = index.find_similar_in_range(&query, 5, time_range)?;

    // Should have results for multiple snapshots
    assert!(!results.is_empty());

    // Each result should have a timestamp and similar nodes
    for (timestamp, similar_nodes) in results {
        assert!(timestamp >= base_time);
        assert!(!similar_nodes.is_empty());
    }

    Ok(())
}

#[test]
fn test_snapshot_pruning_with_keep_n() -> Result<()> {
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(3),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(hnsw_config),
    };
    let index = TemporalVectorIndex::new(config)?;

    // Create 5 snapshots
    for i in 0..5 {
        let node_id = NodeId::new(i).unwrap();
        let timestamp = ((i * 1000) as i64).into();
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
        index.on_transaction_at(timestamp)?;
    }

    assert_eq!(index.snapshot_count(), 5);

    // Prune to keep only 3 most recent
    let removed = index.prune_snapshots()?;
    assert_eq!(removed, 2);
    assert_eq!(index.snapshot_count(), 3);

    Ok(())
}

#[test]
fn test_snapshot_pruning_with_keep_duration() -> Result<()> {
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepDuration(Duration::from_secs(10)),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(hnsw_config),
    };
    let index = TemporalVectorIndex::new(config)?;

    // Create snapshots (all recent)
    let now = time::now();
    for i in 0..3 {
        let node_id = NodeId::new(i).unwrap();
        let timestamp = (now.wallclock() + (i * 1000) as i64).into();
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
        index.on_transaction_at(timestamp)?;
    }

    let count_before = index.snapshot_count();

    // Prune with duration policy (all snapshots are recent, so none should be removed)
    let removed = index.prune_snapshots()?;
    assert_eq!(removed, 0);
    assert_eq!(index.snapshot_count(), count_before);

    Ok(())
}

#[test]
fn test_snapshot_info_retrieval() -> Result<()> {
    let index = create_test_index()?;

    // Create a snapshot
    let t = 1000.into();
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], t)?;
    index.on_transaction_at(t)?;
    index.on_transaction_at(t)?;

    let info = index.get_snapshot_info()?;
    assert_eq!(info.len(), 1);

    let snapshot_info = &info[0];
    assert_eq!(snapshot_info.snapshot_id, 0);
    assert!(snapshot_info.timestamp.wallclock() > 0);
    assert!(snapshot_info.vector_count > 0);
    assert!(snapshot_info.size_bytes > 0);

    Ok(())
}

#[test]
fn test_temporal_vector_with_different_strategies() -> Result<()> {
    // Test TimeInterval strategy
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TimeInterval(1), // 1 second
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(hnsw_config.clone()),
    };

    let base_time = 1_000_000_000; // 1 second in microseconds
    let time_interval_index = TemporalVectorIndex::new_at(config, base_time.into())?;

    time_interval_index.add(
        NodeId::new(1).unwrap(),
        &[1.0, 0.0, 0.0, 0.0],
        base_time.into(),
    )?;

    // Add another vector 2 seconds later (should trigger snapshot)
    let later_time = base_time + 2_000_000; // 2 seconds later
    time_interval_index.add(
        NodeId::new(2).unwrap(),
        &[0.0, 1.0, 0.0, 0.0],
        later_time.into(),
    )?;
    time_interval_index.on_transaction_at(later_time.into())?;

    assert_eq!(time_interval_index.snapshot_count(), 1);

    // Test ChangeThreshold strategy
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::ChangeThreshold(0.5), // 50% changed
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(hnsw_config),
    };
    let change_threshold_index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Add 4 vectors (initial)
    for i in 0..4 {
        change_threshold_index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }

    // Change 2 vectors (50% change should trigger snapshot)
    change_threshold_index.add(NodeId::new(0).unwrap(), &[10.0, 0.0, 0.0, 0.0], 2000.into())?;
    change_threshold_index.add(NodeId::new(1).unwrap(), &[11.0, 0.0, 0.0, 0.0], 2000.into())?;
    change_threshold_index.on_transaction_at(2000.into())?;

    assert!(change_threshold_index.snapshot_count() >= 1);

    Ok(())
}

#[test]
fn test_empty_index_queries() -> Result<()> {
    let index = create_test_index()?;

    // Query on empty index should return empty results or error gracefully
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let timestamp: Timestamp = 1_000_000.into();

    // No snapshot exists, so should return error or empty
    let result = index.find_similar_as_of(&query, 10, timestamp);
    assert!(result.is_err() || result.unwrap().is_empty());

    Ok(())
}

#[test]
fn test_concurrent_snapshot_creation() -> Result<()> {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(create_test_index()?);

    // Add vectors from multiple threads
    let mut handles = vec![];
    for i in 0..4 {
        let index_clone = Arc::clone(&index);
        let handle = thread::spawn(move || -> Result<()> {
            let node_id = NodeId::new(i).unwrap();
            index_clone.add(
                node_id,
                &[i as f32, 0.0, 0.0, 0.0],
                ((i * 1000) as i64).into(),
            )?;
            Ok(())
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap()?;
    }

    // Trigger snapshots
    let now: Timestamp = 1_000_000.into();
    index.on_transaction_at(now)?;
    index.on_transaction_at(now)?;

    // Should have created at least one snapshot
    assert!(index.snapshot_count() > 0);

    Ok(())
}

#[test]
fn test_vector_dimension_validation() -> Result<()> {
    let index = create_test_index()?;

    let node_id = NodeId::new(1).unwrap();
    let wrong_dims = vec![1.0, 2.0]; // 2 dimensions, but index expects 4

    // Should fail with dimension mismatch
    let result = index.add(node_id, &wrong_dims, 1000.into());
    assert!(result.is_err());

    Ok(())
}

// ============================================================================
// Semantic Evolution and Drift Tracking Integration Tests (VS-045)
// ============================================================================

#[test]
fn test_semantic_evolution_end_to_end() -> Result<()> {
    let index = create_test_index()?;

    let node_id = NodeId::new(100).unwrap();
    let base_time: Timestamp = 1_000_000.into();

    // Create a timeline of vector changes
    let vectors = [
        vec![1.0, 0.0, 0.0, 0.0],
        vec![0.9, 0.1, 0.0, 0.0],
        vec![0.7, 0.3, 0.0, 0.0],
        vec![0.5, 0.5, 0.0, 0.0],
    ];

    for (i, vector) in vectors.iter().enumerate() {
        let timestamp = ((i as i64 * 1000) + base_time.wallclock()).into();
        index.add(node_id, vector, timestamp)?;
        index.on_transaction_at(timestamp)?;
        index.on_transaction_at(timestamp)?; // Trigger snapshot with interval=2
    }

    // Get semantic evolution
    // Use a very wide time range to capture all snapshots (which use real timestamps)
    let time_range = TimeRange::between(0.into(), i64::MAX.into()).unwrap();
    let evolution = index.semantic_evolution(node_id, time_range)?;

    // Verify we captured all changes
    // Note: With transaction_interval=2, we should get a snapshot after every 2 transactions
    // 4 adds * 2 on_transaction calls = 8 transactions = 4 snapshots
    assert_eq!(evolution.len(), 4);

    // Verify vectors match
    for (i, (_, vector)) in evolution.iter().enumerate() {
        let expected = &vectors[i];
        let actual: &[f32] = vector.as_ref();
        assert_eq!(actual, expected.as_slice());
    }

    Ok(())
}

#[test]
fn test_track_semantic_drift_over_time() -> Result<()> {
    let index = create_test_index()?;

    let node_id = NodeId::new(200).unwrap();
    let base_time: Timestamp = 1_000_000.into();

    // Create vectors that progressively drift from [1,0,0,0]
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], base_time)?;
    index.on_transaction_at(base_time)?;
    index.on_transaction_at(base_time)?;

    let t1 = (1000 + base_time.wallclock()).into();
    index.add(node_id, &[0.8, 0.2, 0.0, 0.0], t1)?;
    index.on_transaction_at(t1)?;
    index.on_transaction_at(t1)?;

    let t2 = (2000 + base_time.wallclock()).into();
    index.add(node_id, &[0.5, 0.5, 0.0, 0.0], t2)?;
    index.on_transaction_at(t2)?;
    index.on_transaction_at(t2)?;

    let t3 = (3000 + base_time.wallclock()).into();
    index.add(node_id, &[0.0, 1.0, 0.0, 0.0], t3)?;
    index.on_transaction_at(t3)?;
    index.on_transaction_at(t3)?;

    // Track drift from original vector
    let reference = vec![1.0, 0.0, 0.0, 0.0];
    // Use wide time range to capture all snapshots
    let time_range = TimeRange::between(0.into(), i64::MAX.into()).unwrap();
    let drift = index.track_semantic_drift(node_id, &reference, time_range)?;

    // Should have 4 measurements
    assert_eq!(drift.len(), 4);

    // Verify drift increases over time (similarity decreases)
    assert!(drift[0].1 > drift[1].1); // First should be more similar
    assert!(drift[1].1 > drift[2].1); // Progressive drift
    assert!(drift[2].1 > drift[3].1); // Maximum drift at end

    Ok(())
}

#[test]
fn test_calculate_consecutive_drift_end_to_end() -> Result<()> {
    let index = create_test_index()?;

    let node_id = NodeId::new(300).unwrap();
    let base_time: Timestamp = 1_000_000.into();

    // Create a timeline: stable -> change -> stable -> big change
    let vectors = [
        vec![1.0, 0.0, 0.0, 0.0],
        vec![1.0, 0.0, 0.0, 0.0], // Same as previous (no drift)
        vec![0.9, 0.1, 0.0, 0.0], // Small drift
        vec![0.0, 1.0, 0.0, 0.0], // Large drift
    ];

    for (i, vector) in vectors.iter().enumerate() {
        let timestamp = ((i as i64 * 1000) + base_time.wallclock()).into();
        index.add(node_id, vector, timestamp)?;
        index.on_transaction_at(timestamp)?;
        index.on_transaction_at(timestamp)?;
    }

    // Calculate consecutive drift
    // Use wide time range to capture all snapshots
    let time_range = TimeRange::between(0.into(), i64::MAX.into()).unwrap();
    let drift = index.calculate_consecutive_drift(node_id, time_range)?;

    // Should have 3 drift measurements (4 vectors -> 3 pairs)
    assert_eq!(drift.len(), 3);

    // First drift should be very small (identical vectors)
    assert!(drift[0].1 < 0.001);

    // Second drift should be small but non-zero
    assert!(drift[1].1 > 0.0 && drift[1].1 < 0.2);

    // Third drift should be large (orthogonal vectors)
    assert!(drift[2].1 > 0.8);

    Ok(())
}

#[test]
fn test_semantic_evolution_with_gaps() -> Result<()> {
    let index = create_test_index()?;

    let node1 = NodeId::new(400).unwrap();
    let node2 = NodeId::new(401).unwrap();
    let base_time: Timestamp = 1_000_000.into();

    // Add node1 at times 1000 and 3000
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], base_time)?;
    index.on_transaction_at(base_time)?;
    index.on_transaction_at(base_time)?;

    // Add node2 at time 2000 (different node)
    let t1 = (1000 + base_time.wallclock()).into();
    index.add(node2, &[0.0, 1.0, 0.0, 0.0], t1)?;
    index.on_transaction_at(t1)?;
    index.on_transaction_at(t1)?;

    // Add node1 again at time 3000
    let t2 = (2000 + base_time.wallclock()).into();
    index.add(node1, &[0.0, 0.0, 1.0, 0.0], t2)?;
    index.on_transaction_at(t2)?;
    index.on_transaction_at(t2)?;

    // Get evolution for node1
    // Use wide time range to capture all snapshots
    let time_range = TimeRange::between(0.into(), i64::MAX.into()).unwrap();
    let evolution = index.semantic_evolution(node1, time_range)?;

    // Node1 appears in all 3 snapshots:
    // 1. Initial add of node1 -> snapshot has {node1}
    // 2. Add node2 -> snapshot still has {node1, node2} (node1 wasn't removed)
    // 3. Update node1 -> snapshot has {node1 (updated), node2}
    assert_eq!(evolution.len(), 3);
    assert_eq!(evolution[0].1.as_ref(), &[1.0, 0.0, 0.0, 0.0]); // Initial
    assert_eq!(evolution[1].1.as_ref(), &[1.0, 0.0, 0.0, 0.0]); // Same (not removed)
    assert_eq!(evolution[2].1.as_ref(), &[0.0, 0.0, 1.0, 0.0]); // Updated

    Ok(())
}

#[test]
fn test_empty_evolution_for_nonexistent_node() -> Result<()> {
    let index = create_test_index()?;

    let base_time: Timestamp = 1_000_000.into();

    // Add some other node
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], base_time)?;
    index.on_transaction_at(base_time)?;
    index.on_transaction_at(base_time)?;

    // Query for non-existent node
    let nonexistent = NodeId::new(999).unwrap();
    let time_range = TimeRange::between(base_time, (1000 + base_time.wallclock()).into()).unwrap();
    let evolution = index.semantic_evolution(nonexistent, time_range)?;

    // Should return empty, not error
    assert_eq!(evolution.len(), 0);

    Ok(())
}

#[test]
fn test_drift_calculation_with_normalized_vectors() -> Result<()> {
    use gallifreydb::core::vector::normalize;

    let index = create_test_index()?;

    let node_id = NodeId::new(500).unwrap();
    let base_time: Timestamp = 1_000_000.into();

    // Use normalized vectors for more predictable cosine similarity
    let v1 = normalize(&[1.0, 0.0, 0.0, 0.0]);
    let v2 = normalize(&[0.707, 0.707, 0.0, 0.0]); // 45 degrees
    let v3 = normalize(&[0.0, 1.0, 0.0, 0.0]); // 90 degrees

    index.add(node_id, &v1, base_time)?;
    index.on_transaction_at(base_time)?;
    index.on_transaction_at(base_time)?;

    let t1 = (1000 + base_time.wallclock()).into();
    index.add(node_id, &v2, t1)?;
    index.on_transaction_at(t1)?;
    index.on_transaction_at(t1)?;

    let t2 = (2000 + base_time.wallclock()).into();
    index.add(node_id, &v3, t2)?;
    index.on_transaction_at(t2)?;
    index.on_transaction_at(t2)?;

    // Calculate consecutive drift
    // Use wide time range to capture all snapshots
    let time_range = TimeRange::between(0.into(), i64::MAX.into()).unwrap();
    let drift = index.calculate_consecutive_drift(node_id, time_range)?;

    // Should have 2 drift measurements
    assert_eq!(drift.len(), 2);

    // Drift from v1 to v2 is a 45-degree rotation, and so is v2 to v3.
    // Therefore, the drift values should be approximately equal.
    // drift = 1.0 - cos(45deg) = 1.0 - 1/sqrt(2) ≈ 0.2929
    let expected_drift = 1.0 - (1.0 / std::f32::consts::SQRT_2);
    assert!((drift[0].1 - expected_drift).abs() < 1e-4);
    assert!((drift[1].1 - expected_drift).abs() < 1e-4);

    Ok(())
}
