use gallifreydb::index::vector::{HnswIndexBuilder, DistanceMetric, Quantization, HnswConfig, HnswIndex};
use gallifreydb::index::VectorIndex;
use gallifreydb::core::id::NodeId;
use tempfile::tempdir;

#[test]
fn test_crash_via_quantization_mismatch() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("havoc.index");

    // 1. Create a valid I8 index
    println!("Step 1: Creating I8 index...");
    let dims = 128; // Large enough to cause overread
    let index_builder = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .quantization(Quantization::I8)
        .m(16)
        .ef_construction(100);

    let index = index_builder.build().unwrap();

    // Add some vectors
    for i in 1..100 {
        let node_id = NodeId::new(i).unwrap();
        let vector = vec![0.1f32; dims]; // Just some data
        index.add(node_id, &vector).unwrap();
    }

    index.save(&index_path).unwrap();
    println!("Index saved to {:?}", index_path);

    // Drop the index to close file handles
    drop(index);

    // 2. Load it as F32 with Custom Metric
    // This is the malicious configuration.
    // We lie and say it's F32. We also provide a custom metric to force Rust code execution.
    println!("Step 2: Loading as F32 with Custom Metric...");

    let bad_config = HnswConfig::new(dims, DistanceMetric::Cosine)
        .with_quantization(Quantization::F32) // LIE! The file is I8
        .with_custom_metric("havoc_metric", |a: &[f32], b: &[f32]| {
            // Accessing the data should crash if we are reading out of bounds
            let mut sum = 0.0;
            for i in 0..a.len() {
                // If a points to I8 data (1 byte/dim) but we read it as F32 (4 bytes/dim),
                // we will read 4x past the end of the vector.
                // a[i] read will eventually hit unmapped memory.
                sum += a[i] * b[i];
            }
            sum
        });

    // This load should probably fail if usearch validates the file header correctly.
    // If it succeeds, we are in danger.
    let loaded_index_result = HnswIndex::load(&index_path, bad_config);

    if let Err(e) = loaded_index_result {
        println!("Safe! Load failed: {}", e);
        return;
    }

    let loaded_index = loaded_index_result.unwrap();
    println!("Danger! Index loaded successfully despite mismatch.");

    // 3. Trigger the crash
    println!("Step 3: Searching...");
    let query = vec![0.1f32; dims];

    // This calls usearch.search -> custom_metric -> crash
    let result = loaded_index.search(&query, 10);

    match result {
        Ok(_) => println!("Survived? That's unexpected."),
        Err(e) => println!("Search error: {}", e),
    }
}
