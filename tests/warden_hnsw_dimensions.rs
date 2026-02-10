use aletheiadb::core::property::MAX_VECTOR_DIMENSIONS;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder};
use aletheiadb::utils::error::{Error, VectorError};
use std::path::Path;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

#[test]
fn test_hnsw_dimension_overflow_rejection() {
    // Attempt to build an index with dimensions exceeding the maximum allowed.
    // MAX_VECTOR_DIMENSIONS is 100,000.
    // This protects against OOM DoS and potential UB in unsafe code blocks
    // that rely on dimensions * size_of<f32> fitting in isize::MAX.

    let excessive_dims = MAX_VECTOR_DIMENSIONS + 1;

    let result = HnswIndexBuilder::new(excessive_dims, DistanceMetric::Cosine).build();

    match result {
        Ok(_) => panic!(
            "HNSW builder accepted excessively large dimensions ({})",
            excessive_dims
        ),
        Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension,
            max_allowed,
        })) => {
            assert_eq!(dimension, excessive_dims);
            assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
        }
        Err(e) => panic!("Expected DimensionTooLarge error, got: {:?}", e),
    }
}

#[test]
fn test_hnsw_load_dimension_overflow() {
    // Attempt to load an index with a config specifying excessively large dimensions.
    let excessive_dims = MAX_VECTOR_DIMENSIONS + 1;
    let config = HnswConfig::new(excessive_dims, DistanceMetric::Cosine);
    let dummy_path = Path::new("dummy_path.usearch");

    let result = HnswIndex::load(dummy_path, config);

    match result {
        Ok(_) => panic!(
            "HNSW load accepted excessively large dimensions ({})",
            excessive_dims
        ),
        Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension,
            max_allowed,
        })) => {
            assert_eq!(dimension, excessive_dims);
            assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
        }
        Err(e) => panic!("Expected DimensionTooLarge error, got: {:?}", e),
    }
}

#[test]
fn test_hnsw_mmap_dimension_overflow() {
    // Create a temporary directory for our test files
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("malicious.index");
    let mappings_path = dir.path().join("malicious.usearch.mappings");

    let excessive_dims = MAX_VECTOR_DIMENSIONS + 1;

    // 1. Create a "malicious" index using usearch directly (bypassing HnswIndexBuilder checks)
    let options = IndexOptions {
        dimensions: excessive_dims,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity: 16,
        expansion_add: 100,
        expansion_search: 100,
        multi: false,
    };

    let index = Index::new(&options).expect("Failed to create usearch index directly");
    index
        .save(index_path.to_str().unwrap())
        .expect("Failed to save malicious index");

    // 2. Create a dummy mappings file
    let mut mappings_data = Vec::new();
    mappings_data.extend_from_slice(b"GMAP"); // Magic
    mappings_data.push(1); // Version 1
    mappings_data.extend_from_slice(&0u64.to_le_bytes()); // Count 0

    // Calculate CRC
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&mappings_data);
    let crc = hasher.finalize();
    mappings_data.extend_from_slice(&crc.to_le_bytes());

    std::fs::write(&mappings_path, &mappings_data).expect("Failed to write mappings file");

    // 3. Attempt to open_mmap the malicious index
    let result = HnswIndex::open_mmap(&index_path);

    match result {
        Ok(_) => panic!(
            "HNSW open_mmap accepted index with excessively large dimensions ({})",
            excessive_dims
        ),
        Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension,
            max_allowed,
        })) => {
            assert_eq!(dimension, excessive_dims);
            assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
        }
        Err(e) => panic!("Expected DimensionTooLarge error, got: {:?}", e),
    }
}
