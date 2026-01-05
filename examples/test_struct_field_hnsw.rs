// Test if HnswIndex as a struct field causes crash
use gallifreydb::index::vector::{HnswIndex, HnswConfig, DistanceMetric, VectorIndex};
use gallifreydb::core::id::NodeId;
use gallifreydb::utils::Result;

struct Wrapper {
    index: HnswIndex,
}

impl Wrapper {
    fn new() -> Result<Self> {
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        Ok(Wrapper {
            index: HnswIndex::new(config)?,
        })
    }

    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
        self.index.add(id, vector)
    }
}

fn main() -> Result<()> {
    println!("Testing HnswIndex as struct field...");

    let wrapper = Wrapper::new()?;

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];

    println!("About to call add through struct field...");
    wrapper.add(node1, &vec1)?;

    println!("Success! HnswIndex as struct field works.");
    println!("Index length: {}", wrapper.index.len());

    Ok(())
}
