use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex};
use aletheiadb::core::error::{Error, VectorError};
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

#[test]
fn test_load_mappings_huge_count_small_file() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("test.index");
    let mappings_path = index_path.with_extension("usearch.mappings");

    // Create a valid index file first using the builder
    let index = aletheiadb::index::vector::HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    index.save(&index_path).unwrap();

    // Now overwrite the mappings file with a malicious one
    // Malicious file: Valid header claiming 1 billion entries, but file is actually tiny.

    let mut file = File::create(&mappings_path).unwrap();

    // Magic (4 bytes)
    file.write_all(b"GMAP").unwrap();
    // Version (1 byte)
    file.write_all(&[1]).unwrap();

    // Huge count: 1 billion (8 bytes)
    let count = 1_000_000_000u64;
    file.write_all(&count.to_le_bytes()).unwrap();

    // Write just a little bit of data (fake CRC/data)
    // 100 bytes is not enough for 1 billion entries
    file.write_all(&[0u8; 100]).unwrap();

    // With the fix, load() should check file.metadata().len() against expected size derived from count
    // and fail fast with "Mapping file size mismatch" before attempting to read the data.
    // Without the fix, it might try to read ~16GB and OOM, or read all data and then fail check.
    // The key behavior we verify is the specific error message "Mapping file size mismatch".

    let result = HnswIndex::load(&index_path, HnswConfig::new(4, DistanceMetric::Cosine));

    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::IndexError(msg))) => {
            assert!(
                msg.contains("Mapping file size mismatch")
                    || msg.contains("exceeds maximum allowed"),
                "Expected size mismatch or max limit error, got: {}",
                msg
            );
        }
        _ => panic!("Expected IndexError, got {:?}", result),
    }
}

#[test]
fn test_load_mappings_truncated_data() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("test_trunc.index");
    let mappings_path = index_path.with_extension("usearch.mappings");

    let index = aletheiadb::index::vector::HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    index.save(&index_path).unwrap();

    let mut file = File::create(&mappings_path).unwrap();
    file.write_all(b"GMAP").unwrap();
    file.write_all(&[1]).unwrap();

    // Count: 10
    let count = 10u64;
    file.write_all(&count.to_le_bytes()).unwrap();

    // Write 5 entries (5 * 16 bytes = 80 bytes)
    let data = vec![0u8; 80];
    file.write_all(&data).unwrap();

    // Total written: 4 + 1 + 8 + 80 = 93 bytes.
    // Expected: 4 + 1 + 8 + 160 + 4 = 177 bytes.
    // Mismatch!

    let result = HnswIndex::load(&index_path, HnswConfig::new(4, DistanceMetric::Cosine));

    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::IndexError(msg))) => {
            assert!(
                msg.contains("Mapping file size mismatch"),
                "Expected size mismatch error, got: {}",
                msg
            );
        }
        _ => panic!("Expected IndexError, got {:?}", result),
    }
}
