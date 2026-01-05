// Test if Box<HnswIndex> works (instead of Arc)
use gallifreydb::index::vector::{HnswIndex, HnswConfig, DistanceMetric, VectorIndex};
use gallifreydb::core::id::NodeId;
use gallifreydb::utils::Result;

fn main() -> Result<()> {
    println!("Testing Box<HnswIndex>...");

    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let hnsw = Box::new(HnswIndex::new(config)?);

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];

    println!("About to call add on Box<HnswIndex>...");
    hnsw.add(node1, &vec1)?;

    println!("Success! Box<HnswIndex> works.");
    println!("Index length: {}", hnsw.len());

    Ok(())
}
