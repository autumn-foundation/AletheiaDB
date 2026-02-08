use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex};
use aletheiadb::index::VectorIndex;
use aletheiadb::utils::error::{Error, VectorError};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use tempfile::tempdir;

#[test]
fn test_load_mappings_sparse_file_dos() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("test_sparse.index");
    let mappings_path = index_path.with_extension("usearch.mappings");

    // Create a dummy index file so HnswIndex::load doesn't fail on index file
    let index = aletheiadb::index::vector::HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    index.save(&index_path).unwrap();

    // Create a sparse mappings file
    let mut file = File::create(&mappings_path).unwrap();

    // Header: MAGIC (4) + VERSION (1) + COUNT (8)
    file.write_all(b"GMAP").unwrap();
    file.write_all(&[1]).unwrap();

    // Count: 10 million
    // 10,000,000 * 16 bytes = 160 MB data
    let count = 10_000_000u64;
    file.write_all(&count.to_le_bytes()).unwrap();

    // Current position: 13 bytes.
    // We need total size: 13 + (count * 16) + 4 (CRC)
    let total_size = 13 + (count * 16) + 4;

    // Make it sparse!
    file.set_len(total_size).unwrap();

    // Calculate CRC for the header + data (zeros)
    let mut hasher = crc32fast::Hasher::new();

    // Header
    hasher.update(b"GMAP");
    hasher.update(&[1]);
    hasher.update(&count.to_le_bytes());

    // Data (zeros)
    // We can simulate updating with zeros efficiently
    let zero_chunk = vec![0u8; 1024 * 1024]; // 1MB chunk
    let mut remaining = count * 16;
    while remaining > 0 {
        let chunk_size = std::cmp::min(remaining, zero_chunk.len() as u64) as usize;
        hasher.update(&zero_chunk[0..chunk_size]);
        remaining -= chunk_size as u64;
    }

    let crc = hasher.finalize();

    // Write CRC at the end
    file.seek(SeekFrom::Start(total_size - 4)).unwrap();
    file.write_all(&crc.to_le_bytes()).unwrap();

    let start = std::time::Instant::now();
    let result = HnswIndex::load(&index_path, HnswConfig::new(4, DistanceMetric::Cosine));
    let duration = start.elapsed();

    println!("Time taken: {:?}", duration);

    // Expect error
    assert!(result.is_err(), "Sparse file load should fail (duplicate NodeIds)");
     match result {
        Err(Error::Vector(VectorError::IndexError(msg))) => {
            assert!(
                msg.contains("Duplicate NodeId"),
                "Expected Duplicate NodeId error, got: {}",
                msg
            );
        }
        _ => panic!("Expected IndexError, got {:?}", result),
    }
}
