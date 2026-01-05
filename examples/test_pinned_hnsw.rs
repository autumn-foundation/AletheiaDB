// Test if pinning HnswIndex prevents crash
use gallifreydb::index::vector::{HnswIndex, HnswConfig, DistanceMetric, VectorIndex};
use gallifreydb::core::id::NodeId;
use gallifreydb::utils::Result;
use std::pin::Pin;

struct Wrapper {
    index: Pin<Box<HnswIndex>>,
}

impl Wrapper {
    fn new() -> Result<Self> {
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        Ok(Wrapper {
            index: Box::pin(HnswIndex::new(config)?),
        })
    }

    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
        self.index.add(id, vector)
    }
}

fn main() -> Result<()> {
    println!("Testing Pin<Box<HnswIndex>>...");

    let wrapper = Wrapper::new()?;

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];

    println!("About to call add on pinned HnswIndex...");
    wrapper.add(node1, &vec1)?;

    println!("Success! Pin<Box<HnswIndex>> works!");
    println!("Index length: {}", wrapper.index.len());

    Ok(())
}
