use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, Quantization};

#[test]
fn test_havoc_quantization_mismatch_crash() {
    // 1. Create a valid index with I8 quantization
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("quantized.index");

    {
        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_quantization(Quantization::I8); // Use I8

        let index = HnswIndex::new(config).unwrap();

        let node1 = NodeId::new(1).unwrap();
        // Add a vector
        index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        index.save(&index_path).unwrap();
    }

    // 2. Load the index but claim it is F32 (which is required for custom metric)
    // The file has I8 data. We say it's F32.
    let config = HnswConfig::new(4, DistanceMetric::Cosine)
        .with_quantization(Quantization::F32) // Claim F32
        .with_custom_metric("crash_test", |a, b| {
            // This shouldn't be called if validation works
            println!("Metric called! a[0]={}, b[0]={}", a[0], b[0]);
            0.0
        });

    println!("Attempting to load I8 index as F32...");
    let result = HnswIndex::load(&index_path, config);

    match result {
        Ok(idx) => {
            // If we loaded successfully, we are vulnerable!
            println!(
                "Index loaded successfully. Quantization: {:?}",
                idx.quantization()
            );

            // Trigger search to demonstrate crash/garbage
            let query = vec![0.0; 4];
            let _ = idx.search(&query, 1);

            panic!(
                "Security Vulnerability: Successfully loaded I8 index with F32 config and custom metric! This allows buffer over-read."
            );
        }
        Err(e) => {
            println!("Load failed as expected: {}", e);
            // This is what we want
        }
    }
}

#[test]
fn test_havoc_dimension_mismatch_rejection() {
    // 1. Create a valid index with small dimensions (e.g., 4)
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("small_dim.index");

    {
        let index = HnswIndex::new(HnswConfig::new(4, DistanceMetric::Cosine)).unwrap();

        let node1 = NodeId::new(1).unwrap();
        // Add a vector
        index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        index.save(&index_path).unwrap();
    }

    // 2. Load the index but claim it has LARGE dimensions (e.g., 1024)
    let large_dims = 1024;
    let config = HnswConfig::new(large_dims, DistanceMetric::Cosine);

    println!("Attempting to load index with mismatching dimensions...");
    let result = HnswIndex::load(&index_path, config);

    // We want this to fail at load time, not search time
    match result {
        Ok(_) => {
            panic!("Security Vulnerability: Successfully loaded 4-dim index with 1024-dim config!");
        }
        Err(e) => {
            println!("Load failed as expected: {}", e);
        }
    }
}
