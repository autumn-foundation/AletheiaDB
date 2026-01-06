use gallifreydb::core::id::NodeId;
use gallifreydb::core::temporal::TimeRange;
use gallifreydb::index::VectorIndex;
use gallifreydb::index::vector::temporal::*;
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::utils::Result;

fn create_test_index() -> Result<TemporalVectorIndex> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
        retention_policy: RetentionPolicy::KeepN(20),
        max_snapshots: 20,  // Conservative default, see issue #230
        hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
    };
    TemporalVectorIndex::new(config)
}

fn create_test_index_with_snapshots() -> Result<TemporalVectorIndex> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(2), // Create snapshot every 2 transactions
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 10,
        hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
    };
    TemporalVectorIndex::new(config)
}

#[test]
fn integration_test_add_vector() -> Result<()> {
    let index = create_test_index()?;
    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    let timestamp = 1000000;
    index.add(node1, &vec1, timestamp)?;
    assert_eq!(index.current_index().len(), 1);
    Ok(())
}

#[test]
fn integration_test_multiple_adds() -> Result<()> {
    let index = create_test_index()?;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    let vec2 = vec![0.0, 1.0, 0.0, 0.0];
    let timestamp = 1000000;

    index.add(node1, &vec1, timestamp)?;
    index.add(node2, &vec2, timestamp + 100)?;

    assert_eq!(index.current_index().len(), 2);

    Ok(())
}

#[test]
fn test_find_similar_as_of() -> Result<()> {
    let index = create_test_index_with_snapshots()?;

    // Add vectors at different timestamps
    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();

    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    let vec2 = vec![0.9, 0.1, 0.0, 0.0]; // Similar to vec1
    let vec3 = vec![0.0, 0.0, 1.0, 0.0]; // Different

    // Add at timestamp 1000
    index.add(node1, &vec1, 1000)?;
    index.on_transaction_at(1000)?;

    // Add at timestamp 2000
    index.add(node2, &vec2, 2000)?;
    index.on_transaction_at(2000)?; // This should create a snapshot

    // Add at timestamp 3000
    index.add(node3, &vec3, 3000)?;
    index.on_transaction_at(3000)?;

    // Query as of timestamp 2500 (should find node1 and node2, not node3)
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 5, 2500)?;

    // Should have found 2 vectors
    assert!(results.len() >= 2, "Should find at least 2 similar vectors");

    // Verify node3 is not in results (it was added after timestamp 2500)
    assert!(
        !results.iter().any(|(id, _)| *id == node3),
        "Should not find node3"
    );

    Ok(())
}

#[test]
fn test_find_similar_in_range() -> Result<()> {
    let index = create_test_index_with_snapshots()?;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    let vec2 = vec![0.9, 0.1, 0.0, 0.0];

    index.add(node1, &vec1, 1000)?;
    index.on_transaction_at(1000)?;
    index.add(node2, &vec2, 2000)?;
    index.on_transaction_at(2000)?;
    index.on_transaction_at(3000)?; // Create another snapshot

    // Query range from 1500 to 2500
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(1500, 2500);
    let results = index.find_similar_in_range(&query, 5, time_range)?;

    // Should have results for timestamps in range
    assert!(!results.is_empty(), "Should have results in time range");

    Ok(())
}

#[test]
fn test_create_manual_snapshot() -> Result<()> {
    let index = create_test_index()?;

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    index.add(node1, &vec1, 1000)?;

    // Initial snapshot count
    let count_before = index.snapshot_count();

    // Create manual snapshot
    index.create_manual_snapshot()?;

    // Verify snapshot was created
    let count_after = index.snapshot_count();
    assert_eq!(
        count_after,
        count_before + 1,
        "Should have created one snapshot"
    );

    Ok(())
}

#[test]
fn test_prune_snapshots() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(2), // Keep only 2 snapshots
        max_snapshots: 10,
        hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
    };
    let index = TemporalVectorIndex::new(config)?;

    // Create multiple snapshots
    for i in 1..=5 {
        let node = NodeId::new(i).unwrap();
        let vec = vec![i as f32, 0.0, 0.0, 0.0];
        index.add(node, &vec, (i * 1000) as i64)?;
        index.on_transaction_at((i * 1000) as i64)?; // Create snapshot
    }

    // Prune snapshots (should keep only 2 most recent)
    let pruned = index.prune_snapshots()?;

    // Should have pruned some snapshots
    assert!(pruned > 0, "Should have pruned snapshots");
    assert!(
        index.snapshot_count() <= 2,
        "Should keep at most 2 snapshots"
    );

    Ok(())
}

#[test]
fn test_get_snapshot_info() -> Result<()> {
    let index = create_test_index_with_snapshots()?;

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    index.add(node1, &vec1, 1000)?;
    index.on_transaction_at(1000)?;

    index.create_manual_snapshot()?;

    let info = index.get_snapshot_info();
    assert!(!info.is_empty(), "Should have snapshot info");

    Ok(())
}

#[test]
fn test_dimensions_and_metric() -> Result<()> {
    let index = create_test_index()?;

    assert_eq!(index.dimensions(), 4, "Should have 4 dimensions");
    assert_eq!(
        index.distance_metric(),
        DistanceMetric::Cosine,
        "Should use Cosine metric"
    );

    Ok(())
}

#[test]
fn test_config_builders() -> Result<()> {
    let hnsw_config = HnswConfig::new(128, DistanceMetric::Euclidean);

    // Test default_with_hnsw
    let config1 = TemporalVectorConfig::default_with_hnsw(hnsw_config.clone());
    assert!(matches!(
        config1.snapshot_strategy,
        SnapshotStrategy::TransactionInterval(_)
    ));

    // Test with_time_interval
    let config2 = TemporalVectorConfig::with_time_interval(hnsw_config.clone(), 3600);
    assert!(matches!(
        config2.snapshot_strategy,
        SnapshotStrategy::TimeInterval(_)
    ));

    // Test with_change_threshold
    let config3 = TemporalVectorConfig::with_change_threshold(hnsw_config, 0.1);
    assert!(matches!(
        config3.snapshot_strategy,
        SnapshotStrategy::ChangeThreshold(_)
    ));

    Ok(())
}
