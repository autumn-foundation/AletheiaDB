use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

#[test]
fn test_warden_mappings_dos_large_file() {
    // 🛡️ Warden: Test DoS vector via huge mappings file
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("test.index");
    let mappings_path = dir.path().join("test.usearch.mappings");

    // Create a dummy index file first so load doesn't fail on index missing
    use aletheiadb::index::vector::HnswIndexBuilder;
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    index.save(&index_path).unwrap();

    // Now overwrite the mappings file with our huge sparse one
    let mut file = File::create(&mappings_path).unwrap();
    // Seek to 2.1GB
    file.seek(SeekFrom::Start(2_200_000_000)).unwrap();
    file.write_all(&[0]).unwrap(); // Write one byte to establish size

    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let result = HnswIndex::load(&index_path, config);

    assert!(result.is_err(), "Should reject huge mappings file");

    match result {
        Err(aletheiadb::utils::Error::Vector(
            aletheiadb::utils::error::VectorError::IndexError(msg),
        )) => {
            assert!(
                msg.contains("Mappings file too large") || msg.contains("too large"),
                "Unexpected error message: {}",
                msg
            );
        }
        _ => panic!("Expected IndexError for file size, got {:?}", result),
    }
}
