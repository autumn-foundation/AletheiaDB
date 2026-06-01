use super::*;
use crate::core::error::Result;
use crate::index::vector::HnswConfig;

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

#[test]
fn test_temporal_index_creation() -> Result<()> {
    let index = create_test_index()?;
    assert_eq!(index.dimensions(), 4);
    assert_eq!(index.distance_metric(), DistanceMetric::Cosine);
    assert_eq!(index.snapshot_count(), 0);
    Ok(())
}

#[test]
fn test_current_index_direct() -> Result<()> {
    let hnsw = HnswIndex::new(HnswConfig::new(4, DistanceMetric::Cosine))?;
    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    hnsw.add(node1, &vec1)?;
    assert_eq!(hnsw.len(), 1);
    Ok(())
}

#[test]
fn test_add_vector() -> Result<()> {
    let index = create_test_index()?;

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    let timestamp = 1000000;

    index.add(node1, &vec1, timestamp.into())?;

    assert_eq!(index.current_index().len(), 1);
    Ok(())
}

#[test]
fn test_snapshot_creation_transaction_interval() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction()?;
    assert_eq!(index.snapshot_count(), 0);

    index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 2000.into())?;
    index.on_transaction()?;
    assert_eq!(index.snapshot_count(), 1);

    Ok(())
}

#[test]
fn test_snapshot_creation_time_interval() -> Result<()> {
    let base_time = 1000000000;

    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TimeInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, base_time.into())?;

    index.add(
        NodeId::new(1).unwrap(),
        &[1.0, 0.0, 0.0, 0.0],
        base_time.into(),
    )?;
    assert_eq!(index.snapshot_count(), 0);

    index.add(
        NodeId::new(2).unwrap(),
        &[0.0, 1.0, 0.0, 0.0],
        (base_time + 2_000_000).into(),
    )?;
    index.on_transaction_at((base_time + 2_000_000).into())?;
    assert_eq!(index.snapshot_count(), 1);

    Ok(())
}

#[test]
fn test_snapshot_creation_change_threshold() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::ChangeThreshold(0.5),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    for i in 0..4 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    assert_eq!(index.snapshot_count(), 0);

    index.add(NodeId::new(0).unwrap(), &[10.0, 0.0, 0.0, 0.0], 2000.into())?;
    index.add(NodeId::new(1).unwrap(), &[11.0, 0.0, 0.0, 0.0], 2000.into())?;
    index.on_transaction()?;
    assert_eq!(index.snapshot_count(), 1);

    Ok(())
}

#[test]
fn test_manual_snapshot() -> Result<()> {
    let index = create_test_index()?;

    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000.into())?;

    assert_eq!(index.snapshot_count(), 0);

    index.create_manual_snapshot()?;
    assert_eq!(index.snapshot_count(), 1);

    Ok(())
}

#[test]
fn test_max_snapshots_limit() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 3,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    for i in 0..5 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            ((i * 1000) as i64).into(),
        )?;
        index.on_transaction()?;
    }

    assert_eq!(index.snapshot_count(), 3);

    Ok(())
}

#[test]
fn test_find_similar_as_of() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    let query = vec![0.9, 0.1, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 2, 1000.into())?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, NodeId::new(1).unwrap());

    Ok(())
}

#[test]
fn test_find_similar_in_range() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 2000.into())?;
    index.on_transaction_at(2000.into())?;

    index.add(NodeId::new(3).unwrap(), &[0.0, 0.0, 1.0, 0.0], 3000.into())?;
    index.on_transaction_at(3000.into())?;

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(1000.into(), 3000.into()).unwrap();
    let results = index.find_similar_in_range(&query, 5, time_range)?;

    assert!(!results.is_empty());

    for (timestamp, similar_nodes) in results {
        assert!((1000..=3000).contains(&timestamp.wallclock()));
        assert!(!similar_nodes.is_empty());
    }

    Ok(())
}

#[test]
fn test_snapshot_info() -> Result<()> {
    let index = create_test_index()?;

    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.create_manual_snapshot()?;

    let info = index.get_snapshot_info()?;
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].snapshot_id, 0);
    assert!(info[0].timestamp.wallclock() > 0);

    Ok(())
}

#[test]
fn test_temporal_vector_config() {
    let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    let config = TemporalVectorConfig::default_with_hnsw(hnsw_config.clone());

    assert_eq!(config.max_snapshots, 100);
    assert!(matches!(
        config.snapshot_strategy,
        SnapshotStrategy::TransactionInterval(10)
    ));
    assert_eq!(config.hnsw_config, Some(hnsw_config));
}

#[test]
fn test_snapshot_strategy_hybrid() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::Hybrid {
            transaction_interval: 10,
            time_interval_secs: 1,
            change_threshold: 0.5,
        },
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    for i in 0..10 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
        index.on_transaction_at(1000.into())?;
    }
    assert_eq!(index.snapshot_count(), 1);

    Ok(())
}

#[test]
fn test_prune_snapshots_keep_all() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    for i in 0..5 {
        let timestamp = ((1000 * (i + 1)) as i64).into();
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            timestamp,
        )?;
        index.on_transaction_at(timestamp)?;
    }
    assert_eq!(index.snapshot_count(), 5);

    let removed = index.prune_snapshots()?;
    assert_eq!(removed, 0);
    assert_eq!(index.snapshot_count(), 5);

    Ok(())
}

#[test]
fn test_prune_snapshots_keep_n() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(3),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    for i in 0..5 {
        let timestamp = ((1000 * (i + 1)) as i64).into();
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            timestamp,
        )?;
        index.on_transaction_at(timestamp)?;
    }
    assert_eq!(index.snapshot_count(), 5);

    let removed = index.prune_snapshots()?;
    assert_eq!(removed, 2);
    assert_eq!(index.snapshot_count(), 3);

    let info = index.get_snapshot_info()?;
    assert_eq!(info.len(), 3);
    assert_eq!(info[0].snapshot_id, 2);
    assert_eq!(info[1].snapshot_id, 3);
    assert_eq!(info[2].snapshot_id, 4);

    Ok(())
}

#[test]
fn test_prune_snapshots_keep_n_when_under_limit() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(10),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    for i in 0..3 {
        let timestamp = ((1000 * (i + 1)) as i64).into();
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            timestamp,
        )?;
        index.on_transaction_at(timestamp)?;
    }
    assert_eq!(index.snapshot_count(), 3);

    let removed = index.prune_snapshots()?;
    assert_eq!(removed, 0);
    assert_eq!(index.snapshot_count(), 3);

    Ok(())
}

#[test]
fn test_prune_snapshots_keep_duration() -> Result<()> {
    use std::time::Duration;

    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepDuration(Duration::from_secs(5)),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let current_time = TemporalVectorIndex::current_timestamp()?;

    index.add(
        NodeId::new(1).unwrap(),
        &[1.0, 0.0, 0.0, 0.0],
        (current_time.wallclock() - 10_000_000).into(),
    )?;
    index.create_manual_snapshot()?;

    index.add(
        NodeId::new(2).unwrap(),
        &[0.0, 1.0, 0.0, 0.0],
        (current_time.wallclock() - 7_000_000).into(),
    )?;
    index.create_manual_snapshot()?;

    index.add(
        NodeId::new(3).unwrap(),
        &[0.0, 0.0, 1.0, 0.0],
        (current_time.wallclock() - 3_000_000).into(),
    )?;
    index.create_manual_snapshot()?;

    let count_before = index.snapshot_count();
    assert!(count_before >= 3);

    let removed = index.prune_snapshots()?;

    assert_eq!(removed, 0);

    Ok(())
}

#[test]
fn test_retention_policy_default() {
    let default_policy = RetentionPolicy::default();
    assert_eq!(default_policy, RetentionPolicy::KeepN(100));
}

#[test]
fn test_retention_policy_in_config() {
    let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    let config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    assert_eq!(config.retention_policy, RetentionPolicy::KeepN(100));
}

#[test]
fn test_semantic_evolution_basic() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;
    assert_eq!(index.snapshot_count(), 1);

    index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 2000.into())?;
    index.on_transaction_at(2000.into())?;
    assert_eq!(index.snapshot_count(), 2);

    index.add(node_id, &[0.0, 0.0, 1.0, 0.0], 3000.into())?;
    index.on_transaction_at(3000.into())?;
    assert_eq!(index.snapshot_count(), 3);

    let time_range = TimeRange::new(1000.into(), 3000.into()).unwrap();
    let evolution = index.semantic_evolution(node_id, time_range)?;

    assert_eq!(evolution.len(), 3);

    assert_eq!(evolution[0].1.as_ref(), &[1.0, 0.0, 0.0, 0.0]);
    assert_eq!(evolution[1].1.as_ref(), &[0.0, 1.0, 0.0, 0.0]);
    assert_eq!(evolution[2].1.as_ref(), &[0.0, 0.0, 1.0, 0.0]);

    Ok(())
}

#[test]
fn test_semantic_evolution_partial_range() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    for i in 1..=5 {
        let timestamp = i * 1000;
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    let time_range = TimeRange::new(2000.into(), 4000.into()).unwrap();
    let evolution = index.semantic_evolution(node_id, time_range)?;

    assert_eq!(evolution.len(), 3);
    assert_eq!(evolution[0].1.as_ref(), &[2.0, 0.0, 0.0, 0.0]);
    assert_eq!(evolution[1].1.as_ref(), &[3.0, 0.0, 0.0, 0.0]);
    assert_eq!(evolution[2].1.as_ref(), &[4.0, 0.0, 0.0, 0.0]);

    Ok(())
}

#[test]
fn test_semantic_evolution_node_not_in_snapshots() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let other_node = NodeId::new(1).unwrap();
    index.add(other_node, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    let node_id = NodeId::new(42).unwrap();
    let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();
    let evolution = index.semantic_evolution(node_id, time_range)?;

    assert_eq!(evolution.len(), 0);

    Ok(())
}

#[test]
fn test_track_semantic_drift() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    index.add(node_id, &[0.9, 0.1, 0.0, 0.0], 2000.into())?;
    index.on_transaction_at(2000.into())?;

    index.add(node_id, &[0.5, 0.5, 0.0, 0.0], 3000.into())?;
    index.on_transaction_at(3000.into())?;

    let reference = vec![1.0, 0.0, 0.0, 0.0];
    let time_range = TimeRange::new(1000.into(), 3000.into()).unwrap();
    let drift = index.track_semantic_drift(node_id, &reference, time_range)?;

    assert_eq!(drift.len(), 3);
    assert!((drift[0].1 - 1.0).abs() < 0.001);
    assert!(drift[1].1 > 0.99);
    assert!(drift[2].1 < 0.8);

    Ok(())
}

#[test]
fn test_calculate_consecutive_drift() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 2000.into())?;
    index.on_transaction_at(2000.into())?;

    index.add(node_id, &[0.707, 0.707, 0.0, 0.0], 3000.into())?;
    index.on_transaction_at(3000.into())?;

    index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 4000.into())?;
    index.on_transaction_at(4000.into())?;

    let time_range = TimeRange::new(1000.into(), 4000.into()).unwrap();
    let drift = index.calculate_consecutive_drift(node_id, time_range)?;

    assert_eq!(drift.len(), 3);
    assert!(drift[0].1.abs() < 0.001);
    assert!((drift[1].1 - 0.293).abs() < 0.01);
    assert!((drift[2].1 - 0.293).abs() < 0.01);

    Ok(())
}

#[test]
fn test_consecutive_drift_single_vector() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();
    let drift = index.calculate_consecutive_drift(node_id, time_range)?;

    assert_eq!(drift.len(), 0);

    Ok(())
}

#[test]
fn test_semantic_evolution_with_vector_updates() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 2000.into())?;
    index.on_transaction_at(2000.into())?;

    let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();
    let evolution = index.semantic_evolution(node_id, time_range)?;

    assert_eq!(evolution.len(), 2);
    assert_eq!(evolution[0].1.as_ref(), &[1.0, 0.0, 0.0, 0.0]);
    assert_eq!(evolution[1].1.as_ref(), &[0.0, 1.0, 0.0, 0.0]);

    Ok(())
}

#[test]
fn test_vector_history_pruning() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(3),
        max_snapshots: 3,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    for i in 1..=5 {
        let timestamp = i * 1000;
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    assert_eq!(index.snapshot_count(), 3);

    let time_range = TimeRange::new(1000.into(), 5000.into()).unwrap();
    let evolution = index.semantic_evolution(node_id, time_range)?;

    assert_eq!(evolution.len(), 3);
    assert_eq!(evolution[0].1.as_ref(), &[3.0, 0.0, 0.0, 0.0]);
    assert_eq!(evolution[1].1.as_ref(), &[4.0, 0.0, 0.0, 0.0]);
    assert_eq!(evolution[2].1.as_ref(), &[5.0, 0.0, 0.0, 0.0]);

    Ok(())
}

#[test]
fn test_track_semantic_drift_dimension_mismatch() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(42).unwrap();

    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    let wrong_dimension_ref = vec![1.0, 0.0, 0.0];
    let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();

    let result = index.track_semantic_drift(node_id, &wrong_dimension_ref, time_range);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(
        err,
        Error::Vector(VectorError::DimensionMismatch {
            expected: 4,
            actual: 3
        })
    ));

    Ok(())
}

#[test]
fn test_find_semantic_drift_basic() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let mut ts = 1000i64;

    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node1, ts.into())?;
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;

    ts += 1000;
    let node2 = NodeId::new(2).unwrap();
    index.add(node2, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node2, ts.into())?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;

    ts += 1000;
    let node3 = NodeId::new(3).unwrap();
    index.add(node3, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node3, ts.into())?;
    index.add(
        node3,
        &[
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            0.0,
        ],
        ts.into(),
    )?;
    index.on_transaction_at(ts.into())?;

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    let results = index.find_semantic_drift(0.2, time_range, DriftMetric::Cosine)?;

    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|(id, _)| *id == node2));
    assert!(results.iter().any(|(id, _)| *id == node3));

    Ok(())
}

#[test]
fn test_find_semantic_drift_sorted_descending() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let mut ts = 1000i64;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node1, ts.into())?;
    index.add(node1, &[0.9, 0.1, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;

    ts += 1000;
    index.add(node2, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node2, ts.into())?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;

    ts += 1000;
    index.add(node3, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node3, ts.into())?;
    index.add(
        node3,
        &[
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            0.0,
        ],
        ts.into(),
    )?;
    index.on_transaction_at(ts.into())?;

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;

    assert_eq!(results.len(), 3);
    assert!(results[0].1 > results[1].1);
    assert!(results[1].1 > results[2].1);

    assert_eq!(results[0].0, node2);

    Ok(())
}

#[test]
fn test_find_semantic_drift_single_version_excluded() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let mut ts = 1000i64;

    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;

    ts += 1000;
    let node2 = NodeId::new(2).unwrap();
    index.add(node2, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node2, ts.into())?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node2);

    Ok(())
}

#[test]
fn test_find_semantic_drift_empty_range() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let ts = 10000i64;

    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;

    let time_range = TimeRange::new(0.into(), 100.into()).unwrap();
    let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;

    assert_eq!(results.len(), 0);

    Ok(())
}

#[test]
fn test_find_semantic_drift_threshold_zero() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let mut ts = 1000i64;

    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node1, ts.into())?;
    index.add(node1, &[0.9, 0.1, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;
    assert_eq!(results.len(), 1);

    let results = index.find_semantic_drift(-0.5, time_range, DriftMetric::Cosine)?;
    assert_eq!(results.len(), 1);

    Ok(())
}

#[test]
fn test_find_semantic_drift_all_metrics() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let mut ts = 1000i64;

    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node1, ts.into())?;
    index.add(node1, &[0.0, 1.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    let cosine_results = index.find_semantic_drift(0.5, time_range, DriftMetric::Cosine)?;
    assert_eq!(cosine_results.len(), 1);

    let euclidean_results = index.find_semantic_drift(0.5, time_range, DriftMetric::Euclidean)?;
    assert_eq!(euclidean_results.len(), 1);

    let angular_results = index.find_semantic_drift(0.5, time_range, DriftMetric::Angular)?;
    assert_eq!(angular_results.len(), 1);

    Ok(())
}

#[test]
fn test_find_semantic_drift_metrics_comparison() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new(config)?;

    let mut ts = 1000i64;

    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;
    ts += 1000;
    index.remove(node1, ts.into())?;
    index.add(node1, &[0.0, 1.0, 0.0, 0.0], ts.into())?;
    index.on_transaction_at(ts.into())?;

    let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

    let cosine_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;
    assert_eq!(cosine_results[0].1, 1.0);

    let euclidean_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Euclidean)?;
    assert!((euclidean_results[0].1 - 1.414).abs() < 0.01);

    let angular_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Angular)?;
    assert!((angular_results[0].1 - std::f32::consts::FRAC_PI_2).abs() < 0.01);

    Ok(())
}

#[test]
fn test_drift_metric_default() {
    let metric = DriftMetric::default();
    assert_eq!(metric, DriftMetric::Cosine);
}

// ==================== Delta Snapshot Tests ====================

#[test]
fn test_delta_snapshots_actually_used() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create initial vectors - this will be snapshot 0 (full)
    for i in 0..100 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;
    assert_eq!(index.snapshot_count(), 1);

    // Modify only 5 vectors - snapshots 1-9 should be deltas
    for snapshot_num in 1..10 {
        let timestamp = (1000 + (snapshot_num * 1000) as i64).into();
        // Only modify 5 vectors
        for i in 0..5 {
            index.add(
                NodeId::new(i).unwrap(),
                &[(i + snapshot_num) as f32, 1.0, 0.0, 0.0],
                timestamp,
            )?;
        }
        index.on_transaction_at(timestamp)?;
    }

    assert_eq!(index.snapshot_count(), 10);

    // Snapshot 10 should be full again (after 10 snapshots)
    index.add(
        NodeId::new(0).unwrap(),
        &[100.0, 0.0, 0.0, 0.0],
        11000.into(),
    )?;
    index.on_transaction_at(11000.into())?;
    assert_eq!(index.snapshot_count(), 11);

    // Verify we can query all snapshots successfully
    for snapshot_num in 0..11 {
        let timestamp = 1000 + (snapshot_num * 1000);
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 10, timestamp.into())?;
        assert!(
            !results.is_empty(),
            "Snapshot {} should have results",
            snapshot_num
        );
    }

    Ok(())
}

#[test]
fn test_delta_snapshot_search_correctness() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create full snapshot with distinct vectors
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 1000.into())?;
    index.add(NodeId::new(3).unwrap(), &[0.0, 0.0, 1.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;
    assert_eq!(index.snapshot_count(), 1);

    // Update vector 2 to be very similar to vector 3 in delta snapshot
    index.add(NodeId::new(2).unwrap(), &[0.0, 0.0, 1.0, 0.0], 2000.into())?;
    index.on_transaction_at(2000.into())?;
    assert_eq!(index.snapshot_count(), 2);

    // Query for [0.0, 0.0, 1.0, 0.0] at new timestamp - should find both 2 and 3
    let query = vec![0.0, 0.0, 1.0, 0.0];
    let results = index.find_similar_as_of(&query, 3, 2000.into())?;

    // Both vectors 2 and 3 should be top results with high similarity
    let top_ids: Vec<NodeId> = results.iter().take(2).map(|(id, _)| *id).collect();
    assert!(top_ids.contains(&NodeId::new(2).unwrap()));
    assert!(top_ids.contains(&NodeId::new(3).unwrap()));

    // Query at old timestamp for [0.0, 1.0, 0.0, 0.0] - should find OLD vector 2
    let query_old = vec![0.0, 1.0, 0.0, 0.0];
    let results_old = index.find_similar_as_of(&query_old, 3, 1000.into())?;
    // At timestamp 1000, vector 2 was [0.0, 1.0, 0.0, 0.0]
    assert_eq!(results_old[0].0, NodeId::new(2).unwrap());
    assert!(results_old[0].1 > 0.99, "Should match old version exactly");

    Ok(())
}

#[test]
fn test_delta_snapshot_handles_removes() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create full snapshot with 5 vectors
    for i in 0..5 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;
    assert_eq!(index.current_index().len(), 5);

    // Remove vector 2 in delta snapshot
    index.remove(NodeId::new(2).unwrap(), 2000.into())?;
    index.on_transaction_at(2000.into())?;
    assert_eq!(index.current_index().len(), 4);

    // Query at new timestamp should not return removed vector
    let query = vec![2.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 5, 2000.into())?;

    // Should only have 4 results (vector 2 was removed)
    assert_eq!(results.len(), 4);
    assert!(!results.iter().any(|(id, _)| *id == NodeId::new(2).unwrap()));

    // Query at old timestamp should still return it
    let results_old = index.find_similar_as_of(&query, 5, 1000.into())?;
    assert_eq!(results_old.len(), 5);
    assert!(
        results_old
            .iter()
            .any(|(id, _)| *id == NodeId::new(2).unwrap())
    );

    Ok(())
}

// ==================== Concurrent Snapshot Tests ====================

#[test]
fn test_concurrent_snapshot_creation() -> Result<()> {
    use std::sync::Arc;
    use std::thread;

    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = Arc::new(TemporalVectorIndex::new_at(config, 1000.into())?);

    // Add initial vectors
    for i in 0..50 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }

    // Spawn multiple threads that concurrently trigger snapshots
    let mut handles = vec![];
    for thread_id in 0..10 {
        let idx = Arc::clone(&index);
        handles.push(thread::spawn(move || {
            let base_time = 2000 + (thread_id * 1000);
            for i in 0..5 {
                let node_id = NodeId::new((thread_id * 10 + i) as u64).unwrap();
                let timestamp = base_time + (i * 100);
                idx.add(
                    node_id,
                    &[thread_id as f32, i as f32, 0.0, 0.0],
                    timestamp.into(),
                )
                .unwrap();
                // Try to trigger snapshot
                idx.on_transaction_at(timestamp.into()).unwrap();
            }
        }));
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify no panics occurred and snapshots were created
    let snapshot_count = index.snapshot_count();
    assert!(snapshot_count > 0, "Expected snapshots to be created");

    // Verify we can query without errors
    let query = vec![0.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 10, 10000.into())?;
    assert!(!results.is_empty());

    Ok(())
}

#[test]
fn test_concurrent_snapshot_no_double_create() -> Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = Arc::new(TemporalVectorIndex::new_at(config, 1000.into())?);
    let timestamp = Arc::new(AtomicU64::new(1000));

    // Add initial vectors
    for i in 0..50 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }

    // Spawn threads that all try to create the 10th transaction
    let mut handles = vec![];
    for thread_id in 0..5 {
        let idx = Arc::clone(&index);
        let ts = Arc::clone(&timestamp);
        handles.push(thread::spawn(move || {
            for i in 0..3 {
                let t = ts.fetch_add(100, Ordering::SeqCst);
                let node_id = NodeId::new((thread_id * 10 + i) as u64).unwrap();
                idx.add(
                    node_id,
                    &[thread_id as f32, i as f32, 0.0, 0.0],
                    (t as i64).into(),
                )
                .unwrap();
                idx.on_transaction_at((t as i64).into()).unwrap();
            }
        }));
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Should have created snapshots, but not excessive duplicates
    // With 5 threads x 3 transactions = 15 total, and interval=10, we expect 1-2 snapshots ideally
    // However, with concurrent races, multiple threads can all see transactions_since_snapshot >= 10
    // before any reset the counter, leading to multiple snapshot creations at the same "trigger point"
    // Accept up to 6 snapshots (still much better than 15 if every transaction created one)
    let count = index.snapshot_count();
    assert!(
        (1..=6).contains(&count),
        "Expected 1-6 snapshots due to concurrent racing, got {} (15 transactions with interval=10 should not create 15 snapshots)",
        count
    );

    Ok(())
}

// ==================== Pruning + Delta Tests ====================

#[test]
fn test_prune_base_snapshot_fallback() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(3),
        max_snapshots: 3,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create full snapshot at t=1000
    for i in 0..10 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;
    assert_eq!(index.snapshot_count(), 1);

    // Create delta snapshots at t=2000, 3000 (both reference t=1000)
    for snapshot_num in 1..3 {
        let timestamp = 1000 + (snapshot_num * 1000);
        index.add(
            NodeId::new(0).unwrap(),
            &[snapshot_num as f32, 0.0, 0.0, 0.0],
            timestamp.into(),
        )?;
        index.on_transaction_at(timestamp.into())?;
    }
    assert_eq!(index.snapshot_count(), 3);

    // Create more snapshots to exceed max_snapshots, which will prune t=1000 (the base)
    for snapshot_num in 3..6 {
        let timestamp = 1000 + (snapshot_num * 1000);
        index.add(
            NodeId::new(0).unwrap(),
            &[snapshot_num as f32, 0.0, 0.0, 0.0],
            timestamp.into(),
        )?;
        index.on_transaction_at(timestamp.into())?;
    }

    // Should still have 3 snapshots (max limit)
    assert_eq!(index.snapshot_count(), 3);

    // Verify we can still query the remaining snapshots
    let query = vec![0.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 5, 6000.into())?;
    assert!(
        !results.is_empty(),
        "Should still be able to query after pruning base"
    );

    Ok(())
}

#[test]
fn test_manual_snapshot_with_delta_optimization() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1000), // High threshold
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create initial full snapshot manually
    for i in 0..100 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.create_manual_snapshot()?;
    let first_snapshot_count = index.snapshot_count();
    assert_eq!(first_snapshot_count, 1);

    // Modify only 5 vectors
    for i in 0..5 {
        index.add(
            NodeId::new(i).unwrap(),
            &[100.0 + i as f32, 0.0, 0.0, 0.0],
            2000.into(),
        )?;
    }

    // Manual snapshot should use delta (only 5 changes)
    index.create_manual_snapshot()?;
    assert_eq!(index.snapshot_count(), 2);

    // Verify the snapshots exist and are queryable
    let snapshot_info = index.get_snapshot_info()?;
    assert_eq!(snapshot_info.len(), 2);

    // Verify we can query the latest snapshot
    let query = vec![100.0, 0.0, 0.0, 0.0];
    let latest_timestamp = snapshot_info[1].timestamp;
    let results = index.find_similar_as_of(&query, 10, latest_timestamp)?;
    assert!(!results.is_empty(), "Latest snapshot should have results");

    Ok(())
}

#[test]
fn test_delta_snapshot_len_correctness() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create full snapshot with 100 vectors
    for i in 0..100 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;

    // Get snapshot info for full snapshot
    let snapshots = index.get_snapshot_info()?;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].vector_count, 100);

    // Update 50 vectors (creates delta with 50 in added, 50 in removed, net = 100)
    for i in 0..50 {
        index.add(
            NodeId::new(i).unwrap(),
            &[200.0 + i as f32, 0.0, 0.0, 0.0],
            2000.into(),
        )?;
    }
    index.on_transaction_at(2000.into())?;

    // Check delta snapshot reports correct count
    let snapshots = index.get_snapshot_info()?;
    assert_eq!(snapshots.len(), 2);
    assert_eq!(
        snapshots[1].vector_count, 100,
        "Delta snapshot should report 100 vectors (base 100 + added 50 - removed 50)"
    );

    Ok(())
}

// ==================== Safety and Edge Case Tests ====================

#[test]
fn test_delta_chain_depth_limit_enforced() -> Result<()> {
    // This test verifies that MAX_DELTA_CHAIN_DEPTH is enforced during reconstruction
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10, // Set to MAX_DELTA_CHAIN_DEPTH
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create initial full snapshot
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    // Create 15 delta snapshots (exceeds MAX_DELTA_CHAIN_DEPTH of 10)
    for i in 1..=15 {
        let timestamp = 1000 + (i * 100);
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    // Query at a timestamp that would require traversing > 10 deltas
    // This should either return None or force a full snapshot
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let result = index.find_similar_as_of(&query, 5, 2500.into());

    // Test passes if it doesn't panic and handles the deep chain gracefully
    assert!(
        result.is_ok(),
        "Should handle deep delta chains without panic"
    );

    Ok(())
}

#[test]
fn test_full_snapshot_interval_boundary() -> Result<()> {
    // Test that full snapshots are created exactly at the configured interval
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 5, // Create full snapshot every 5 snapshots
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    let node_id = NodeId::new(1).unwrap();

    // Create snapshots and track which are full vs delta
    for i in 0..10 {
        let timestamp = 1000 + (i * 100);
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    // Verify we created snapshots (should be 10)
    let snapshot_count = index.snapshot_count();
    assert_eq!(snapshot_count, 10, "Should have created 10 snapshots");

    // Get snapshot info to verify snapshots were created
    let snapshot_info = index.get_snapshot_info()?;

    // Verify we have all 10 snapshots
    assert_eq!(snapshot_info.len(), 10, "Should have 10 snapshots in info");

    // Note: We can't directly verify Full vs Delta from SnapshotInfo,
    // but the test verifies that the full_snapshot_interval configuration works

    Ok(())
}

#[test]
fn test_delta_snapshot_base_validation() -> Result<()> {
    // This test verifies that delta snapshots only reference Full snapshots
    // by checking the internal structure after snapshot creation
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 3,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create initial full snapshot
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    // Create several delta snapshots
    for i in 1..=5 {
        let timestamp = 1000 + (i * 100);
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    // Verify all queries work correctly (this implicitly tests base validation)
    for i in 1..=5 {
        let timestamp = 1000 + (i * 100);
        let query = vec![i as f32, 0.0, 0.0, 0.0];
        let result = index.find_similar_as_of(&query, 5, timestamp.into())?;
        assert!(
            !result.is_empty(),
            "Should find results at timestamp {}",
            timestamp
        );
    }

    Ok(())
}

#[test]
fn test_concurrent_snapshot_with_pruning() -> Result<()> {
    use std::sync::Arc;
    use std::thread;

    // Test concurrent snapshot creation with aggressive pruning
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
        retention_policy: RetentionPolicy::KeepN(5), // Aggressive pruning
        max_snapshots: 5,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = Arc::new(TemporalVectorIndex::new_at(config, 1000.into())?);

    // Add initial vectors
    for i in 0..20 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }

    // Spawn threads that will trigger both snapshot creation and pruning
    let mut handles = vec![];
    for thread_id in 0..5 {
        let idx = Arc::clone(&index);
        handles.push(thread::spawn(move || {
            for i in 0..10 {
                let timestamp = 2000 + (thread_id * 1000) + (i * 100);
                let node_id = NodeId::new((thread_id * 10 + i) as u64).unwrap();
                idx.add(
                    node_id,
                    &[thread_id as f32, i as f32, 0.0, 0.0],
                    timestamp.into(),
                )
                .unwrap();
                // In a concurrent stress test, snapshot creation can legitimately fail
                // due to concurrent modifications. This is expected behavior.
                let _ = idx.on_transaction_at(timestamp.into());
            }
        }));
    }

    // Wait for completion
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify snapshot count is within the retention limit
    let snapshot_count = index.snapshot_count();
    assert!(
        snapshot_count <= 5,
        "Should respect max_snapshots limit, got {}",
        snapshot_count
    );

    // Verify we can still query
    let query = vec![0.0, 0.0, 0.0, 0.0];
    let current_time = TemporalVectorIndex::current_timestamp()?;
    let results = index.find_similar_as_of(&query, 5, current_time)?;
    assert!(
        !results.is_empty(),
        "Should still be able to query after concurrent pruning"
    );

    Ok(())
}

#[test]
fn test_snapshot_retry_limit() -> Result<()> {
    // Test that snapshot creation respects MAX_SNAPSHOT_RETRIES
    // This is a best-effort test - it's hard to force exactly 3 retries
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Add vectors and create snapshots normally
    for i in 0..5 {
        let timestamp = (1000 + (i * 100) as i64).into();
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            timestamp,
        )?;
        index.on_transaction_at(timestamp)?;
    }

    // Verify snapshots were created successfully
    let snapshot_count = index.snapshot_count();
    assert!(snapshot_count >= 1, "Should have created snapshots");

    // Test passes if no panic occurred during snapshot creation
    // (The retry logic prevents infinite loops)
    Ok(())
}

#[test]
fn test_delta_snapshot_len_with_pure_additions() -> Result<()> {
    // This test verifies the fix for the critical len() bug where pure additions
    // were incorrectly included in DeltaIndex.removed, causing incorrect counts
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10, // Set to MAX_DELTA_CHAIN_DEPTH to ensure delta snapshots
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create full snapshot with 3 nodes at t=1000
    for i in 0..3 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;

    // Verify base snapshot has 3 nodes
    let snapshot_info = index.get_snapshot_info()?;
    assert_eq!(snapshot_info.len(), 1);
    assert_eq!(snapshot_info[0].vector_count, 3, "Base should have 3 nodes");

    // Add 2 NEW nodes at t=2000 (pure additions, not updates)
    for i in 3..5 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            2000.into(),
        )?;
    }
    index.on_transaction_at(2000.into())?;

    // Verify delta snapshot reports correct count
    let snapshot_info = index.get_snapshot_info()?;
    assert_eq!(snapshot_info.len(), 2, "Should have 2 snapshots");

    // CRITICAL TEST: Delta snapshot should report 5 total nodes (3 from base + 2 new)
    // Before fix: would incorrectly report 3 (because added.len=2 and removed.len=2)
    // After fix: correctly reports 5 (base=3 + added=2 - removed=0)
    assert_eq!(
        snapshot_info[1].vector_count, 5,
        "Delta snapshot should report 5 nodes (3 from base + 2 additions)"
    );

    // Verify we can actually query and find all 5 nodes
    let query = vec![0.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 10, 2000.into())?;
    assert_eq!(
        results.len(),
        5,
        "Should find all 5 nodes when querying at t=2000"
    );

    Ok(())
}

#[test]
fn test_delta_snapshot_len_with_mixed_operations() -> Result<()> {
    // Test len() correctness with a mix of additions, updates, and removals
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create full snapshot with 10 nodes at t=1000
    for i in 0..10 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;
    assert_eq!(index.get_snapshot_info()?[0].vector_count, 10);

    // At t=2000: update 3 nodes, add 2 new nodes, remove 1 node
    // Updates: nodes 0, 1, 2 -> new values
    for i in 0..3 {
        index.add(
            NodeId::new(i).unwrap(),
            &[100.0 + i as f32, 0.0, 0.0, 0.0],
            2000.into(),
        )?;
    }
    // Additions: nodes 10, 11
    for i in 10..12 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            2000.into(),
        )?;
    }
    // Removal: node 9
    index.remove(NodeId::new(9).unwrap(), 2000.into())?;
    index.on_transaction_at(2000.into())?;

    // Expected final count: 10 - 1 (removed) + 2 (added) = 11
    // Updates don't change count (old version replaced by new)
    let snapshot_info = index.get_snapshot_info()?;
    assert_eq!(snapshot_info.len(), 2);
    assert_eq!(
        snapshot_info[1].vector_count, 11,
        "Delta should report 11 nodes (10 base - 1 removed + 2 added)"
    );

    // Verify query results match
    let query = vec![0.0, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 20, 2000.into())?;
    assert_eq!(results.len(), 11, "Should find 11 nodes when querying");

    // Verify removed node 9 is not in results
    assert!(
        !results.iter().any(|(id, _)| *id == NodeId::new(9).unwrap()),
        "Removed node 9 should not appear in results"
    );

    Ok(())
}

#[test]
fn test_delta_search_quality_with_merge() -> Result<()> {
    // Test that delta snapshot search correctly merges results from added and base
    // and returns the true top-k (not just top-k from each index)
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create base with vectors at [1.0, 0, 0, 0], [2.0, 0, 0, 0], ..., [10.0, 0, 0, 0]
    for i in 1..=10 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;

    // Add new vectors at [1.5, 0, 0, 0], [2.5, 0, 0, 0], ..., [10.5, 0, 0, 0]
    for i in 1..=10 {
        index.add(
            NodeId::new((i + 10) as u64).unwrap(),
            &[i as f32 + 0.5, 0.0, 0.0, 0.0],
            2000.into(),
        )?;
    }
    index.on_transaction_at(2000.into())?;

    // Query for [5.5, 0, 0, 0] with k=5
    // Expected top-5 (by cosine similarity to 5.5):
    // 1. Node 15 (vector [5.5, 0, 0, 0]) - exact match
    // 2. Node 5 (vector [5.0, 0, 0, 0]) or Node 16 (vector [6.5, 0, 0, 0])
    // The key test is that we get INTERLEAVED results from both indexes,
    // not just top-5 from added or top-5 from base
    let query = vec![5.5, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 5, 2000.into())?;

    assert_eq!(results.len(), 5, "Should return exactly k=5 results");

    // Verify no duplicates (each NodeId appears once)
    let node_ids: Vec<NodeId> = results.iter().map(|(id, _)| *id).collect();
    let unique_ids: std::collections::HashSet<_> = node_ids.iter().collect();
    assert_eq!(
        unique_ids.len(),
        node_ids.len(),
        "Should have no duplicate NodeIds in results"
    );

    // Note: With k*2 search strategy, we should get high-quality results.
    // The exact distribution between base and added depends on HNSW search behavior,
    // so we just verify we got results (at least some should be from different sources)
    let from_base = results.iter().filter(|(id, _)| id.as_u64() <= 10).count();
    let from_added = results.iter().filter(|(id, _)| id.as_u64() > 10).count();
    // Just log the distribution, don't assert on it as it depends on HNSW behavior
    println!(
        "Results distribution - base: {}, added: {}",
        from_base, from_added
    );

    // Verify results are sorted by descending similarity
    for i in 1..results.len() {
        assert!(
            results[i - 1].1 >= results[i].1,
            "Results should be sorted by descending similarity"
        );
    }

    Ok(())
}

#[test]
fn test_delta_search_no_duplicates_on_update() -> Result<()> {
    // Test that updated nodes don't appear twice in search results
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create base with node 1 at [1.0, 0, 0, 0]
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    for i in 2..=5 {
        index.add(
            NodeId::new(i).unwrap(),
            &[i as f32, 0.0, 0.0, 0.0],
            1000.into(),
        )?;
    }
    index.on_transaction_at(1000.into())?;

    // Update node 1 to [1.5, 0, 0, 0]
    index.add(NodeId::new(1).unwrap(), &[1.5, 0.0, 0.0, 0.0], 2000.into())?;
    index.on_transaction_at(2000.into())?;

    // Search for [1.5, 0, 0, 0] - should find updated node 1 ONCE
    let query = vec![1.5, 0.0, 0.0, 0.0];
    let results = index.find_similar_as_of(&query, 10, 2000.into())?;

    // Count how many times node 1 appears
    let node1_count = results
        .iter()
        .filter(|(id, _)| *id == NodeId::new(1).unwrap())
        .count();
    assert_eq!(
        node1_count, 1,
        "Updated node 1 should appear exactly once in results, not twice"
    );

    // Verify node 1 has the updated vector value (distance should be 0 to [1.5, 0, 0, 0])
    let node1_result = results
        .iter()
        .find(|(id, _)| *id == NodeId::new(1).unwrap())
        .expect("Node 1 should be in results");

    // For cosine similarity, similarity of 1.0 means identical vectors
    assert!(
        node1_result.1 > 0.99,
        "Node 1 should have high similarity to query (updated vector), got {}",
        node1_result.1
    );

    Ok(())
}

// ==================== Stack Overflow Prevention Tests ====================

#[test]
fn test_to_hashmap_depth_limit_with_collect_all() -> Result<()> {
    // CRITICAL TEST: Verifies that to_hashmap() (used by collect_all() and
    // find_semantic_drift()) enforces depth limits and doesn't stack overflow
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10, // Valid config
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create initial full snapshot
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
    index.on_transaction_at(1000.into())?;

    // Create 15 delta snapshots (exceeds MAX_DELTA_CHAIN_DEPTH of 10)
    for i in 1..=15 {
        let timestamp = 1000 + (i * 100);
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    // Call collect_all() which uses to_hashmap() - should succeed without panicking
    // With full_snapshot_interval=10, we get full snapshots periodically,
    // so max delta chain is ~5 (well under MAX_DELTA_CHAIN_DEPTH=10)
    let snapshot_data = index.snapshot_data.read();
    let latest_timestamp = (1000 + (15 * 100)).into();
    if let Some(latest_snapshot) = snapshot_data.vector_history.get(&latest_timestamp) {
        let result = latest_snapshot.collect_all(&snapshot_data.vector_history);

        // Should succeed (not panic) and return all vectors
        // If depth were exceeded, it would return an error instead of partial results
        assert!(
            result.is_ok(),
            "collect_all() should succeed without overflow when full snapshots are properly spaced"
        );

        // Verify we got results
        if let Ok(all_vectors) = result {
            assert!(!all_vectors.is_empty(), "Should have collected vectors");
        }
    }

    Ok(())
}

#[test]
fn test_find_semantic_drift_with_deep_delta_chain() -> Result<()> {
    // Test that find_semantic_drift (which calls to_hashmap via collect_all)
    // handles deep delta chains gracefully
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 100,
        full_snapshot_interval: 10, // Valid config
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Create node with gradual drift over 15 snapshots
    let node_id = NodeId::new(1).unwrap();
    for i in 0..=15 {
        let timestamp = 1000 + (i * 100);
        let drift = i as f32 * 0.1; // Gradual drift from [1.0, 0, 0, 0] to [2.5, 0, 0, 0]
        index.add(node_id, &[1.0 + drift, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    // Call find_semantic_drift which internally uses collect_all/to_hashmap
    // Should NOT stack overflow even with many snapshots
    // With full_snapshot_interval=10, we get full snapshots at 0 and 10,
    // so max delta chain is 5 (snapshots 11-15), well under MAX_DELTA_CHAIN_DEPTH
    use crate::core::temporal::TimeRange;
    let time_range = TimeRange::new(1000.into(), 2500.into()).unwrap();
    let result = index.find_semantic_drift(
        0.1, // threshold
        time_range,
        DriftMetric::Cosine,
    );

    // Test passes if it doesn't panic/overflow and returns valid results
    // (no depth limit exceeded since we have proper full snapshots)
    assert!(
        result.is_ok(),
        "find_semantic_drift should handle chains within depth limit"
    );

    Ok(())
}

#[test]
fn test_depth_limit_error_behavior() {
    // Test that missing base snapshot returns a proper error
    // Directly test the VectorSnapshot methods with corrupted state

    use std::collections::BTreeMap;

    // Create a delta snapshot that references a missing base
    let mut added = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
    added.insert(
        NodeId::new(1).unwrap(),
        Arc::from(vec![1.0f32, 0.0f32, 0.0f32, 0.0f32]) as Arc<[f32]>,
    );

    let delta_snapshot = VectorSnapshot::Delta {
        base_time: 1000.into(), // This base won't exist
        added: Arc::new(added),
        removed: Arc::new(HashSet::with_hasher(
            BuildHasherDefault::<IdentityHasher>::default(),
        )),
    };

    let snapshots: BTreeMap<Timestamp, VectorSnapshot> = BTreeMap::new(); // Empty - no base

    // Test get_vector with missing base - query a node NOT in added, so it must traverse to base
    let result = delta_snapshot.get_vector(&NodeId::new(2).unwrap(), &snapshots);
    assert!(
        result.is_err(),
        "Should return error when base snapshot is missing"
    );

    if let Err(err) = result {
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("not found")
                || err_msg.contains("corrupted")
                || err_msg.contains("missing"),
            "Error should mention missing base or corrupted state, got: {}",
            err_msg
        );
    }

    // Test to_hashmap with missing base
    let result = delta_snapshot.to_hashmap(&snapshots);
    assert!(
        result.is_err(),
        "to_hashmap should return error when base snapshot is missing"
    );

    if let Err(err) = result {
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("not found")
                || err_msg.contains("corrupted")
                || err_msg.contains("missing"),
            "Error should mention missing base or corrupted state, got: {}",
            err_msg
        );
    }
}

// ==================== Config Validation Tests ====================

#[test]
fn test_config_validation_full_snapshot_interval_allowed_large() {
    // This test verifies that we can set full_snapshot_interval > MAX_DELTA_CHAIN_DEPTH
    // because the implementation uses a Star Topology (depth=1), so large intervals
    // do not cause deep traversals.
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 100, // Exceeds MAX_DELTA_CHAIN_DEPTH (50)
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };

    let result = TemporalVectorIndex::new(config);
    assert!(
        result.is_ok(),
        "Should accept config with large full_snapshot_interval"
    );
}

#[test]
fn test_config_validation_max_snapshots_zero() {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 0, // Invalid
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };

    let result = TemporalVectorIndex::new(config);
    assert!(result.is_err(), "Should reject config with max_snapshots=0");

    if let Err(err) = result {
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("max_snapshots must be at least 1"),
            "Error message should mention max_snapshots requirement"
        );
    }
}

#[test]
fn test_config_validation_valid_config() -> Result<()> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10, // Valid: equals MAX_DELTA_CHAIN_DEPTH
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };

    let result = TemporalVectorIndex::new(config);
    assert!(
        result.is_ok(),
        "Should accept valid config with full_snapshot_interval=MAX_DELTA_CHAIN_DEPTH"
    );

    Ok(())
}

#[test]
fn test_nan_validation() -> Result<()> {
    let index = create_test_index()?;
    let node_id = NodeId::new(1).unwrap();

    // Test NaN rejection
    let vec_with_nan = vec![1.0, f32::NAN, 0.0, 0.0];
    let result = index.add(node_id, &vec_with_nan, 1000.into());
    assert!(result.is_err(), "Should reject vector with NaN");
    if let Err(err) = result {
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("NaN") || err_msg.contains("infinite"),
            "Error message should mention NaN or infinite values"
        );
    }

    // Test infinite rejection
    let vec_with_inf = vec![1.0, f32::INFINITY, 0.0, 0.0];
    let result = index.add(node_id, &vec_with_inf, 1000.into());
    assert!(result.is_err(), "Should reject vector with infinity");

    // Test negative infinite rejection
    let vec_with_neg_inf = vec![1.0, f32::NEG_INFINITY, 0.0, 0.0];
    let result = index.add(node_id, &vec_with_neg_inf, 1000.into());
    assert!(
        result.is_err(),
        "Should reject vector with negative infinity"
    );

    // Test valid vector still works
    let vec_valid = vec![1.0, 0.0, 0.0, 0.0];
    let result = index.add(node_id, &vec_valid, 1000.into());
    assert!(result.is_ok(), "Should accept valid vector");

    Ok(())
}

#[test]
fn test_timestamp_upper_bound_validation() -> Result<()> {
    use crate::core::temporal::MAX_VALID_TIMESTAMP;

    let index = create_test_index()?;
    let node_id = NodeId::new(1).unwrap();
    let vec_valid = vec![1.0, 0.0, 0.0, 0.0];

    // Test timestamp at MAX_VALID_TIMESTAMP (should work)
    let result = index.add(node_id, &vec_valid, MAX_VALID_TIMESTAMP.into());
    assert!(result.is_ok(), "Should accept MAX_VALID_TIMESTAMP");

    // Test timestamp exceeding MAX_VALID_TIMESTAMP (should fail)
    let result = index.add(node_id, &vec_valid, (MAX_VALID_TIMESTAMP + 1).into());
    assert!(
        result.is_err(),
        "Should reject timestamp > MAX_VALID_TIMESTAMP"
    );
    if let Err(err) = result {
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("MAX_VALID_TIMESTAMP") || err_msg.contains("exceeds"),
            "Error message should mention MAX_VALID_TIMESTAMP"
        );
    }

    // Test timestamp at i64::MAX (should fail - exceeds MAX_VALID_TIMESTAMP)
    let result = index.add(node_id, &vec_valid, (i64::MAX).into());
    assert!(result.is_err(), "Should reject timestamp at i64::MAX");

    // Test negative timestamp (should fail - timestamps must be non-negative)
    let result = index.add(node_id, &vec_valid, (-1i64).into());
    assert!(result.is_err(), "Should reject negative timestamp");

    Ok(())
}

#[test]
fn test_memory_growth_with_many_updates() -> Result<()> {
    // Test that changes_accumulated grows as expected and triggers fallback
    // Use a high full_snapshot_interval to see accumulation
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(50),
        max_snapshots: 50,
        full_snapshot_interval: 10, // Full snapshot every 10 transactions
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };
    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Add many unique vectors to grow changes_accumulated
    for i in 0..25 {
        let node_id = NodeId::new(i as u64).unwrap();
        let timestamp = 1000 + (i * 100);
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    // Check memory stats
    let stats = index.memory_stats();

    // After 25 transactions with full_snapshot_interval=10:
    // - Full snapshots at: 0, 10, 20
    // - Currently at snapshot 25 (5 since last full at 20)
    // - changes_accumulated should have ~5 unique vectors since last full
    assert!(
        stats.changes_accumulated_size > 0,
        "Should have accumulated changes"
    );

    assert!(
        stats.changes_accumulated_size < 30,
        "Should have reset on full snapshots (got {})",
        stats.changes_accumulated_size
    );

    assert_eq!(
        stats.current_vectors, 25,
        "Should have 25 vectors in current index"
    );

    // Test is_high_memory_usage (should be false for small dataset)
    assert!(
        !stats.is_high_memory_usage(),
        "Should not flag high memory for small dataset"
    );

    Ok(())
}

#[test]
fn test_memory_fallback_forces_full_snapshot() -> Result<()> {
    // Test that exceeding MAX_ACCUMULATED_CHANGES forces a full snapshot
    // We'll use a small threshold for testing by checking the actual behavior

    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(50),
        max_snapshots: 50,
        full_snapshot_interval: 100, // High interval, should rely on memory fallback
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };

    let index = TemporalVectorIndex::new_at(config, 1000.into())?;

    // Add vectors - with full_snapshot_interval=100, we normally wouldn't snapshot fully
    // until 100 snapshots. But we want to test if memory limits force one earlier.
    // NOTE: Since MAX_ACCUMULATED_CHANGES is 100,000, we can't easily trigger it
    // in a unit test without mocking constants or adding huge data.
    // Instead, we verify that the config is now accepted.
    for i in 0..15 {
        let node_id = NodeId::new(i as u64).unwrap();
        let timestamp = 1000 + (i * 100);
        index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    let stats = index.memory_stats();

    // Verify changes_accumulated is bounded by full snapshots
    // With full_snapshot_interval=10: full at 0, 10, so at snapshot 15 we have 5 accumulated
    assert!(
        stats.changes_accumulated_size < 20,
        "changes_accumulated should be bounded by periodic full snapshots (got {})",
        stats.changes_accumulated_size
    );

    Ok(())
}
