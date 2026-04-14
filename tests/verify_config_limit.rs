use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::DistanceMetric;
use aletheiadb::index::vector::hnsw::HnswConfig;
use aletheiadb::index::vector::temporal::{
    RetentionPolicy, SnapshotStrategy, TemporalVectorConfig, TemporalVectorIndex,
};

#[test]
fn test_large_full_snapshot_interval() -> aletheiadb::core::error::Result<()> {
    // This test verifies that we can use a full_snapshot_interval significantly larger than
    // the safety sentinel MAX_DELTA_CHAIN_DEPTH (50).
    let interval = 100;

    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1), // Snapshot every txn
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 200,
        full_snapshot_interval: interval,
        hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
    };

    // Should succeed (validation removed)
    let index = TemporalVectorIndex::new(config)?;

    // Create more snapshots than the safety limit (50) but less than interval (100)
    // All of these should be Delta snapshots pointing to the same base (Star Topology).
    // If the implementation incorrectly chained them, we would hit the safety limit around snapshot 50.
    for i in 0..70 {
        let node_id = NodeId::new(i as u64).unwrap();
        let timestamp = (1000 + i * 100) as i64;

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], timestamp.into())?;
        index.on_transaction_at(timestamp.into())?;
    }

    assert_eq!(index.snapshot_count(), 70);

    // Verify we can query the latest snapshot (depth should be 1, not 70)
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let latest_timestamp = (1000 + 69 * 100) as i64;
    let results = index.find_similar_as_of(&query, 10, latest_timestamp.into())?;

    assert!(!results.is_empty());

    Ok(())
}
