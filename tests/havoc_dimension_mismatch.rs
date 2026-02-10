use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorIndex};

#[test]
fn havoc_dimension_mismatch_legacy_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.usearch");

    // 1. Create index with dim=10
    println!("Creating index with dim=10...");
    let config_small = HnswConfig::new(10, DistanceMetric::Cosine);
    let index_small = HnswIndex::new(config_small).unwrap();

    // Add some vectors
    for i in 0..100 {
        let vec = vec![0.1f32; 10];
        index_small.add(NodeId::new(i + 1).unwrap(), &vec).unwrap();
    }

    index_small.save(&path).unwrap();

    // 2. Delete mappings file to simulate legacy index (no metadata)
    let mappings_path = path.with_extension("usearch.mappings");
    if mappings_path.exists() {
        std::fs::remove_file(&mappings_path).unwrap();
        println!("Deleted mappings file to simulate legacy index.");
    }

    // 3. Load with dim=100 (Mismatch!)
    println!("Attempting to load with dim=100...");
    let config_large = HnswConfig::new(100, DistanceMetric::Cosine);

    // This load *might* succeed if usearch doesn't validate dimensions or if HnswIndex doesn't check
    let index_large_result = HnswIndex::load(&path, config_large);

    match index_large_result {
        Ok(index_large) => {
            // If load succeeds, we are in a dangerous state.
            println!("Load SUCCEEDED (Vulnerability Potential).");
            println!("Configured dimensions: 100");
            println!("Index reported dimensions: {}", index_large.dimensions());

            // Check if reported dimensions match config or file
            if index_large.dimensions() == 100 {
                println!("Index thinks it has 100 dimensions (matches config).");
                // This is bad if underlying data is 10.
            } else if index_large.dimensions() == 10 {
                println!("Index thinks it has 10 dimensions (matches file).");
                // This means usearch updated its state from file.
                // But HnswIndex wrapper might still rely on config.dimensions (100).
            }

            // Try searching with dim=100 query
            let query = vec![0.1f32; 100];
            println!("Searching with query len=100...");
            let result = index_large.search(&query, 10);

            match result {
                Ok(matches) => {
                    println!("Search SUCCEEDED with {} matches", matches.len());
                    // If we get matches, we likely read garbage data from the heap.
                    // This confirms the vulnerability (Information Disclosure / Buffer Over-read).
                    panic!("VULNERABILITY CONFIRMED: Search succeeded despite dimension mismatch!");
                }
                Err(e) => {
                    println!("Search FAILED safely: {}", e);
                    // If search fails, it might be due to usearch internal checks.
                    // But loading shouldn't have succeeded in the first place without warning.
                    // We want strict validation.
                    panic!("Load should have failed for dimension mismatch!");
                }
            }
        }
        Err(e) => {
            println!("Load FAILED correctly: {}", e);
            // This is the desired behavior.
        }
    }
}
