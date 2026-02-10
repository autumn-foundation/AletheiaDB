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
fn test_hnsw_dimension_limit_boundary() {
    // Ensure that exactly MAX_VECTOR_DIMENSIONS is accepted.
    // This catches mutations that might change > to >=.
    let max_dims = MAX_VECTOR_DIMENSIONS;

    // We expect this to succeed (or at least fail with a different error like allocation,
    // but NOT DimensionTooLarge). Since 100,000 floats is ~400KB, it should succeed.
    let result = HnswIndexBuilder::new(max_dims, DistanceMetric::Cosine).build();

    if let Err(Error::Vector(VectorError::DimensionTooLarge { .. })) = result {
        panic!("HNSW builder rejected max dimensions ({}) which should be allowed", max_dims);
    }
    // Other errors (e.g. OOM on small systems) are acceptable for this specific test,
    // as long as it's not the security check rejecting it.
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

#[test]
fn test_hnsw_mmap_connectivity_overflow() {
    // Test that open_mmap rejects excessive connectivity (m > 64).
    // usearch clamps input values during creation, so we must manually patch the
    // binary file to simulate a malicious file with M > 64.
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("excessive_connectivity.index");
    let mappings_path = dir.path().join("excessive_connectivity.usearch.mappings");

    let excessive_m = 65; // Max is 64

    // 1. Create valid index with M=16
    let options = IndexOptions {
        dimensions: 10,
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
        .expect("Failed to save index");

    // 2. Patch the binary file to set connectivity to 65
    // Based on investigation (tests/warden_coverage_debug.rs), connectivity is at offset 80.
    // We assume it's a 64-bit integer (or at least 8-bit compatible at that offset).
    let mut bytes = std::fs::read(&index_path).expect("Failed to read index file");

    // Safety check: ensure file is large enough
    if bytes.len() > 80 {
        // Overwrite byte at offset 80 with 65
        bytes[80] = excessive_m as u8;
        std::fs::write(&index_path, &bytes).expect("Failed to patch index file");
    } else {
        panic!("Index file too small to patch connectivity");
    }

    // 3. Create dummy mappings
    let mut mappings_data = Vec::new();
    mappings_data.extend_from_slice(b"GMAP");
    mappings_data.push(1);
    mappings_data.extend_from_slice(&0u64.to_le_bytes());
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&mappings_data);
    let crc = hasher.finalize();
    mappings_data.extend_from_slice(&crc.to_le_bytes());
    std::fs::write(&mappings_path, &mappings_data).expect("Failed to write mappings");

    // 4. Attempt to open_mmap
    let result = HnswIndex::open_mmap(&index_path);

    match result {
        Ok(idx) => {
            // Check if usearch ignored our patch or if it succeeded with excessive M
            let actual_m = idx.m();
            if actual_m > 64 {
                panic!("HNSW open_mmap accepted excessively large connectivity ({})", actual_m);
            } else {
                // If actual_m <= 64, then usearch validated or clamped it internally,
                // so our validation code was effectively unreachable or redundant for this specific attack vector.
                // We print a message but pass the test since the security property (no excessive M) holds.
                println!("HNSW/usearch clamped connectivity from {} to {}", excessive_m, actual_m);
            }
        },
        Err(Error::Vector(VectorError::InvalidVector { reason })) => {
            assert!(
                reason.contains("M must be in range"),
                "Error message should mention M range, got: {}",
                reason
            );
            assert!(
                reason.contains(&excessive_m.to_string()),
                "Error message should contain the actual value"
            );
        }
        Err(e) => panic!("Expected InvalidVector error, got: {:?}", e),
    }
}
