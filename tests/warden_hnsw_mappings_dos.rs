use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex};
use crc32fast::Hasher;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

#[test]
fn test_hnsw_load_mappings_dos_prevention() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dos_index.usearch");
    let mappings_path = path.with_extension("usearch.mappings");

    // Create a valid index file (minimal)
    // We just need it to exist for HnswIndex::load to proceed to loading mappings
    // usearch index file creation is complex, but we can use the library to create one
    // or just mock it if we knew the format. Using the library is safer.
    // However, HnswIndex::load calls usearch::Index::new() then .load().
    // If the index file is invalid, it will fail before loading mappings.

    // Let's create a real index first.
    {
        let index = HnswIndex::new(HnswConfig::new(4, DistanceMetric::Cosine)).unwrap();
        index.save(&path).unwrap();
    }

    // Now overwrite the mappings file with our malicious one
    let mut file = File::create(&mappings_path).unwrap();

    // Magic: GMAP
    file.write_all(b"GMAP").unwrap();
    // Version: 1
    file.write_all(&[1]).unwrap();

    // Count: 100,000,001 (Just over the 100M limit)
    let count: u64 = 100_000_001;
    file.write_all(&count.to_le_bytes()).unwrap();

    // We need to calculate CRC32 for:
    // Header (GMAP + 1 + count)
    // Data (count * 16 bytes of zeros)
    let mut hasher = Hasher::new();
    hasher.update(b"GMAP");
    hasher.update(&[1]);
    hasher.update(&count.to_le_bytes());

    // Update hasher with zeros
    // We do this in chunks to avoid allocating huge buffer
    let zeros = vec![0u8; 1024 * 1024]; // 1MB buffer
    let mut remaining = count * 16;
    while remaining > 0 {
        let chunk_size = std::cmp::min(remaining, zeros.len() as u64) as usize;
        hasher.update(&zeros[0..chunk_size]);
        remaining -= chunk_size as u64;
    }

    // Set file length to expected size (creating sparse file)
    let header_size = 4 + 1 + 8;
    let data_size = count * 16;
    let footer_size = 4;
    let total_size = header_size + data_size + footer_size;

    file.set_len(total_size).unwrap();

    // Write CRC at the end
    file.seek(SeekFrom::Start(header_size + data_size)).unwrap();
    let crc = hasher.finalize();
    file.write_all(&crc.to_le_bytes()).unwrap();

    // Close file
    drop(file);

    // Try to load
    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));

    // Assert failure
    assert!(
        result.is_err(),
        "Should reject mappings file with excessive count"
    );

    let err = result.unwrap_err();
    let msg = format!("{}", err);

    // We expect a specific error message about the limit
    // Note: This string might need adjustment once we implement the fix
    assert!(
        msg.contains("exceeds maximum allowed") || msg.contains("too large"),
        "Unexpected error message: {}",
        msg
    );
}
