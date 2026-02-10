use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric, HnswConfig};
use aletheiadb::index::VectorIndex;
use aletheiadb::core::id::NodeId;
use aletheiadb::utils::Error;

#[test]
fn test_havoc_dimension_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.usearch");

    // 1. Create a valid index with dimension 4
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.save(&path).unwrap();
    }

    // 2. Load it with dimension 128 (mismatched config)
    // We do NOT tamper with metadata. The metadata correctly says 4.
    // The config incorrectly says 128.
    // HnswIndex::load should REJECT this because the file dimensions (4) do not match
    // the expected configuration (128). This prevents inconsistent state.

    let config = HnswConfig::new(128, DistanceMetric::Cosine)
        .with_custom_metric("spy", |_a, _b| {
             0.0
        });

    let result = aletheiadb::index::vector::HnswIndex::load(&path, config);

    assert!(result.is_err(), "Load should fail due to dimension mismatch between config and file");

    match result {
        Err(Error::Vector(aletheiadb::utils::error::VectorError::IndexError(msg))) => {
            assert!(msg.contains("Index dimension mismatch"), "Error message should mention dimension mismatch. Got: {}", msg);
            assert!(msg.contains("expected 128"), "Error should expect 128 (config)");
            assert!(msg.contains("found 4"), "Error should find 4 (file)");
        },
        _ => panic!("Expected VectorError::IndexError, got {:?}", result),
    }
}

#[test]
fn test_havoc_tampered_metadata_detection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tampered.usearch");

    // 1. Create a valid index with dimension 4
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.save(&path).unwrap();
    }

    // 2. Tamper with metadata to claim dimension 128
    let mappings_path = path.with_extension("usearch.mappings");
    let mut data = std::fs::read(&mappings_path).unwrap();

    // V2 Format: Magic(4) + Ver(1) + Dims(8) ...
    let dims_offset = 5;
    let new_dims: u64 = 128;
    data[dims_offset..dims_offset+8].copy_from_slice(&new_dims.to_le_bytes());

    // Fix CRC
    let crc_offset = data.len() - 4;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&data[..crc_offset]);
    let new_crc = hasher.finalize();
    data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

    std::fs::write(&mappings_path, &data).unwrap();

    // Config correctly claims 4 (matches actual index), but metadata claims 128.
    // This tests that we verify metadata integrity even if config matches index.
    let config = HnswConfig::new(4, DistanceMetric::Cosine);

    // Load should FAIL because metadata (128) contradicts config/index (4).
    let result = aletheiadb::index::vector::HnswIndex::load(&path, config);

    assert!(result.is_err(), "Load should fail due to metadata mismatch");

    match result {
        Err(Error::Vector(aletheiadb::utils::error::VectorError::IndexError(msg))) => {
            assert!(msg.contains("Index dimension mismatch"), "Error message should mention dimension mismatch. Got: {}", msg);
            assert!(msg.contains("expected 4"), "Error should expect 4 (config/index)");
            assert!(msg.contains("found 128"), "Error should find 128 (metadata)");
        },
        _ => panic!("Expected VectorError::IndexError, got {:?}", result),
    }
}
