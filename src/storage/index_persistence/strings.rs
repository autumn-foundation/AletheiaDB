//! String interner persistence.

use crate::core::GLOBAL_INTERNER;
use std::fs;
use std::path::Path;

use crc32fast::Hasher;

use super::error::{IndexPersistenceError, Result};
use super::formats::StringInternerData;
use super::{INTERNER_MAGIC, MANIFEST_VERSION};

/// Save the global string interner to disk with CRC32 checksum using atomic write.
///
/// Format: `[bitcode_data][crc32_checksum_4_bytes]`
///
/// Uses write-temp-then-rename to prevent corruption on crash.
pub fn save_string_interner(path: &Path) -> Result<()> {
    let strings = GLOBAL_INTERNER.get_all_strings();

    let data = StringInternerData {
        magic: INTERNER_MAGIC,
        version: MANIFEST_VERSION,
        string_count: strings.len() as u64,
        strings,
    };

    let encoded = bitcode::encode(&data);

    // Calculate CRC32 of the encoded data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    // Write data + checksum
    let mut data_with_checksum = encoded;
    data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

    super::atomic_write(path, &data_with_checksum)?;

    Ok(())
}

/// Load the string interner from disk and validate CRC32 checksum.
pub fn load_string_interner(path: &Path) -> Result<StringInternerData> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > super::MAX_STRING_INTERNER_FILE_SIZE {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "String interner file size {} exceeds limit {}",
                metadata.len(),
                super::MAX_STRING_INTERNER_FILE_SIZE
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

    // Decode and validate
    let data: StringInternerData = bitcode::decode(data)?;

    // Validate magic bytes
    if data.magic != INTERNER_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: INTERNER_MAGIC,
            got: data.magic,
        });
    }

    // Validate version
    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    // Validate string count to prevent DoS via memory exhaustion
    if data.string_count > super::MAX_STRING_COUNT {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "String count {} exceeds maximum allowed count {}",
                data.string_count,
                super::MAX_STRING_COUNT
            ),
        });
    }

    // Validate individual string lengths to prevent DoS
    for s in &data.strings {
        if s.len() > super::MAX_STRING_LENGTH {
            return Err(IndexPersistenceError::SizeLimitExceeded {
                message: format!(
                    "String length {} exceeds maximum allowed length {}",
                    s.len(),
                    super::MAX_STRING_LENGTH
                ),
            });
        }
    }

    Ok(data)
}

/// Restore GLOBAL_INTERNER from persisted data.
///
/// This must be called before loading any other indexes since they
/// reference string indices.
pub fn restore_string_interner(data: &StringInternerData) -> Result<()> {
    for (idx, s) in data.strings.iter().enumerate() {
        let interned_id = GLOBAL_INTERNER.intern(s).map_err(|e| {
            IndexPersistenceError::Serialization(format!("Failed to intern string: {}", e))
        })?;
        // The interner should assign indices in order
        // If not, the interner had pre-existing strings which is a bug
        if interned_id.as_u32() != idx as u32 {
            // Allow mismatch if the string is empty. This handles "holes" in the ID space
            // created by race conditions (e.g. during DoS tests where intern() reverts ID).
            // get_all_strings() fills holes with "", which resolves to a low ID (e.g. 0),
            // causing a mismatch with the hole's high index. This is safe because no
            // valid data references the hole ID.
            if s.is_empty() {
                continue;
            }

            return Err(IndexPersistenceError::InternerMismatch {
                expected: idx as u32,
                got: interned_id.as_u32(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_string_interner_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Intern some strings
        let _idx1 = GLOBAL_INTERNER.intern("test_string_1").unwrap();
        let _idx2 = GLOBAL_INTERNER.intern("test_string_2").unwrap();
        let _idx3 = GLOBAL_INTERNER.intern("test_string_3").unwrap();

        // Save
        save_string_interner(&path).unwrap();

        // Load
        let loaded = load_string_interner(&path).unwrap();

        assert_eq!(loaded.magic, INTERNER_MAGIC);
        assert!(loaded.strings.contains(&"test_string_1".to_string()));
        assert!(loaded.strings.contains(&"test_string_2".to_string()));
        assert!(loaded.strings.contains(&"test_string_3".to_string()));
    }

    #[test]
    fn test_invalid_magic_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.idx");

        // Write garbage with valid CRC32
        let bad_data = StringInternerData {
            magic: *b"BAAD",
            version: 1,
            string_count: 0,
            strings: vec![],
        };
        let encoded = bitcode::encode(&bad_data);

        // Calculate valid CRC32 for the bad data
        let mut hasher = Hasher::new();
        hasher.update(&encoded);
        let checksum = hasher.finalize();
        let mut data_with_checksum = encoded;
        data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

        fs::write(&path, data_with_checksum).unwrap();

        // Should fail on magic validation
        let result = load_string_interner(&path);
        assert!(matches!(
            result,
            Err(IndexPersistenceError::InvalidMagic { .. })
        ));
    }

    #[test]
    fn test_crc_corruption_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Save valid data
        let _idx1 = GLOBAL_INTERNER.intern("corruption_test").unwrap();
        save_string_interner(&path).unwrap();

        // Corrupt the data (change a byte in the middle)
        let mut bytes = fs::read(&path).unwrap();
        bytes[10] ^= 0xFF; // Flip all bits in one byte
        fs::write(&path, bytes).unwrap();

        // Loading should fail with corruption error
        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }

    #[test]
    fn test_truncated_file_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Write a file that's too small
        fs::write(&path, b"ab").unwrap();

        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }

    #[test]
    fn test_string_count_limit_dos_protection() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Create data with excessive string count
        let bad_data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: super::super::MAX_STRING_COUNT + 1,
            strings: vec!["test".to_string()],
        };

        let encoded = bitcode::encode(&bad_data);

        // Calculate valid CRC32 for the bad data
        let mut hasher = Hasher::new();
        hasher.update(&encoded);
        let checksum = hasher.finalize();
        let mut data_with_checksum = encoded;
        data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

        fs::write(&path, data_with_checksum).unwrap();

        // Loading should fail with size limit error
        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Size limit exceeded"));
        assert!(err.to_string().contains("String count"));
    }

    #[test]
    fn test_string_length_limit_dos_protection() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Create data with excessively long string
        // Use MAX_STRING_LENGTH + 1 to exceed the limit
        // Test file size limit is 20MB to allow this test to work
        let oversized_string = "x".repeat(super::super::MAX_STRING_LENGTH + 1);

        // Verify this exceeds the limit but is within file size bounds for test
        assert!(
            oversized_string.len() > super::super::MAX_STRING_LENGTH,
            "String should exceed MAX_STRING_LENGTH"
        );
        assert!(
            oversized_string.len() < super::super::MAX_STRING_INTERNER_FILE_SIZE as usize,
            "String should be within file size limit to test string length check"
        );

        let bad_data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: 1,
            strings: vec![oversized_string],
        };

        let encoded = bitcode::encode(&bad_data);

        // Calculate valid CRC32 for the bad data
        let mut hasher = Hasher::new();
        hasher.update(&encoded);
        let checksum = hasher.finalize();
        let mut data_with_checksum = encoded;
        data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

        fs::write(&path, data_with_checksum).unwrap();

        // Loading should fail with size limit error
        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Size limit exceeded"));
        assert!(err.to_string().contains("String length"));
    }

    #[test]
    fn test_restore_string_interner_mismatch() {
        // This test verifies that we catch inconsistencies where the restored interner
        // indices don't match the expected order (e.g. if GLOBAL_INTERNER already
        // contains strings in a different order).

        // Construct data where index 0 is "type" (which has ID 2 in COMMON_STRINGS).
        // Since GLOBAL_INTERNER is pre-warmed, intern("type") will return 2.
        // restore_string_interner will expect 0.
        // 2 != 0 => Mismatch.
        let data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: 1,
            strings: vec!["type".to_string()],
        };

        let result = restore_string_interner(&data);
        assert!(result.is_err());

        match result.unwrap_err() {
            IndexPersistenceError::InternerMismatch { expected, got } => {
                assert_eq!(expected, 0, "Expected index 0 (from data position)");
                // "type" is usually index 2 in COMMON_STRINGS, but we just verify mismatch
                assert_ne!(got, 0, "Got index should not be 0");
            }
            err => panic!("Expected InternerMismatch, got: {:?}", err),
        }
    }

    #[test]
    fn test_restore_string_interner_mismatch_with_new_string() {
        // This test verifies mismatch with a completely new string.
        // Since GLOBAL_INTERNER is pre-warmed (len > 0), a new string will get ID > 0.
        // If we put it at index 0 in data, it should fail.

        let unique_string = format!(
            "unique_string_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: 1,
            strings: vec![unique_string],
        };

        let result = restore_string_interner(&data);
        assert!(result.is_err());

        match result.unwrap_err() {
            IndexPersistenceError::InternerMismatch { expected, got } => {
                assert_eq!(expected, 0, "Expected index 0 (from data position)");
                assert!(
                    got > 0,
                    "Got index should be > 0 (because interner is pre-warmed)"
                );
            }
            err => panic!("Expected InternerMismatch, got: {:?}", err),
        }
    }
}
