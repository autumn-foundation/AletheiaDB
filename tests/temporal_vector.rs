//! Integration tests for temporal vector support (Phase 3).
//!
//! Tests end-to-end temporal vector functionality including:
//! - Temporal vector index creation and configuration
//! - Snapshot creation with different strategies
//! - Point-in-time vector queries
//! - Time-range vector queries
//! - Snapshot pruning with retention policies

use gallifreydb::core::id::NodeId;
use gallifreydb::core::temporal::{TimeRange, time};
use gallifreydb::index::vector::temporal::{
    RetentionPolicy, SnapshotStrategy, TemporalVectorConfig, TemporalVectorIndex,
};
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::utils::Result;
use std::time::Duration;

/// Helper to create a test temporal vector index with default config
fn create_test_index() -> Result<TemporalVectorIndex> {
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 20,  // Conservative default, see issue #230
        hnsw_config,
    };
    TemporalVectorIndex::new(config)
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

    index.add(node1, &vec1, 1000)?;
    index.add(node2, &vec2, 2000)?;

    assert_eq!(index.snapshot_count(), 0);

    // Trigger snapshot (transaction interval = 2)
    index.on_transaction()?;
    assert_eq!(index.snapshot_count(), 0); // Not yet

    index.on_transaction()?;
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

    let timestamp1 = time::now();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], timestamp1)?;
    index.add(node2, &[0.9, 0.1, 0.0, 0.0], timestamp1)?;

    // Create snapshot
    index.on_transaction()?;
    index.on_transaction()?;
    assert_eq!(index.snapshot_count(), 1);

    // Add more vectors after snapshot
    let timestamp2 = timestamp1 + 1000;
    index.add(node3, &[0.0, 0.0, 1.0, 0.0], timestamp2)?;

    // Query at snapshot time (should not include node3)
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 10, timestamp1)?;

    // Should only find nodes from the snapshot
    assert!(results.len() <= 2);
    assert!(results.iter().all(|(id, _)| *id == node1 || *id == node2));

    Ok(())
}

#[test]
fn test_time_range_vector_query() -> Result<()> {
    let index = create_test_index()?;

    let base_time = time::now();

    // Create multiple snapshots with vectors
    for i in 0..3 {
        let node_id = NodeId::new(i).unwrap();
        let timestamp = base_time + (i as i64 * 1000);
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;

        index.on_transaction()?;
        index.on_transaction()?;
    }

    assert!(index.snapshot_count() >= 2);

    // Query across time range
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::between(base_time, base_time + 3000);
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
        max_snapshots: 100,  // High limit to test retention policy enforcement
        hnsw_config,
    };
    let index = TemporalVectorIndex::new(config)?;

    // Create 5 snapshots
    for i in 0..5 {
        let node_id = NodeId::new(i).unwrap();
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], (i * 1000) as i64)?;
        index.on_transaction()?;
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
        max_snapshots: 100,  // High limit to test retention policy enforcement
        hnsw_config,
    };
    let index = TemporalVectorIndex::new(config)?;

    // Create snapshots (all recent)
    for i in 0..3 {
        let node_id = NodeId::new(i).unwrap();
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], (i * 1000) as i64)?;
        index.on_transaction()?;
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
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
    index.on_transaction()?;
    index.on_transaction()?;

    let info = index.get_snapshot_info();
    assert_eq!(info.len(), 1);

    let snapshot_info = &info[0];
    assert_eq!(snapshot_info.snapshot_id, 0);
    assert!(snapshot_info.timestamp > 0);
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
        max_snapshots: 20,  // Conservative default, see issue #230
        hnsw_config: hnsw_config.clone(),
    };

    let base_time = 1_000_000_000; // 1 second in microseconds
    let time_interval_index = TemporalVectorIndex::new_at(config, base_time)?;

    time_interval_index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], base_time)?;

    // Add another vector 2 seconds later (should trigger snapshot)
    let later_time = base_time + 2_000_000; // 2 seconds later
    time_interval_index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], later_time)?;
    time_interval_index.on_transaction_at(later_time)?;

    assert_eq!(time_interval_index.snapshot_count(), 1);

    // Test ChangeThreshold strategy
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::ChangeThreshold(0.5), // 50% changed
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 20,  // Conservative default, see issue #230
        hnsw_config,
    };
    let change_threshold_index = TemporalVectorIndex::new_at(config, 1000)?;

    // Add 4 vectors (initial)
    for i in 0..4 {
        change_threshold_index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
    }

    // Change 2 vectors (50% change should trigger snapshot)
    change_threshold_index.add(NodeId::new(0).unwrap(), &[10.0, 0.0, 0.0, 0.0], 2000)?;
    change_threshold_index.add(NodeId::new(1).unwrap(), &[11.0, 0.0, 0.0, 0.0], 2000)?;
    change_threshold_index.on_transaction_at(2000)?;

    assert!(change_threshold_index.snapshot_count() >= 1);

    Ok(())
}

#[test]
fn test_empty_index_queries() -> Result<()> {
    let index = create_test_index()?;

    // Query on empty index should return empty results or error gracefully
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let timestamp = time::now();

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
            index_clone.add(node_id, &[i as f32, 0.0, 0.0, 0.0], (i * 1000) as i64)?;
            Ok(())
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap()?;
    }

    // Trigger snapshots
    index.on_transaction()?;
    index.on_transaction()?;

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
    let result = index.add(node_id, &wrong_dims, 1000);
    assert!(result.is_err());

    Ok(())
}
