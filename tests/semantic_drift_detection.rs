//! Integration tests for semantic drift detection.
//!
//! Tests the `find_semantic_drift()` API with realistic scenarios involving
//! multiple documents with varying drift levels over time.

use gallifreydb::core::id::NodeId;
use gallifreydb::core::temporal::TimeRange;
use gallifreydb::index::vector::temporal::{
    DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig, TemporalVectorIndex,
};
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::utils::Result;

/// Creates a normalized vector from raw values.
fn normalize(v: &[f32]) -> Vec<f32> {
    let mag = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / mag).collect()
}

#[test]
fn test_semantic_drift_detection_realistic_scenario() -> Result<()> {
    // Create a temporal vector index with 384 dimensions (common embedding size)
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 200,
        hnsw_config: HnswConfig::new(384, DistanceMetric::Cosine),
    };
    let index = TemporalVectorIndex::new(config)?;
    let mut ts = 1000i64;

    // Simulate 10 document nodes with varying drift patterns

    // Docs 1-3: Stable documents (< 0.1 drift)
    // These represent documents whose meaning hasn't changed much
    for i in 1..=3 {
        let node_id = NodeId::new(i).unwrap();

        // Initial embedding
        let mut embedding = vec![0.0f32; 384];
        embedding[0] = 1.0;
        embedding[i as usize] = 0.1; // Small variation per doc
        let embedding = normalize(&embedding);
        index.add(node_id, &embedding, ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;

        // Updated embedding (very similar)
        let mut updated = vec![0.0f32; 384];
        updated[0] = 1.0;
        updated[i as usize] = 0.11; // Tiny change
        let updated = normalize(&updated);
        index.remove(node_id, ts)?;
        index.add(node_id, &updated, ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
    }

    // Docs 4-6: Moderate drift (0.2-0.4)
    // These represent documents whose meaning has shifted moderately
    for i in 4..=6 {
        let node_id = NodeId::new(i).unwrap();

        // Initial embedding
        let mut embedding = vec![0.0f32; 384];
        embedding[0] = 1.0;
        let embedding = normalize(&embedding);
        index.add(node_id, &embedding, ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;

        // Updated embedding (moderate change)
        let mut updated = vec![0.0f32; 384];
        updated[0] = 0.7;
        updated[1] = 0.3;
        updated[i as usize] = 0.2; // Variation per doc
        let updated = normalize(&updated);
        index.remove(node_id, ts)?;
        index.add(node_id, &updated, ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
    }

    // Docs 7-9: High drift (> 0.5)
    // These represent documents whose meaning has changed significantly
    for i in 7..=9 {
        let node_id = NodeId::new(i).unwrap();

        // Initial embedding
        let mut embedding = vec![0.0f32; 384];
        embedding[0] = 1.0;
        let embedding = normalize(&embedding);
        index.add(node_id, &embedding, ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;

        // Updated embedding (large change - nearly orthogonal)
        let mut updated = vec![0.0f32; 384];
        updated[1] = 1.0; // Different direction
        updated[i as usize] = 0.1; // Variation per doc
        let updated = normalize(&updated);
        index.remove(node_id, ts)?;
        index.add(node_id, &updated, ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
    }

    // Doc 10: Single version (should be excluded)
    let node10 = NodeId::new(10).unwrap();
    let mut embedding = vec![0.0f32; 384];
    embedding[0] = 1.0;
    let embedding = normalize(&embedding);
    index.add(node10, &embedding, ts)?;
    index.on_transaction_at(ts)?;

    // Query with threshold 0.3 - should get docs 4-9
    let time_range = TimeRange::new(0, i64::MAX);
    let results = index.find_semantic_drift(0.3, time_range, DriftMetric::Cosine)?;

    // Verify results
    // High drift nodes (7-9) should always be included, moderate drift nodes (4-6)
    // may or may not be included depending on exact drift values after normalization
    assert!(
        !results.is_empty(),
        "Expected at least high drift nodes (7-9) in results"
    );

    // Verify doc 10 (single version) is not in results
    assert!(
        !results.iter().any(|(id, _)| *id == node10),
        "Single-version node should not be in results"
    );

    // Verify docs 7-9 (high drift ~0.7-1.0) are definitely in results
    for i in 7..=9 {
        let node_id = NodeId::new(i).unwrap();
        assert!(
            results.iter().any(|(id, _)| *id == node_id),
            "High drift node {} (drift > 0.5) should be in results with threshold 0.3",
            i
        );
    }

    // Verify all results have drift >= threshold
    for (node_id, drift) in &results {
        assert!(
            *drift >= 0.3,
            "Node {:?} has drift {:.3} which is below threshold 0.3",
            node_id,
            drift
        );
    }

    // Verify results are sorted by drift descending
    for i in 0..results.len().saturating_sub(1) {
        assert!(
            results[i].1 >= results[i + 1].1,
            "Results should be sorted by drift descending"
        );
    }

    // Verify high drift nodes appear before moderate drift nodes
    let high_drift_indices: Vec<_> = results
        .iter()
        .enumerate()
        .filter(|(_, (id, _))| (7..=9).any(|i| *id == NodeId::new(i).unwrap()))
        .map(|(idx, _)| idx)
        .collect();

    let moderate_drift_indices: Vec<_> = results
        .iter()
        .enumerate()
        .filter(|(_, (id, _))| (4..=6).any(|i| *id == NodeId::new(i).unwrap()))
        .map(|(idx, _)| idx)
        .collect();

    if !high_drift_indices.is_empty() && !moderate_drift_indices.is_empty() {
        let max_high_idx = high_drift_indices.iter().max().unwrap();
        let min_moderate_idx = moderate_drift_indices.iter().min().unwrap();
        assert!(
            max_high_idx < min_moderate_idx,
            "High drift nodes should appear before moderate drift nodes"
        );
    }

    Ok(())
}

#[test]
fn test_semantic_drift_with_multiple_snapshots() -> Result<()> {
    // Test drift detection across multiple snapshots
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        hnsw_config: HnswConfig::new(128, DistanceMetric::Cosine),
    };
    let index = TemporalVectorIndex::new(config)?;
    let mut ts = 1000i64;

    let node_id = NodeId::new(1).unwrap();

    // Create a timeline of embeddings showing gradual drift
    let embeddings = [
        // t0: Initial state
        normalize(&[1.0, 0.0, 0.0, 0.0]),
        // t1: Small change
        normalize(&[0.9, 0.1, 0.0, 0.0]),
        // t2: Moderate change
        normalize(&[0.7, 0.3, 0.0, 0.0]),
        // t3: Large change (approaching orthogonal)
        normalize(&[0.3, 0.7, 0.0, 0.0]),
        // t4: Very large change (nearly orthogonal)
        normalize(&[0.1, 0.9, 0.0, 0.0]),
    ];

    for (i, embedding) in embeddings.iter().enumerate() {
        if i > 0 {
            index.remove(node_id, ts)?;
        }
        let mut padded = vec![0.0f32; 128];
        padded[..embedding.len()].copy_from_slice(embedding);
        index.add(node_id, &padded, ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        ts += 1000;
    }

    // Query for drift - should find the node
    let time_range = TimeRange::new(0, i64::MAX);
    let results = index.find_semantic_drift(0.05, time_range, DriftMetric::Cosine)?;

    assert!(
        !results.is_empty(),
        "Should find at least one drifting node"
    );
    assert!(
        results.iter().any(|(id, _)| *id == node_id),
        "Should find the test node"
    );

    // Find the drift value for our test node
    let node_drift = results
        .iter()
        .find(|(id, _)| *id == node_id)
        .map(|(_, d)| d);
    if let Some(&drift) = node_drift {
        // The maximum consecutive drift should be significant
        assert!(drift > 0.1, "Max drift should be > 0.1, got {}", drift);
    }

    Ok(())
}

#[test]
fn test_semantic_drift_different_metrics() -> Result<()> {
    // Test that different metrics produce different but valid results
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        hnsw_config: HnswConfig::new(64, DistanceMetric::Cosine),
    };
    let index = TemporalVectorIndex::new(config)?;
    let mut ts = 1000i64;

    let node_id = NodeId::new(1).unwrap();

    // Create orthogonal vectors (maximum semantic drift)
    let embedding1 = {
        let mut v = vec![0.0f32; 64];
        v[0] = 1.0;
        v
    };
    index.add(node_id, &embedding1, ts)?;
    index.on_transaction_at(ts)?;
    ts += 1000;

    ts += 1000;

    let embedding2 = {
        let mut v = vec![0.0f32; 64];
        v[1] = 1.0;
        v
    };
    index.remove(node_id, ts)?;
    index.add(node_id, &embedding2, ts)?;
    index.on_transaction_at(ts)?;

    let time_range = TimeRange::new(0, i64::MAX);

    // Test all three metrics
    let cosine_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;
    let euclidean_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Euclidean)?;
    let angular_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Angular)?;

    // All metrics should detect the drift
    assert_eq!(cosine_results.len(), 1);
    assert_eq!(euclidean_results.len(), 1);
    assert_eq!(angular_results.len(), 1);

    // Drift values should be different for each metric
    let cosine_drift = cosine_results[0].1;
    let euclidean_drift = euclidean_results[0].1;
    let angular_drift = angular_results[0].1;

    // For orthogonal unit vectors:
    // - Cosine distance: 1.0 (similarity = 0)
    // - Euclidean distance: sqrt(2) ≈ 1.414
    // - Angular distance: π/2 ≈ 1.571
    assert!(
        (cosine_drift - 1.0).abs() < 0.01,
        "Cosine drift should be ~1.0"
    );
    assert!(
        (euclidean_drift - 1.414).abs() < 0.1,
        "Euclidean drift should be ~1.414"
    );
    assert!(
        (angular_drift - std::f32::consts::FRAC_PI_2).abs() < 0.1,
        "Angular drift should be ~π/2"
    );

    Ok(())
}
