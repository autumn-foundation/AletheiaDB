use gallifreydb::index::vector::temporal::*;
use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
use gallifreydb::index::VectorIndex;
use gallifreydb::core::id::NodeId;
use gallifreydb::utils::Result;

fn create_test_index() -> Result<TemporalVectorIndex> {
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
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
