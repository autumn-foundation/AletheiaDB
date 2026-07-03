//! Common utilities for index persistence.
//!
//! Provides generic helpers for saving and loading data with CRC32 checksums,
//! with optional encryption-at-rest support.

use bitcode::{Decode, Encode};
use crc32fast::Hasher;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use super::atomic_write;
use super::error::{IndexPersistenceError, Result};
use crate::encryption::cipher::Cipher;

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

/// Read a file from disk and validate its trailing CRC32 checksum, returning
/// the checksum-verified payload bytes (with the checksum suffix stripped)
/// without decoding them.
///
/// Exposed separately from [`load_encoded_with_crc`] so callers that may need
/// to try decoding the same verified bytes as more than one candidate shape
/// (e.g. a current format falling back to a frozen legacy shape) can read the
/// file and validate its checksum exactly once, rather than re-reading from
/// disk and re-verifying the checksum for each candidate decode attempt.
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
pub fn read_and_verify_crc(path: &Path, max_size: u64, context: &str) -> Result<Vec<u8>> {
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

    let mut data = bytes;
    data.truncate(data.len() - 4);
    Ok(data)
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
    let data = read_and_verify_crc(path, max_size, context)?;
    let decoded: T = bitcode::decode(&data)?;
    Ok(decoded)
}

/// Save encoded data with CRC32 checksum and encryption.
///
/// Format on disk: `encrypt([bitcode_data][crc32_checksum_4_bytes])`
///
/// The CRC32 is computed on the plaintext **before** encryption so that
/// decryption failures (wrong key, tampered ciphertext) are detected before
/// the CRC check, giving clearer error messages.
///
/// # Arguments
///
/// * `data` - The data to serialize, checksum, and encrypt
/// * `path` - The file path to write to
/// * `cipher` - AEAD cipher used for encryption
///
/// # Errors
///
/// Returns an error if serialization, encryption, or file I/O fails.
#[allow(dead_code)] // Wired when IndexPersistenceManager gains cipher awareness
pub fn save_encoded_encrypted<T: Encode>(
    data: &T,
    path: &Path,
    cipher: &Arc<dyn Cipher>,
) -> Result<()> {
    let encoded = bitcode::encode(data);

    // CRC on plaintext data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    let mut plaintext = encoded;
    plaintext.extend_from_slice(&checksum.to_le_bytes());

    // Encrypt the whole thing (data + CRC)
    let encrypted = cipher
        .encrypt(&plaintext, &[])
        .map_err(|e| IndexPersistenceError::Serialization(format!("Encryption failed: {e}")))?;

    atomic_write(path, &encrypted)
}

/// Load encrypted data, decrypt, validate CRC32, and decode.
///
/// Reverses the process of [`save_encoded_encrypted`]: reads raw bytes from
/// disk, decrypts them, validates the embedded CRC32, and deserializes.
///
/// # Arguments
///
/// * `path` - The file path to read from
/// * `max_size` - Maximum allowed file size (DoS protection)
/// * `context` - Context name for error messages (e.g., "Vector index")
/// * `cipher` - AEAD cipher used for decryption
///
/// # Errors
///
/// Returns an error if:
/// - File size exceeds `max_size`
/// - Decryption fails (wrong key, corrupted data)
/// - Decrypted payload is too small (missing checksum)
/// - CRC32 checksum mismatch
/// - Deserialization fails
#[allow(dead_code)] // Wired when IndexPersistenceManager gains cipher awareness
pub fn load_encoded_encrypted<T: for<'a> Decode<'a>>(
    path: &Path,
    max_size: u64,
    context: &str,
    cipher: &Arc<dyn Cipher>,
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

    let encrypted = fs::read(path)?;

    // Decrypt
    let plaintext =
        cipher
            .decrypt(&encrypted, &[])
            .map_err(|e| IndexPersistenceError::Corrupted {
                path: path.to_path_buf(),
                source: format!("Decryption failed: {e}").into(),
            })?;

    // Now proceed exactly like load_encoded_with_crc: split data/checksum, validate, decode
    if plaintext.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Decrypted data too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data, checksum_bytes) = plaintext.split_at(plaintext.len() - 4);
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

    // ── Encrypted variant tests ────────────────────────────────────

    fn test_cipher() -> Arc<dyn Cipher> {
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;

        let mut key = Zeroizing::new([0u8; 32]);
        key[0] = 0xAB;
        key[1] = 0xCD;
        Arc::new(Aes256GcmCipher::new(&key))
    }

    fn different_cipher() -> Arc<dyn Cipher> {
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;

        let mut key = Zeroizing::new([0u8; 32]);
        key[0] = 0x12;
        key[1] = 0x34;
        Arc::new(Aes256GcmCipher::new(&key))
    }

    #[test]
    fn test_encrypted_save_load_round_trip() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();
        let data = 42u64;

        // Save encrypted
        save_encoded_encrypted(&data, path, &cipher).unwrap();

        // Load encrypted
        let loaded: u64 = load_encoded_encrypted(path, 4096, "Test", &cipher).unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_encrypted_complex_data_round_trip() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();
        let data = vec![1u8, 2, 3, 4, 5, 100, 200, 255];

        save_encoded_encrypted(&data, path, &cipher).unwrap();

        let loaded: Vec<u8> = load_encoded_encrypted(path, 4096, "Test", &cipher).unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_encrypted_tampered_file_fails() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();
        let data = 42u64;

        save_encoded_encrypted(&data, path, &cipher).unwrap();

        // Corrupt the encrypted bytes on disk
        let mut bytes = fs::read(path).unwrap();
        // Flip a byte in the middle of the ciphertext
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        let mut file_rw = fs::File::create(path).unwrap();
        file_rw.write_all(&bytes).unwrap();

        // Decryption should fail
        let result: Result<u64> = load_encoded_encrypted(path, 4096, "Test", &cipher);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), IndexPersistenceError::Corrupted { .. }),
            "Expected Corrupted error for tampered encrypted file"
        );
    }

    #[test]
    fn test_encrypted_wrong_key_fails() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher1 = test_cipher();
        let cipher2 = different_cipher();
        let data = 42u64;

        // Encrypt with cipher1
        save_encoded_encrypted(&data, path, &cipher1).unwrap();

        // Attempt to decrypt with cipher2
        let result: Result<u64> = load_encoded_encrypted(path, 4096, "Test", &cipher2);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), IndexPersistenceError::Corrupted { .. }),
            "Expected Corrupted error when using wrong key"
        );
    }

    #[test]
    fn test_encrypted_size_limit_exceeded() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();
        let data = vec![0u8; 100];

        save_encoded_encrypted(&data, path, &cipher).unwrap();

        // Load with tiny limit
        let result: Result<Vec<u8>> = load_encoded_encrypted(path, 10, "Test", &cipher);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::SizeLimitExceeded { .. }
        ));
    }

    #[test]
    fn test_encrypted_file_not_readable_as_unencrypted() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();
        let data = 42u64;

        // Save encrypted
        save_encoded_encrypted(&data, path, &cipher).unwrap();

        // Attempting to load as unencrypted should fail (CRC or decode mismatch)
        let result: Result<u64> = load_encoded_with_crc(path, 4096, "Test");
        assert!(result.is_err());
    }
}
