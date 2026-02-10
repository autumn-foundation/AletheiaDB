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
    // HnswIndex::load should detect the actual dimensions from the file (4),
    // update the config, and validation should pass (4 == 4).

    let config = HnswConfig::new(128, DistanceMetric::Cosine)
        .with_custom_metric("spy", |a, b| {
             // If we get here, and len is 128, we are reading OOB (bad!)
             // If len is 4, we are safe (good!)
             if a.len() != 4 {
                 panic!("CRITICAL: Metric wrapper called with wrong dimension: {}", a.len());
             }
             0.0
        });

    let index = aletheiadb::index::vector::HnswIndex::load(&path, config).expect("Load should succeed by adapting to actual index dimensions");

    println!("Index reported dimensions: {}", index.dimensions());
    assert_eq!(index.dimensions(), 4, "Index should report actual dimensions (4), not config dimensions (128)");

    // 3. Try to add a 4-dim vector
    // This should succeed.
    let vec_4 = vec![0.1f32; 4];
    match index.add(NodeId::new(2).unwrap(), &vec_4) {
        Ok(_) => println!("Add 4-dim success"),
        Err(e) => {
            panic!("HnswIndex failed to accept valid vector for underlying index: {}", e);
        }
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

    // Config claims 128 (matches tampered metadata)
    let config = HnswConfig::new(128, DistanceMetric::Cosine);

    // Load should FAIL because HnswIndex detects the actual index dimension is 4,
    // and refuses to load it because metadata (128) contradicts reality (4).
    let result = aletheiadb::index::vector::HnswIndex::load(&path, config);

    assert!(result.is_err(), "Load should fail due to metadata mismatch with actual index");

    match result {
        Err(Error::Vector(aletheiadb::utils::error::VectorError::IndexError(msg))) => {
            assert!(msg.contains("Index dimension mismatch"), "Error message should mention dimension mismatch. Got: {}", msg);
            assert!(msg.contains("expected 4"), "Error should expect 4 (actual index dim)");
            assert!(msg.contains("found 128"), "Error should find 128 (metadata dim)");
        },
        _ => panic!("Expected VectorError::IndexError"),
    }
}
