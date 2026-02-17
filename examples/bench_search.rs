use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::hnsw::{HnswConfig, HnswIndex};
use aletheiadb::index::vector::{DistanceMetric, VectorIndex};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dimensions = 128;
    let count = 10_000;
    let k = 100;

    println!(
        "Setting up index with {} vectors (dims={})...",
        count, dimensions
    );
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine)
        .with_m(16)
        .with_ef_construction(100);
    let index = HnswIndex::new(config)?;

    // Add vectors
    for i in 0..count {
        let vec: Vec<f32> = (0..dimensions)
            .map(|x| (x as f32 + i as f32).sin())
            .collect();
        index.add(NodeId::new(i as u64 + 1).unwrap(), &vec)?;
    }

    println!("Warmup...");
    let query = vec![0.5f32; dimensions];
    for _ in 0..100 {
        index.search(&query, k)?;
    }

    println!("Benchmarking search...");
    let start = Instant::now();
    let iterations = 10_000;
    for _ in 0..iterations {
        let results = index.search(&query, k)?;
        assert_eq!(results.len(), k);
    }
    let duration = start.elapsed();

    println!("Total time: {:?}", duration);
    println!("Avg time per search: {:?}", duration / iterations as u32);

    Ok(())
}
