use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
use gallifreydb::index::vector::hnsw::HnswIndex;
use gallifreydb::core::id::NodeId;
use std::sync::Arc;

fn main() {
    println!("Test 1: Direct HnswIndex");
    let index1 = HnswIndex::new(HnswConfig::new(4, DistanceMetric::Cosine)).unwrap();
    index1.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
    println!("Test 1 PASSED: len = {}", index1.len());
    
    println!("\nTest 2: Arc<HnswIndex>");
    let index2 = Arc::new(HnswIndex::new(HnswConfig::new(4, DistanceMetric::Cosine)).unwrap());
    index2.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
    println!("Test 2 PASSED: len = {}", index2.len());
    
    println!("\nTest 3: TemporalVectorIndex minimal");
    use gallifreydb::index::vector::temporal::*;
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
    };
    let index3 = TemporalVectorIndex::new(config).unwrap();
    println!("Test 3a: Created TemporalVectorIndex");
    index3.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000000).unwrap();
    println!("Test 3 PASSED: len = {}", index3.current_index().len());
}
