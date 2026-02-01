//! Compression utilities for cold storage.
//!
//! This module provides a unified interface for compressing and decompressing data
//! using various algorithms with optional CRC32 checksums. It is used by all cold
//! storage backends to ensure consistent data handling.

use crate::storage::redb_cold_storage::ColdStorageConfig;
use crate::utils::error::{Result, StorageError};

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
            zstd::decode_all(data_to_decompress).map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(e.to_string()).into()
            })
        }
        None => Ok(data_to_decompress.to_vec()),
    }
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
}
