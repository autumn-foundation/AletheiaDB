use aletheiadb::storage::compression::{compress, decompress, decompress_with_limit};
use aletheiadb::storage::redb_cold_storage::{ColdStorageConfig, CompressionAlgorithm};

#[test]
fn test_config_mismatch_checksum_enabled_but_data_has_none() {
    let data = b"some data";

    // 1. Write without checksum
    let config_write = ColdStorageConfig {
        compression: CompressionAlgorithm::None,
        enable_checksums: false,
        ..Default::default()
    };
    let compressed = compress(data, &config_write).unwrap();

    // 2. Read with checksum expected
    let config_read = ColdStorageConfig {
        compression: CompressionAlgorithm::None,
        enable_checksums: true,
        ..Default::default()
    };

    let result = decompress(&compressed, &config_read);

    // Should fail because data is likely too short to contain CRC + payload
    // or CRC check will fail
    assert!(
        result.is_err(),
        "Should fail when expecting checksum but none present"
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Data too short") || err.contains("Checksum mismatch"));
}

#[test]
fn test_config_mismatch_checksum_disabled_but_data_has_one_zstd() {
    let data = b"some repetitive data to compress some repetitive data to compress";

    // 1. Write WITH checksum + Zstd
    let config_write = ColdStorageConfig {
        compression: CompressionAlgorithm::Zstd,
        enable_checksums: true,
        ..Default::default()
    };
    let compressed = compress(data, &config_write).unwrap();

    // 2. Read WITHOUT checksum expected (expects raw Zstd)
    let config_read = ColdStorageConfig {
        compression: CompressionAlgorithm::Zstd, // Still Zstd
        enable_checksums: false,                 // But no checksum
        ..Default::default()
    };

    let result = decompress(&compressed, &config_read);

    // Should fail because Zstd decoder sees CRC bytes as header
    assert!(
        result.is_err(),
        "Should fail when Zstd decoder sees CRC header"
    );
    // Zstd error usually says "Frame Format Error" or similar, wrapped in IO error
}

#[test]
fn test_config_mismatch_checksum_disabled_but_data_has_one_none() {
    let data = b"raw data";

    // 1. Write WITH checksum + No compression
    let config_write = ColdStorageConfig {
        compression: CompressionAlgorithm::None,
        enable_checksums: true,
        ..Default::default()
    };
    let compressed = compress(data, &config_write).unwrap();

    // 2. Read WITHOUT checksum expected
    let config_read = ColdStorageConfig {
        compression: CompressionAlgorithm::None,
        enable_checksums: false,
        ..Default::default()
    };

    // This succeeds in decompression (just returns bytes)
    let result = decompress(&compressed, &config_read);
    assert!(result.is_ok(), "Decompression succeeds (just pass-through)");

    let decompressed = result.unwrap();
    // But the data is corrupted (has CRC prefix)
    assert_ne!(decompressed, data, "Data should contain extra bytes");
    assert_eq!(decompressed.len(), data.len() + 4);

    // Verify the prefix is the checksum (4 bytes)
    // We don't need to depend on bitcode to prove it's corrupted for usage.
    // The length mismatch alone is sufficient proof of corruption.
}

#[test]
fn test_decompress_with_limit_on_checksummed_data() {
    let data = b"test data for limit";

    // Write with checksum + Zstd
    let config_write = ColdStorageConfig {
        compression: CompressionAlgorithm::Zstd,
        enable_checksums: true,
        ..Default::default()
    };
    let compressed = compress(data, &config_write).unwrap();

    // Try decompress_with_limit (expects raw Zstd)
    let result = decompress_with_limit(&compressed, 1024);

    // Should fail
    assert!(
        result.is_err(),
        "decompress_with_limit should fail on checksummed data"
    );
}
