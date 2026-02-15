use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder};
use aletheiadb::utils::Error;
use std::path::Path;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

// Helper to check error types
fn check_error<T>(result: Result<T, Error>, context: &str) {
    match result {
        Ok(_) => {
            panic!("{}: Should not allow excessive dimensions", context);
        }
        Err(e) => {
            println!("{}: Rejected with: {}", context, e);
            if let Error::Vector(ve) = e {
                let msg = ve.to_string();
                if msg.contains("dimensions") && msg.contains("too large") {
                    println!("{}: Successfully rejected with dimensions error", context);
                    return;
                }
                if msg.contains("failed to reserve memory") {
                    panic!(
                        "{}: Vulnerability: Attempted massive allocation (caught by OOM, but should be caught by validation)",
                        context
                    );
                }

                panic!("{}: Rejected but with unexpected error: {}", context, msg);
            } else {
                panic!("{}: Rejected with non-Vector error: {}", context, e);
            }
        }
    }
}

#[test]
fn test_hnsw_build_excessive_dimensions_dos() {
    // Attempt to create an index with massive dimensions via Builder
    // This covers HnswIndexBuilder::build validation
    let massive_dims = 1_000_000_000;

    let result = HnswIndexBuilder::new(massive_dims, DistanceMetric::Cosine).build();

    check_error(result, "build");
}

#[test]
fn test_hnsw_load_excessive_dimensions_dos() {
    // Attempt to load an index with massive dimensions in config
    // This covers HnswIndex::load validation
    let massive_dims = 1_000_000_000;
    let config = HnswConfig::new(massive_dims, DistanceMetric::Cosine);

    // We provide a dummy path. The validation should happen BEFORE file I/O.
    // If it attempts to open the file first, it might fail with NotFound,
    // which would mean the validation isn't strictly "first" (but still safe if it checks config).
    // The implementation puts the check at the very top.
    let result = HnswIndex::load(Path::new("dummy_path.usearch"), config);

    check_error(result, "load");
}

#[test]
fn test_hnsw_mmap_excessive_dimensions_dos() {
    // Attempt to open mmap with a file that claims dimensions > 100,000
    // This covers HnswIndex::open_mmap validation

    // 100,001 dimensions is just over the limit (100,000)
    // It's small enough that usearch should succeed in creating/saving it (approx 400KB per vector)
    // but large enough to trigger our validation.
    let high_dims = 100_001;

    let options = IndexOptions {
        dimensions: high_dims,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity: 16,
        expansion_add: 100,
        expansion_search: 100,
        multi: false,
    };

    // Try to create raw index using usearch directly
    let index = match Index::new(&options) {
        Ok(idx) => idx,
        Err(e) => {
            println!(
                "Skipping mmap test: usearch refused to create {} dim index: {}",
                high_dims, e
            );
            return;
        }
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("high_dim.usearch");
    let path_str = path.to_str().unwrap();

    // Save the index to disk
    if let Err(e) = index.save(path_str) {
        println!("Skipping mmap test: failed to save high dim index: {}", e);
        return;
    }

    // Note: HnswIndex::open_mmap calls load_mappings_with_integrity.
    // We don't create a mappings file, so it will return Ok((..., None)).
    // Then open_mmap proceeds to check dimensions.

    // Now try to open it via HnswIndex::open_mmap
    let result = HnswIndex::open_mmap(&path);

    check_error(result, "open_mmap");
}
