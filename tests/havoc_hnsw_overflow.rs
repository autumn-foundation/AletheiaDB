use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, Quantization, VectorIndex,
};
use tempfile::tempdir;

#[test]
#[ignore]
fn test_hnsw_dimension_mismatch_overflow() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("overflow.index");

    // 1. Create index with SMALL dimensions (4)
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();
    }

    // 2. Load with LARGE dimensions (100) and custom metric
    // The custom metric expects 100 floats.
    // If usearch passes a pointer to 4 floats, reading 100 is UB.

    let large_dims = 100;

    let config = HnswConfig::new(large_dims, DistanceMetric::Cosine)
        .with_quantization(Quantization::F32) // Must be F32 for custom metric
        .with_custom_metric("exploit", move |a: &[f32], _b: &[f32]| {
            // Check length. If we are safe, this should be 4.
            // If we are unsafe, this slice thinks it is 100.
            // Accessing a[99] should read garbage or crash.

            // We want to prove it thinks it's 100.
            if a.len() != large_dims {
                println!("Safe! Dimension mismatch detected. Len: {}", a.len());
                return 0.0;
            }

            println!(
                "Unsafe! Wrapper thinks len is {}. Reading a[99]...",
                a.len()
            );
            // Force a read of the out-of-bounds memory
            let val = unsafe { std::ptr::read_volatile(&a[99]) };
            println!("Read value: {}", val);

            0.0
        });

    // 3. Load the index
    // This might fail if load() checks dimensions.
    let index_res = HnswIndex::load(&path, config);

    if let Ok(index) = index_res {
        println!("Index loaded successfully (unexpectedly!)");

        // 4. Trigger search to run the metric
        // We need a query vector of size 100 (matching config)
        let query = vec![0.0f32; large_dims];
        let _ = index.search(&query, 1);
    } else {
        println!("Load failed (as expected?): {:?}", index_res.err());
    }
}
