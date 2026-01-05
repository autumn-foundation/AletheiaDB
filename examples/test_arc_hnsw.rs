// Minimal test to see if Arc<HnswIndex> causes crash
use gallifreydb::index::vector::{HnswIndex, HnswConfig, DistanceMetric, VectorIndex};
use gallifreydb::core::id::NodeId;
use gallifreydb::utils::Result;
use std::sync::Arc;

fn main() -> Result<()> {
    println!("Testing Arc<HnswIndex>...");

    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let hnsw = Arc::new(HnswIndex::new(config)?);

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];

    println!("About to call add on Arc<HnswIndex>...");
    hnsw.add(node1, &vec1)?;

    println!("Success! Arc<HnswIndex> works.");
    println!("Index length: {}", hnsw.len());

    Ok(())
}
