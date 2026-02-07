//! Compression utilities for cold storage.
//!
//! This module provides a unified interface for compressing and decompressing data
//! using various algorithms with optional CRC32 checksums. It is used by all cold
//! storage backends to ensure consistent data handling.

use crate::storage::redb_cold_storage::ColdStorageConfig;
use crate::utils::error::{Result, StorageError};
use std::io::Read;

/// Maximum allowed decompressed size for generic operations (DoS protection).
/// Set to 256MB by default, which should be sufficient for most individual items
/// (versions, property maps, etc.) while preventing OOM attacks.
pub const MAX_DECOMPRESSED_SIZE: usize = 256 * 1024 * 1024;

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
///
/// # DoS Protection
///
/// This function enforces a maximum decompressed size of [`MAX_DECOMPRESSED_SIZE`]
/// to prevent "zip bomb" attacks. If the data exceeds this limit,
/// `StorageError::CapacityExceeded` is returned.
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
        Some(_) => decompress_with_limit(data_to_decompress, MAX_DECOMPRESSED_SIZE),
        None => Ok(data_to_decompress.to_vec()),
    }
}

/// Decompress zstd data with a strict output size limit.
///
/// This function uses a streaming decoder to prevent unbounded allocation
/// attacks (e.g., "zip bombs") where a small compressed payload expands
/// to fill all available memory.
///
/// # Arguments
///
/// * `data` - The compressed data
/// * `limit` - The maximum allowed decompressed size in bytes
///
/// # Errors
///
/// Returns `StorageError::CapacityExceeded` if the decompressed size exceeds `limit`.
/// Returns `StorageError::IoError` if decompression fails.
pub fn decompress_with_limit(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    // Create a streaming decoder
    let decoder = zstd::stream::read::Decoder::new(data).map_err(|e| {
        crate::utils::error::Error::Storage(StorageError::io_error(format!(
            "Failed to create zstd decoder: {}",
            e
        )))
    })?;

    // Use take() to limit the output size.
    // We limit to `limit + 1` to detect if we exceeded the limit.
    // Casting limit to u64 is safe on 32-bit and 64-bit systems.
    let mut limited_reader = decoder.take((limit as u64).saturating_add(1));

    let mut buffer = Vec::new();
    limited_reader.read_to_end(&mut buffer).map_err(|e| {
        crate::utils::error::Error::Storage(StorageError::io_error(format!(
            "Decompression failed: {}",
            e
        )))
    })?;

    // Debugging output (only in tests)
    #[cfg(test)]
    {
        // println!("Decompressed {} bytes with limit {}", buffer.len(), limit);
    }

    if buffer.len() > limit {
        return Err(crate::utils::error::Error::Storage(
            StorageError::CapacityExceeded {
                resource: "decompressed_size".to_string(),
                current: buffer.len(),
                limit,
            },
        ));
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
        // Data smaller than limit
        let data = [0u8; 100];
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        let decompressed = decompress_with_limit(&compressed, 200).unwrap();
        assert_eq!(decompressed.len(), 100);
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_decompress_with_limit_exceeded() {
        // Data larger than limit
        let data = [0u8; 200];
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        // Limit to 100 bytes
        let result = decompress_with_limit(&compressed, 100);
        assert!(result.is_err());

        match result.unwrap_err() {
            crate::utils::error::Error::Storage(StorageError::CapacityExceeded {
                resource,
                current,
                limit,
            }) => {
                assert_eq!(resource, "decompressed_size");
                assert!(current > 100);
                assert_eq!(limit, 100);
            }
            e => panic!("Unexpected error type: {:?}", e),
        }
    }

    #[test]
    fn test_decompress_with_limit_exact() {
        // Data exactly at limit
        let data = [0u8; 100];
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        let decompressed = decompress_with_limit(&compressed, 100).unwrap();
        assert_eq!(decompressed.len(), 100);
        assert_eq!(decompressed, data);
    }
}
