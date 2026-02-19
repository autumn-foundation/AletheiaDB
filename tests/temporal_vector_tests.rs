use aletheiadb::core::id::NodeId;
use aletheiadb::core::temporal::TimeRange;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::temporal::*;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::utils::Result;

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
        // Compare timestamps
        assert_eq!(
            results1[i].0, results2[i].0,
            "Timestamp mismatch for results2 at index {}",
            i
        );
        assert_eq!(
            results1[i].0, results3[i].0,
            "Timestamp mismatch for results3 at index {}",
            i
        );

        let r1 = &results1[i].1;
        let r2 = &results2[i].1;
        let r3 = &results3[i].1;

        // Compare result counts
        assert_eq!(
            r1.len(),
            r2.len(),
            "Result count mismatch for results2 at index {}",
            i
        );
        assert_eq!(
            r1.len(),
            r3.len(),
            "Result count mismatch for results3 at index {}",
            i
        );

        // Compare individual results (NodeId and score)
        for j in 0..r1.len() {
            assert_eq!(
                r1[j].0, r2[j].0,
                "NodeId mismatch for results2 at index {}/{}",
                i, j
            );
            assert!(
                (r1[j].1 - r2[j].1).abs() < 1e-6,
                "Score mismatch for results2 at index {}/{}: {} vs {}",
                i,
                j,
                r1[j].1,
                r2[j].1
            );

            assert_eq!(
                r1[j].0, r3[j].0,
                "NodeId mismatch for results3 at index {}/{}",
                i, j
            );
            assert!(
                (r1[j].1 - r3[j].1).abs() < 1e-6,
                "Score mismatch for results3 at index {}/{}: {} vs {}",
                i,
                j,
                r1[j].1,
                r3[j].1
            );
        }
    }

    Ok(())
}

// ============================================================================
// RED PHASE TESTS for Issue #233: Batch API and Single Lock Optimization
// ============================================================================

/// Test add_batch() API - RED PHASE: This will fail until add_batch() is implemented
#[test]
fn test_add_batch_basic() -> Result<()> {
    let index = create_test_index()?;

    // Prepare batch of vectors to add
    let batch = vec![
        (
            NodeId::new(1).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0],
            1000.into(),
        ),
        (
            NodeId::new(2).unwrap(),
            vec![0.0, 1.0, 0.0, 0.0],
            1100.into(),
        ),
        (
            NodeId::new(3).unwrap(),
            vec![0.0, 0.0, 1.0, 0.0],
            1200.into(),
        ),
    ];

    // Add batch - this method doesn't exist yet (RED PHASE)
    index.add_batch(&batch)?;

    // Verify all vectors were added
    assert_eq!(
        index.current_index().len(),
        3,
        "All 3 vectors should be added"
    );

    Ok(())
}

/// Test add_batch() with empty batch
#[test]
fn test_add_batch_empty() -> Result<()> {
    let index = create_test_index()?;

    let batch: Vec<(NodeId, Vec<f32>, _)> = vec![];

    // Should handle empty batch gracefully
    index.add_batch(&batch)?;

    assert_eq!(index.current_index().len(), 0);

    Ok(())
}

/// Test add_batch() maintains correctness - vectors are searchable
#[test]
fn test_add_batch_correctness() -> Result<()> {
    let index = create_test_index()?;

    // Add batch of vectors
    let batch = vec![
        (
            NodeId::new(1).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0],
            1000.into(),
        ),
        (
            NodeId::new(2).unwrap(),
            vec![0.9, 0.1, 0.0, 0.0],
            1100.into(),
        ),
        (
            NodeId::new(3).unwrap(),
            vec![0.0, 0.0, 1.0, 0.0],
            1200.into(),
        ),
    ];

    index.add_batch(&batch)?;

    // Verify vectors can be found via similarity search
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.current_index().search(&query, 2)?;

    assert_eq!(results.len(), 2, "Should find 2 most similar vectors");

    // The first two vectors should be most similar to query
    assert!(
        results.iter().any(|(id, _)| *id == NodeId::new(1).unwrap()),
        "Node 1 should be in results"
    );
    assert!(
        results.iter().any(|(id, _)| *id == NodeId::new(2).unwrap()),
        "Node 2 should be in results"
    );

    Ok(())
}

/// Test add_batch() with invalid vectors (NaN)
#[test]
fn test_add_batch_nan_validation() -> Result<()> {
    let index = create_test_index()?;

    let batch = vec![
        (
            NodeId::new(1).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0],
            1000.into(),
        ),
        (
            NodeId::new(2).unwrap(),
            vec![f32::NAN, 1.0, 0.0, 0.0],
            1100.into(),
        ), // Invalid
        (
            NodeId::new(3).unwrap(),
            vec![0.0, 0.0, 1.0, 0.0],
            1200.into(),
        ),
    ];

    // Should fail due to NaN in batch
    let result = index.add_batch(&batch);
    assert!(result.is_err(), "Should reject batch with NaN values");

    Ok(())
}

/// Test add_batch() with invalid vectors (Infinity)
#[test]
fn test_add_batch_infinity_validation() -> Result<()> {
    let index = create_test_index()?;

    let batch = vec![
        (
            NodeId::new(1).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0],
            1000.into(),
        ),
        (
            NodeId::new(2).unwrap(),
            vec![f32::INFINITY, 1.0, 0.0, 0.0],
            1100.into(),
        ), // Invalid
    ];

    // Should fail due to Infinity in batch
    let result = index.add_batch(&batch);
    assert!(result.is_err(), "Should reject batch with Infinity values");

    Ok(())
}

/// Test add_batch() produces same results as multiple add() calls
#[test]
fn test_add_batch_equivalence() -> Result<()> {
    let index1 = create_test_index()?;
    let index2 = create_test_index()?;

    let vectors = vec![
        (
            NodeId::new(1).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0],
            1000.into(),
        ),
        (
            NodeId::new(2).unwrap(),
            vec![0.0, 1.0, 0.0, 0.0],
            1100.into(),
        ),
        (
            NodeId::new(3).unwrap(),
            vec![0.0, 0.0, 1.0, 0.0],
            1200.into(),
        ),
        (
            NodeId::new(4).unwrap(),
            vec![0.5, 0.5, 0.0, 0.0],
            1300.into(),
        ),
    ];

    // Add using batch API
    index1.add_batch(&vectors)?;

    // Add individually
    for (id, vec, ts) in &vectors {
        index2.add(*id, vec, *ts)?;
    }

    // Both should have same size
    assert_eq!(index1.current_index().len(), index2.current_index().len());

    // Both should produce same search results
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results1 = index1.current_index().search(&query, 3)?;
    let results2 = index2.current_index().search(&query, 3)?;

    assert_eq!(
        results1.len(),
        results2.len(),
        "Should return same number of results"
    );

    // Results should have same node IDs (order may vary due to tie-breaking)
    let ids1: std::collections::HashSet<_> = results1.iter().map(|(id, _)| id).collect();
    let ids2: std::collections::HashSet<_> = results2.iter().map(|(id, _)| id).collect();
    assert_eq!(ids1, ids2, "Should return same nodes");

    Ok(())
}

/// Test add_batch() with large batch size
#[test]
fn test_add_batch_large() -> Result<()> {
    let index = create_test_index()?;

    // Create large batch
    let batch: Vec<_> = (0..1000)
        .map(|i| {
            let id = NodeId::new(i).unwrap();
            let vec = vec![
                (i as f32) / 1000.0,
                ((i + 1) as f32) / 1000.0,
                ((i + 2) as f32) / 1000.0,
                ((i + 3) as f32) / 1000.0,
            ];
            let ts = ((i * 100) as i64).into();
            (id, vec, ts)
        })
        .collect();

    // Should handle large batch
    index.add_batch(&batch)?;

    assert_eq!(
        index.current_index().len(),
        1000,
        "All 1000 vectors should be added"
    );

    Ok(())
}

// ============================================================================
// RED PHASE TESTS for Issue #233 Critical Bugs - Atomicity & Ordering
// ============================================================================

/// Test that add_batch() maintains atomicity - if one vector fails, none should be added
/// This test exposes the atomicity bug where Phase 2 commits state before Phase 3 validates HNSW
#[test]
fn test_add_batch_atomicity_on_dimension_mismatch() -> Result<()> {
    let index = create_test_index()?; // 4 dimensions

    // Batch with last vector having wrong dimensions (will fail in HNSW)
    let batch = vec![
        (
            NodeId::new(1).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0],
            1000.into(),
        ),
        (
            NodeId::new(2).unwrap(),
            vec![0.0, 1.0, 0.0, 0.0],
            1100.into(),
        ),
        // This vector has wrong dimensions - should cause ENTIRE batch to fail
        (
            NodeId::new(3).unwrap(),
            vec![0.0, 0.0, 1.0], // Only 3 dimensions instead of 4!
            1200.into(),
        ),
    ];

    // Batch should fail
    let result = index.add_batch(&batch);
    assert!(
        result.is_err(),
        "Batch should fail due to dimension mismatch"
    );

    // CRITICAL: No vectors should be in the index (atomicity)
    // If bug exists, first 2 vectors will be in current_state but not HNSW
    assert_eq!(
        index.current_index().len(),
        0,
        "No vectors should be added when batch fails"
    );

    // Also check memory stats to ensure current_state is empty
    let stats = index.memory_stats();
    assert_eq!(
        stats.current_vectors, 0,
        "current_state should have no vectors after failed batch"
    );

    Ok(())
}

/// Test that batch operations maintain consistency between HNSW and current_state
#[test]
fn test_add_batch_consistency() -> Result<()> {
    let index = create_test_index()?;

    // Add multiple batches
    let batch1 = vec![
        (
            NodeId::new(1).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0],
            1000.into(),
        ),
        (
            NodeId::new(2).unwrap(),
            vec![0.9, 0.1, 0.0, 0.0],
            1100.into(),
        ),
    ];
    index.add_batch(&batch1)?;

    // After successful batch, verify consistency
    assert_eq!(index.current_index().len(), 2, "HNSW should have 2 vectors");
    let stats = index.memory_stats();
    assert_eq!(
        stats.current_vectors, 2,
        "current_state should have 2 vectors"
    );

    // Add another batch
    let batch2 = vec![
        (
            NodeId::new(3).unwrap(),
            vec![0.0, 1.0, 0.0, 0.0],
            2000.into(),
        ),
        (
            NodeId::new(4).unwrap(),
            vec![0.0, 0.9, 0.1, 0.0],
            2100.into(),
        ),
    ];
    index.add_batch(&batch2)?;

    // Verify all 4 vectors are in both HNSW and current_state
    assert_eq!(index.current_index().len(), 4, "HNSW should have 4 vectors");
    let stats = index.memory_stats();
    assert_eq!(
        stats.current_vectors, 4,
        "current_state should have 4 vectors"
    );

    // Verify all vectors are searchable
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.current_index().search(&query, 4)?;
    assert_eq!(results.len(), 4, "Should find all 4 vectors via search");

    Ok(())
}

/// Test concurrent add_batch() calls don't cause data corruption
#[test]
fn test_concurrent_add_batch() -> Result<()> {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(create_test_index()?);

    // Spawn multiple threads doing batch adds
    let mut handles = vec![];
    for thread_id in 0..4 {
        let index_clone = Arc::clone(&index);
        let handle = thread::spawn(move || {
            let batch: Vec<_> = (0..25)
                .map(|i| {
                    let node_id = NodeId::new((thread_id * 25 + i) as u64).unwrap();
                    let vector = vec![(thread_id as f32) / 10.0, (i as f32) / 100.0, 0.0, 0.0];
                    let timestamp = ((thread_id * 25 + i) as i64 * 1000).into();
                    (node_id, vector, timestamp)
                })
                .collect();

            index_clone.add_batch(&batch)
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap()?;
    }

    // Verify all 100 vectors were added (4 threads * 25 vectors each)
    assert_eq!(
        index.current_index().len(),
        100,
        "All 100 vectors should be added"
    );

    let stats = index.memory_stats();
    assert_eq!(
        stats.current_vectors, 100,
        "current_state should have 100 vectors"
    );

    Ok(())
}

/// Test that remove() is atomic - if HNSW remove fails, state shouldn't change
#[test]
fn test_remove_on_nonexistent_node() -> Result<()> {
    let index = create_test_index()?;

    // Add a vector
    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    assert_eq!(index.current_index().len(), 1);

    // Try to remove a non-existent node
    let node2 = NodeId::new(2).unwrap();
    let _result = index.remove(node2, 2000.into());

    // Remove should fail (or succeed silently depending on implementation)
    // The important thing is: node1 should still be in both HNSW and current_state
    assert_eq!(
        index.current_index().len(),
        1,
        "Node1 should still be in HNSW"
    );

    let stats = index.memory_stats();
    assert_eq!(
        stats.current_vectors, 1,
        "Node1 should still be in current_state"
    );

    Ok(())
}
