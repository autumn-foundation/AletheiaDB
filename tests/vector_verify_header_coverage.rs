use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, Quantization, VectorIndex,
};
use std::fs::File;
use std::io::Write;

#[test]
fn test_verify_header_file_open_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("non_existent.index");
    let config = HnswConfig::default();

    let result = HnswIndex::load(&path, config);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Failed to open index file for verification"));
}

#[test]
fn test_verify_header_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.index");

    // Create a file smaller than 8 bytes
    let mut file = File::create(&path).unwrap();
    file.write_all(&[0u8; 4]).unwrap(); // Only 4 bytes

    let config = HnswConfig::default();

    let result = HnswIndex::load(&path, config);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Failed to read index header"));
}

#[test]
fn test_verify_header_quantization_coverage() {
    let dir = tempfile::tempdir().unwrap();

    // F16
    {
        let path = dir.path().join("f16_valid.index");
        let index = HnswIndexBuilder::new(16, DistanceMetric::Cosine)
            .quantization(Quantization::F16)
            .build()
            .unwrap();
        index.add(NodeId::new(1).unwrap(), &vec![1.0; 16]).unwrap();
        index.save(&path).unwrap();

        let config =
            HnswConfig::new(16, DistanceMetric::Cosine).with_quantization(Quantization::F16);
        let loaded = HnswIndex::load(&path, config);
        assert!(loaded.is_ok());
    }

    // I8
    {
        let path = dir.path().join("i8_valid.index");
        let index = HnswIndexBuilder::new(16, DistanceMetric::Cosine)
            .quantization(Quantization::I8)
            .build()
            .unwrap();
        index.add(NodeId::new(1).unwrap(), &vec![1.0; 16]).unwrap();
        index.save(&path).unwrap();

        let config =
            HnswConfig::new(16, DistanceMetric::Cosine).with_quantization(Quantization::I8);
        let loaded = HnswIndex::load(&path, config);
        assert!(loaded.is_ok());
    }
}
