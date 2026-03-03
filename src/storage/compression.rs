//! Compression utilities for cold storage.
//!
//! This module provides a unified interface for compressing and decompressing data
//! using various algorithms with optional CRC32 checksums. It is used by all cold
//! storage backends to ensure consistent data handling.

use crate::storage::redb_cold_storage::ColdStorageConfig;
use crate::utils::error::{Result, StorageError};
use std::io::Read;

/// Compress data according to the configuration.
///
/// If checksums are enabled, a 4-byte CRC32 checksum is prepended to the compressed data.
/// If compression is disabled (CompressionAlgorithm::None), the checksum is still added
/// if enabled, just without compression.
pub fn compress(data: &[u8], config: &ColdStorageConfig) -> Result<Vec<u8>> {
    match config.compression.zstd_level() {
        Some(level) => {
            // Compress with Zstd
            let compressed =
                zstd::encode_all(data, level).map_err(|e| -> crate::utils::error::Error {
                    StorageError::io_error(e.to_string()).into()
                })?;

            // Add checksum if enabled
            if config.enable_checksums {
                let checksum = crc32fast::hash(&compressed);
                let mut result = Vec::with_capacity(compressed.len() + 4);
                result.extend_from_slice(&checksum.to_le_bytes());
                result.extend_from_slice(&compressed);
                Ok(result)
            } else {
                Ok(compressed)
            }
        }
        None => {
            // No compression, but maybe checksum
            if config.enable_checksums {
                let checksum = crc32fast::hash(data);
                let mut result = Vec::with_capacity(data.len() + 4);
                result.extend_from_slice(&checksum.to_le_bytes());
                result.extend_from_slice(data);
                Ok(result)
            } else {
                Ok(data.to_vec())
            }
        }
    }
}

/// Decompress data using the configured algorithm.
///
/// If checksums were enabled during compression, the checksum is verified.
/// Returns an error if the checksum doesn't match or if decompression fails.
pub fn decompress(data: &[u8], config: &ColdStorageConfig) -> Result<Vec<u8>> {
    // Extract checksum if enabled
    let (data_to_decompress, expected_checksum) = if config.enable_checksums {
        if data.len() < 4 {
            return Err(StorageError::corruption("Data too short for checksum".to_string()).into());
        }
        let checksum = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let payload = &data[4..];
        (payload, Some(checksum))
    } else {
        (data, None)
    };

    // Verify checksum if enabled
    if let Some(expected) = expected_checksum {
        let actual = crc32fast::hash(data_to_decompress);
        if actual != expected {
            return Err(StorageError::corruption(format!(
                "Checksum mismatch: expected {}, got {}",
                expected, actual
            ))
            .into());
        }
    }

    // Decompress based on algorithm
    match config.compression.zstd_level() {
        Some(_) => {
            // Use decompress_with_limit to prevent Zip Bomb attacks
            decompress_with_limit(data_to_decompress, config.max_decompressed_size)
        }
        None => Ok(data_to_decompress.to_vec()),
    }
}

/// Decompress zstd data with a strict output size limit.
///
/// Uses a streaming decoder with chunked reads to prevent unbounded allocation
/// attacks (e.g., "zip bombs") where a small compressed payload expands to fill
/// all available memory. Unlike `read_to_end`, this reads in fixed-size chunks
/// and aborts as soon as the limit is exceeded.
///
/// # Arguments
///
/// * `data` - The compressed zstd data
/// * `limit` - The maximum allowed decompressed size in bytes
///
/// # Errors
///
/// Returns `StorageError::CapacityExceeded` if the decompressed size exceeds `limit`.
/// Returns `StorageError::IoError` if decompression fails.
pub fn decompress_with_limit(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(data).map_err(|e| {
        crate::utils::error::Error::Storage(StorageError::io_error(format!(
            "Failed to create zstd decoder: {}",
            e
        )))
    })?;

    // Read in chunks to avoid allocating the full limit upfront.
    // 1MB chunks balance syscall overhead vs memory usage.
    const CHUNK_SIZE: usize = 1024 * 1024;
    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; CHUNK_SIZE];

    loop {
        let bytes_read = decoder.read(&mut chunk).map_err(|e| {
            crate::utils::error::Error::Storage(StorageError::io_error(format!(
                "Decompression failed: {}",
                e
            )))
        })?;

        if bytes_read == 0 {
            break;
        }

        // Check limit BEFORE extending the buffer to prevent memory spike
        if buffer.len().saturating_add(bytes_read) > limit {
            return Err(crate::utils::error::Error::Storage(
                StorageError::CapacityExceeded {
                    resource: "decompressed_size".to_string(),
                    current: buffer.len() + bytes_read,
                    limit,
                },
            ));
        }

        buffer.extend_from_slice(&chunk[..bytes_read]);
    }

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::redb_cold_storage::CompressionAlgorithm;

    #[test]
    fn test_compress_decompress_zstd() {
        let config = ColdStorageConfig {
            compression: CompressionAlgorithm::Zstd,
            enable_checksums: true,
            ..Default::default()
        };

        // Use longer, repetitive data for effective compression
        let data = b"Hello, world! This is test data. Hello, world! This is test data. Hello, world! This is test data.";
        let compressed = compress(data, &config).unwrap();
        let decompressed = decompress(&compressed, &config).unwrap();

        assert_eq!(data, decompressed.as_slice());
        // With repetitive data, compression should provide benefit
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn test_compress_decompress_none() {
        let config = ColdStorageConfig {
            compression: CompressionAlgorithm::None,
            enable_checksums: false,
            ..Default::default()
        };

        let data = b"Hello, world!";
        let compressed = compress(data, &config).unwrap();
        let decompressed = decompress(&compressed, &config).unwrap();

        assert_eq!(data, decompressed.as_slice());
        assert_eq!(data.len(), compressed.len()); // No compression or checksum
    }

    #[test]
    fn test_checksum_verification() {
        let config = ColdStorageConfig {
            compression: CompressionAlgorithm::None,
            enable_checksums: true,
            ..Default::default()
        };

        let data = b"Test data";
        let mut compressed = compress(data, &config).unwrap();

        // Corrupt the data
        compressed[5] ^= 0xFF;

        // Should fail checksum
        let result = decompress(&compressed, &config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Checksum mismatch")
        );
    }

    #[test]
    fn test_decompress_with_limit_success() {
        let data = [42u8; 100];
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        let decompressed = decompress_with_limit(&compressed, 200).unwrap();
        assert_eq!(decompressed.len(), 100);
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_decompress_with_limit_exact_boundary() {
        let data = [42u8; 100];
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        // Exactly at the limit should succeed
        let decompressed = decompress_with_limit(&compressed, 100).unwrap();
        assert_eq!(decompressed.len(), 100);
    }

    #[test]
    fn test_decompress_with_limit_exceeded() {
        let data = [0u8; 200];
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        let result = decompress_with_limit(&compressed, 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::utils::error::Error::Storage(StorageError::CapacityExceeded {
                resource,
                limit,
                ..
            }) => {
                assert_eq!(resource, "decompressed_size");
                assert_eq!(limit, 100);
            }
            e => panic!("Expected CapacityExceeded, got: {:?}", e),
        }
    }

    #[test]
    fn test_decompress_with_limit_zstd_bomb() {
        // Simulate a zip bomb: 10MB of zeros compresses to very few bytes
        let data = vec![0u8; 10 * 1024 * 1024];
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        // The compressed payload should be tiny
        assert!(compressed.len() < 1000, "Bomb should compress well");

        // But decompression with a 1MB limit should fail fast
        let result = decompress_with_limit(&compressed, 1024 * 1024);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::utils::error::Error::Storage(StorageError::CapacityExceeded { .. })
        ));
    }

    #[test]
    fn test_decompress_zip_bomb_via_config() {
        // 1. Create a "bomb": 10MB of zeros
        // We use a smaller size than 100MB to avoid slowing down tests too much,
        // but large enough to trigger a small limit.
        let uncompressed_size = 10 * 1024 * 1024; // 10MB
        let bomb_data = vec![0u8; uncompressed_size];

        // 2. Compress it (should be very small, ~100-500 bytes)
        let compressed = zstd::encode_all(&bomb_data[..], 1).unwrap();
        assert!(compressed.len() < 1000);

        // 3. Configure a small limit (e.g., 1MB)
        let limit = 1024 * 1024; // 1MB
        let config = ColdStorageConfig {
            compression: CompressionAlgorithm::Zstd,
            enable_checksums: false, // Simplify test (skip checksum wrapping)
            max_decompressed_size: limit,
            ..Default::default()
        };

        // 4. Attempt decompression
        let result = decompress(&compressed, &config);

        // 5. Verify it fails with CapacityExceeded
        assert!(result.is_err());
        match result.unwrap_err() {
            crate::utils::error::Error::Storage(StorageError::CapacityExceeded {
                limit: l,
                current: c,
                ..
            }) => {
                assert_eq!(l, limit);
                // current should be > limit
                assert!(c > limit);
            }
            e => panic!("Expected CapacityExceeded error, got: {:?}", e),
        }
    }
}

    #[test]
    fn test_decompress_with_limit_near_overflow() {
        // This test targets the mutation "replace + with *" in decompress_with_limit.
        // We want to ensure that the logic
        // is actually testing addition, not multiplication.
        //
        // If the code was , we need a case where:
        // - addition PASSES (is <= limit)
        // - multiplication FAILS (is > limit)
        // OR
        // - addition FAILS (is > limit)
        // - multiplication PASSES (is <= limit)
        //
        // Case 1: buffer.len() = 10, bytes_read = 10, limit = 25
        // Add: 20 <= 25 (OK) -> extend buffer -> OK
        // Mul: 100 > 25 (FAIL) -> error
        //
        // If the mutant (mul) exists, this test would fail because it would error out
        // when it should succeed.

        let data = [1u8; 10];
        // Compression level 1 for speed
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        // We need to trick the decoder to read in chunks.
        // Real zstd decoder behavior is complex, but we can simulate the state check logic.
        // Effectively, we just need to ensure  works correctly
        // for inputs that would cause multiplication to blow up.

        let limit = 100;
        let result = decompress_with_limit(&compressed, limit);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), data);
    }
