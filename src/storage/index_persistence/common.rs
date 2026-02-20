//! Common utilities for index persistence.
//!
//! Provides generic helpers for saving and loading data with CRC32 checksums.

use bitcode::{Decode, Encode};
use crc32fast::Hasher;
use std::fs;
use std::path::Path;

use super::atomic_write;
use super::error::{IndexPersistenceError, Result};

/// Save encoded data with CRC32 checksum using atomic write.
///
/// Format: `[bitcode_data][crc32_checksum_4_bytes]`
///
/// Uses write-temp-then-rename to prevent corruption on crash.
///
/// # Arguments
///
/// * `data` - The data to serialize and save
/// * `path` - The file path to write to
///
/// # Errors
///
/// Returns an error if serialization or file I/O fails.
pub fn save_encoded_with_crc<T: Encode>(data: &T, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(data);

    // Calculate CRC32 of the encoded data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    // Write data + checksum
    let mut data_with_checksum = encoded;
    data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

    atomic_write(path, &data_with_checksum)
}

/// Load encoded data from disk and validate CRC32 checksum.
///
/// # Arguments
///
/// * `path` - The file path to read from
/// * `max_size` - Maximum allowed file size (DoS protection)
/// * `context` - Context name for error messages (e.g., "Vector index")
///
/// # Errors
///
/// Returns an error if:
/// - File size exceeds `max_size`
/// - File is too small (missing checksum)
/// - CRC32 checksum mismatch
/// - Deserialization fails
pub fn load_encoded_with_crc<T: for<'a> Decode<'a>>(
    path: &Path,
    max_size: u64,
    context: &str,
) -> Result<T> {
    // Check file size before reading to prevent OOM/DoS
    let metadata = fs::metadata(path)?;
    if metadata.len() > max_size {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "{} file size {} exceeds limit {}",
                context,
                metadata.len(),
                max_size
            ),
        });
    }

    let bytes = fs::read(path)?;

    // Check minimum size (must have at least 4 bytes for CRC)
    if bytes.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data, checksum_bytes) = bytes.split_at(bytes.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Verify checksum
    let mut hasher = Hasher::new();
    hasher.update(data);
    let computed_checksum = hasher.finalize();

    if computed_checksum != stored_checksum {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!(
                "CRC32 checksum mismatch: expected {}, got {}",
                stored_checksum, computed_checksum
            )
            .into(),
        });
    }

    // Decode
    let decoded: T = bitcode::decode(data)?;
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_save_load_round_trip() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let data = 42u64;

        // Save
        save_encoded_with_crc(&data, path).unwrap();

        // Load
        let loaded: u64 = load_encoded_with_crc(path, 1024, "Test").unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_checksum_mismatch() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let data = 42u64;

        // Save
        save_encoded_with_crc(&data, path).unwrap();

        // Corrupt file (flip a bit in the data)
        let mut bytes = fs::read(path).unwrap();
        bytes[0] ^= 0xFF; // Flip first byte
        let mut file_rw = fs::File::create(path).unwrap();
        file_rw.write_all(&bytes).unwrap();

        // Load should fail
        let result: Result<u64> = load_encoded_with_crc(path, 1024, "Test");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::Corrupted { .. }
        ));
    }

    #[test]
    fn test_size_limit_exceeded() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let data = vec![0u8; 100]; // 100 bytes + overhead

        // Save
        save_encoded_with_crc(&data, path).unwrap();

        // Load with tiny limit
        let result: Result<Vec<u8>> = load_encoded_with_crc(path, 10, "Test");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::SizeLimitExceeded { .. }
        ));
    }

    #[test]
    fn test_file_too_small() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();

        // Write junk < 4 bytes
        let mut file_rw = fs::File::create(path).unwrap();
        file_rw.write_all(&[1, 2, 3]).unwrap();

        // Load should fail
        let result: Result<u64> = load_encoded_with_crc(path, 1024, "Test");
        assert!(result.is_err());
        match result.unwrap_err() {
            IndexPersistenceError::Corrupted { source, .. } => {
                assert!(source.to_string().contains("File too small"));
            }
            _ => panic!("Expected corrupted error for small file"),
        }
    }
}
