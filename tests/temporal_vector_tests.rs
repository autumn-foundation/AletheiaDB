use gallifreydb::core::id::NodeId;
use gallifreydb::core::temporal::TimeRange;
use gallifreydb::index::VectorIndex;
use gallifreydb::index::vector::temporal::*;
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::utils::Result;

fn create_test_index() -> Result<TemporalVectorIndex> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    TemporalVectorIndex::new(config)
}

fn create_test_index_with_snapshots() -> Result<TemporalVectorIndex> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(2), // Create snapshot every 2 transactions
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 10,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    TemporalVectorIndex::new(config)
}

#[test]
fn integration_test_add_vector() -> Result<()> {
    let index = create_test_index()?;
    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    let timestamp = 1000000.into();
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
    let timestamp = 1000000.into();

    index.add(node1, &vec1, timestamp)?;
    index.add(node2, &vec2, (timestamp.wallclock() + 100).into())?;

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
    index.add(node1, &vec1, 1000.into())?;
    index.on_transaction_at(1000.into())?;

    // Add at timestamp 2000
    index.add(node2, &vec2, 2000.into())?;
    index.on_transaction_at(2000.into())?; // This should create a snapshot

    // Add at timestamp 3000
    index.add(node3, &vec3, 3000.into())?;
    index.on_transaction_at(3000.into())?;

    // Query as of timestamp 2500 (should find node1 and node2, not node3)
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 5, 2500.into())?;

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

    index.add(node1, &vec1, 1000.into())?;
    index.on_transaction_at(1000.into())?;
    index.add(node2, &vec2, 2000.into())?;
    index.on_transaction_at(2000.into())?;
    index.on_transaction_at(3000.into())?; // Create another snapshot

    // Query range from 1500 to 2500
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(1500.into(), 2500.into()).unwrap();
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
    index.add(node1, &vec1, 1000.into())?;

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
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    // Create multiple snapshots
    for i in 1..=5 {
        let node = NodeId::new(i).unwrap();
        let vec = vec![i as f32, 0.0, 0.0, 0.0];
        index.add(node, &vec, ((i * 1000) as i64).into())?;
        index.on_transaction_at(((i * 1000) as i64).into())?; // Create snapshot
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
    index.add(node1, &vec1, 1000.into())?;
    index.on_transaction_at(1000.into())?;

    index.create_manual_snapshot()?;

    let info = index.get_snapshot_info()?;
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

/// Test that find_similar_in_range results are sorted chronologically
#[test]
fn test_find_similar_in_range_chronological_order() -> Result<()> {
    let index = create_test_index_with_snapshots()?;

    // Create many snapshots
    for i in 0i64..20 {
        let node_id = NodeId::new(i as u64).unwrap();
        let vector = vec![1.0, 0.0, 0.0, 0.0];
        index.add(node_id, &vector, (i * 1000).into())?;
        index.on_transaction_at((i * 1000).into())?;
    }

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(0.into(), 20000.into()).unwrap();
    let results = index.find_similar_in_range(&query, 5, time_range)?;

    // Verify results are in chronological order
    for i in 1..results.len() {
        assert!(
            results[i - 1].0 <= results[i].0,
            "Results should be sorted chronologically, but found {:?} after {:?}",
            results[i].0,
            results[i - 1].0
        );
    }

    Ok(())
}

/// Test find_similar_in_range with many snapshots (parallelism test)
#[test]
fn test_find_similar_in_range_many_snapshots() -> Result<()> {
    let index = create_test_index_with_snapshots()?;

    // Create 25 snapshots (more than typical core count to test parallelism)
    for i in 0i64..25 {
        for j in 0i64..10 {
            let node_id = NodeId::new((i * 10 + j) as u64).unwrap();
            let vector = vec![1.0 / (i + 1) as f32, (j as f32) / 10.0, 0.0, 0.0];
            index.add(node_id, &vector, (i * 1000 + j * 10).into())?;
        }
        index.on_transaction_at((i * 1000).into())?;
    }

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(0.into(), 25000.into()).unwrap();
    let results = index.find_similar_in_range(&query, 5, time_range)?;

    // Should have results from multiple snapshots
    assert!(
        results.len() >= 10,
        "Should have results from multiple snapshots, got {}",
        results.len()
    );

    // Verify chronological order
    for i in 1..results.len() {
        assert!(
            results[i - 1].0 <= results[i].0,
            "Results must be chronologically ordered"
        );
    }

    // Verify each snapshot has results
    for (timestamp, snapshot_results) in &results {
        assert!(
            !snapshot_results.is_empty(),
            "Snapshot at timestamp {} should have results",
            timestamp.wallclock()
        );
        assert!(snapshot_results.len() <= 5, "Should respect k=5 limit");
    }

    Ok(())
}

/// Test find_similar_in_range with edge cases
#[test]
fn test_find_similar_in_range_edge_cases() -> Result<()> {
    let index = create_test_index_with_snapshots()?;

    // Add vectors and create snapshots at specific timestamps
    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();
    let vector = vec![1.0, 0.0, 0.0, 0.0];

    index.add(node1, &vector, 1000.into())?;
    index.on_transaction_at(1000.into())?;
    index.add(node2, &vector, 2000.into())?;
    index.on_transaction_at(2000.into())?;
    index.add(node3, &vector, 3000.into())?;
    index.on_transaction_at(3000.into())?;

    let query = vec![1.0, 0.0, 0.0, 0.0];

    // Test empty range (no snapshots in range)
    let empty_range = TimeRange::new(10000.into(), 11000.into()).unwrap();
    let empty_results = index.find_similar_in_range(&query, 5, empty_range)?;
    assert!(
        empty_results.is_empty(),
        "Should have no results for empty range"
    );

    // Test range with snapshots
    let range_with_snapshots = TimeRange::new(1500.into(), 2500.into()).unwrap();
    let results = index.find_similar_in_range(&query, 5, range_with_snapshots)?;
    assert!(
        !results.is_empty(),
        "Should have results when snapshots exist in range"
    );

    Ok(())
}

/// Test that parallel implementation produces deterministic results
#[test]
fn test_find_similar_in_range_deterministic() -> Result<()> {
    let index = create_test_index_with_snapshots()?;

    // Create snapshots with diverse vectors
    for i in 0i64..15 {
        for j in 0i64..20 {
            let node_id = NodeId::new((i * 20 + j) as u64).unwrap();
            let angle = (j as f32) * std::f32::consts::PI / 10.0;
            let vector = vec![angle.cos(), angle.sin(), (i as f32) / 15.0, 0.0];
            index.add(node_id, &vector, (i * 1000 + j * 10).into())?;
        }
        index.on_transaction_at((i * 1000).into())?;
    }

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(0.into(), 15000.into()).unwrap();

    // Run multiple times and verify results are identical
    let results1 = index.find_similar_in_range(&query, 10, time_range)?;
    let results2 = index.find_similar_in_range(&query, 10, time_range)?;
    let results3 = index.find_similar_in_range(&query, 10, time_range)?;

    assert_eq!(
        results1.len(),
        results2.len(),
        "Results should be deterministic (same length)"
    );
    assert_eq!(
        results1.len(),
        results3.len(),
        "Results should be deterministic (same length)"
    );

    // Compare each snapshot's results
    for i in 0..results1.len() {
        assert_eq!(
            results1[i].0, results2[i].0,
            "Timestamps should match at position {}",
            i
        );
        assert_eq!(
            results1[i].0, results3[i].0,
            "Timestamps should match at position {}",
            i
        );
        assert_eq!(
            results1[i].1.len(),
            results2[i].1.len(),
            "Result count should match at position {}",
            i
        );
        assert_eq!(
            results1[i].1.len(),
            results3[i].1.len(),
            "Result count should match at position {}",
            i
        );
    }

    Ok(())
}
