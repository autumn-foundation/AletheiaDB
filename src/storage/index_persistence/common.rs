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

// ── Encrypted index file header (Issue #481) ─────────────────────────
//
// Every encrypted index file is prefixed with a small PLAINTEXT header,
// ahead of the AEAD ciphertext produced from the file's normal plaintext
// content. The header is fed to the cipher as Additional Authenticated Data
// (AAD), so any tampering of the header (e.g. flipping the algorithm id)
// fails authentication on decrypt.
//
// Layout (10 bytes, little-endian):
//   [MAGIC: 4 = b"AEIX"][format_version: u8][algorithm_id: u8][key_version: u32]
//
// Detection on read peeks the first 4 bytes: `AEIX` => encrypted, anything
// else => legacy/plaintext. This magic can never collide with a legacy
// plaintext bitcode index file: every in-struct magic used by this module is
// ASCII "G..." (GIDX/GSTR/GGRP/GDLT/GTMP/GTAJ/GVEC) and a bitcode-encoded
// struct begins with those bytes, so a plaintext file never starts with
// "AEIX". This lets a single directory hold a mix of plaintext and encrypted
// files (the upgrade scenario) and makes encryption-disabled a pure no-op.

/// Magic prefix identifying an encrypted index file.
pub(crate) const ENC_INDEX_MAGIC: [u8; 4] = *b"AEIX";

/// Current encrypted-index header format version.
pub(crate) const ENC_INDEX_FORMAT_V1: u8 = 1;

/// Key version stamped into v1 headers. There is no per-version key API yet
/// (Issue #488 will add key rotation); v1 always records `1` for
/// forward-compatibility so a future reader can select the right key.
pub(crate) const ENC_INDEX_KEY_VERSION_V1: u32 = 1;

/// Total length in bytes of the plaintext encrypted-index header.
pub(crate) const ENC_HEADER_LEN: usize = 10;

/// Returns `true` if `bytes` begins with the encrypted-index header magic and
/// is long enough to contain a full header.
pub(crate) fn is_encrypted_index(bytes: &[u8]) -> bool {
    bytes.len() >= ENC_HEADER_LEN && bytes[..4] == ENC_INDEX_MAGIC
}

/// Build the 10-byte plaintext header for an encrypted index file.
fn build_enc_header(algorithm_id: u8, key_version: u32) -> [u8; ENC_HEADER_LEN] {
    let mut header = [0u8; ENC_HEADER_LEN];
    header[..4].copy_from_slice(&ENC_INDEX_MAGIC);
    header[4] = ENC_INDEX_FORMAT_V1;
    header[5] = algorithm_id;
    header[6..10].copy_from_slice(&key_version.to_le_bytes());
    header
}

/// Wrap already-serialized plaintext file content with the encrypted-index
/// header and AEAD ciphertext: returns `[header][nonce||ciphertext||tag]`.
///
/// The header is passed as AAD so it is authenticated (but not encrypted).
pub(crate) fn encrypt_index_bytes(plaintext: &[u8], cipher: &Arc<dyn Cipher>) -> Result<Vec<u8>> {
    let header = build_enc_header(cipher.algorithm_id(), ENC_INDEX_KEY_VERSION_V1);
    let ciphertext = cipher
        .encrypt(plaintext, &header)
        .map_err(|e| IndexPersistenceError::Serialization(format!("Encryption failed: {e}")))?;

    let mut out = Vec::with_capacity(ENC_HEADER_LEN + ciphertext.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an encrypted index file's raw bytes, returning the plaintext file
/// content. `bytes` MUST satisfy [`is_encrypted_index`].
///
/// Fails closed:
/// - unknown header format version => [`IndexPersistenceError::Corrupted`];
/// - no cipher available (`cipher` is `None`) => `Corrupted` (never falls back
///   to reading ciphertext as plaintext);
/// - decryption / AAD / tag failure => `Corrupted`.
pub(crate) fn decrypt_index_bytes(
    bytes: &[u8],
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<Vec<u8>> {
    debug_assert!(is_encrypted_index(bytes));
    let header = &bytes[..ENC_HEADER_LEN];
    let format_version = header[4];
    if format_version != ENC_INDEX_FORMAT_V1 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!("Unsupported encrypted index format version {format_version}").into(),
        });
    }

    let cipher = cipher.ok_or_else(|| IndexPersistenceError::Corrupted {
        path: path.to_path_buf(),
        source: "File is encrypted but no encryption key is configured".into(),
    })?;

    cipher
        .decrypt(&bytes[ENC_HEADER_LEN..], header)
        .map_err(|e| IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!("Decryption failed: {e}").into(),
        })
}

/// Read an index file from disk, transparently decrypting it if it carries the
/// encrypted-index header, and return the plaintext file content.
///
/// The on-disk file size is checked against `max_size` (DoS protection) before
/// reading. A legacy/plaintext file is returned byte-for-byte unchanged, so a
/// caller's existing plaintext-decode logic works on the result regardless of
/// whether the file was encrypted.
pub(crate) fn read_index_file(
    path: &Path,
    max_size: u64,
    context: &str,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<Vec<u8>> {
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
    if is_encrypted_index(&bytes) {
        decrypt_index_bytes(&bytes, path, cipher)
    } else {
        Ok(bytes)
    }
}

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
    verify_and_strip_crc(bytes, path)
}

/// Verify and strip the trailing CRC32 from an in-memory buffer (shared by the
/// plaintext and encrypted read paths).
fn verify_and_strip_crc(bytes: Vec<u8>, path: &Path) -> Result<Vec<u8>> {
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

/// Like [`read_and_verify_crc`], but transparently decrypts the file first if
/// it carries the encrypted-index header (Issue #481). For a legacy/plaintext
/// file (`cipher` may be `None` or `Some`) this behaves exactly like
/// [`read_and_verify_crc`]; for an encrypted file it decrypts with the given
/// cipher (failing closed if `cipher` is `None`) and then verifies the CRC32
/// embedded in the decrypted plaintext.
pub(crate) fn read_and_verify_crc_maybe_encrypted(
    path: &Path,
    max_size: u64,
    context: &str,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<Vec<u8>> {
    let content = read_index_file(path, max_size, context, cipher)?;
    verify_and_strip_crc(content, path)
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

/// Save encoded data with a CRC32 checksum, encrypting the whole file (with a
/// plaintext header) when a cipher is supplied (Issue #481).
///
/// With `cipher == None` this is exactly [`save_encoded_with_crc`] (a plaintext
/// file with no header), so encryption-disabled is a byte-for-byte no-op.
pub(crate) fn save_encoded_maybe_encrypted<T: Encode>(
    data: &T,
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    match cipher {
        Some(cipher) => save_encoded_encrypted(data, path, cipher),
        None => save_encoded_with_crc(data, path),
    }
}

/// Load encoded data written by [`save_encoded_maybe_encrypted`], sniffing the
/// on-disk header to decide whether to decrypt.
///
/// A file bearing the encrypted-index header is decrypted with `cipher`
/// (failing closed if `cipher` is `None`); any other file is read as a legacy
/// plaintext CRC file. This makes a directory mixing plaintext and encrypted
/// files load correctly, and makes a legacy plaintext file readable even when a
/// cipher is configured (the upgrade scenario).
pub(crate) fn load_encoded_maybe_encrypted<T: for<'a> Decode<'a>>(
    path: &Path,
    max_size: u64,
    context: &str,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<T> {
    let data = read_and_verify_crc_maybe_encrypted(path, max_size, context, cipher)?;
    let decoded: T = bitcode::decode(&data)?;
    Ok(decoded)
}

/// Save encoded data with a CRC32 checksum, encrypted whole-file with a
/// plaintext header (Issue #481).
///
/// Format on disk: `[header:10]encrypt([bitcode_data][crc32:4], aad=header)`
/// where the header is `[AEIX][format][algorithm_id][key_version:4]`.
///
/// The CRC32 is computed on the plaintext **before** encryption (the AEAD tag
/// already authenticates the ciphertext; the inner CRC gives a clear error if
/// a decrypted buffer is nonetheless malformed and keeps the plaintext format
/// identical to the unencrypted files).
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
pub(crate) fn save_encoded_encrypted<T: Encode>(
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

    // Encrypt the whole thing (data + CRC) with the header as AAD.
    let encrypted = encrypt_index_bytes(&plaintext, cipher)?;

    atomic_write(path, &encrypted)
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
        let loaded: u64 = load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher)).unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_encrypted_complex_data_round_trip() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();
        let data = vec![1u8, 2, 3, 4, 5, 100, 200, 255];

        save_encoded_encrypted(&data, path, &cipher).unwrap();

        let loaded: Vec<u8> =
            load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher)).unwrap();
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
        let result: Result<u64> = load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher));
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
        let result: Result<u64> = load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher2));
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
        let result: Result<Vec<u8>> = load_encoded_maybe_encrypted(path, 10, "Test", Some(&cipher));
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

    // ── Header-aware / mixed-directory tests (Issue #481) ────────────

    #[test]
    fn test_encrypted_file_has_plaintext_header_on_disk() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();

        save_encoded_encrypted(&99u64, path, &cipher).unwrap();

        let raw = fs::read(path).unwrap();
        assert!(is_encrypted_index(&raw), "expected AEIX header on disk");
        assert_eq!(&raw[..4], &ENC_INDEX_MAGIC);
        assert_eq!(raw[4], ENC_INDEX_FORMAT_V1, "format version byte");
        assert_eq!(raw[5], cipher.algorithm_id(), "algorithm id byte");
        assert_eq!(
            u32::from_le_bytes(raw[6..10].try_into().unwrap()),
            ENC_INDEX_KEY_VERSION_V1
        );
    }

    #[test]
    fn test_maybe_encrypted_none_writes_plaintext_no_header() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();

        save_encoded_maybe_encrypted(&123u64, path, None).unwrap();

        let raw = fs::read(path).unwrap();
        assert!(
            !is_encrypted_index(&raw),
            "None cipher must not add a header"
        );

        // Round-trips through both the plaintext and sniffing loaders.
        let a: u64 = load_encoded_with_crc(path, 4096, "Test").unwrap();
        let b: u64 = load_encoded_maybe_encrypted(path, 4096, "Test", None).unwrap();
        assert_eq!(a, 123);
        assert_eq!(b, 123);
    }

    #[test]
    fn test_maybe_encrypted_roundtrip_with_cipher() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();
        let data = vec![7u8, 8, 9, 10];

        save_encoded_maybe_encrypted(&data, path, Some(&cipher)).unwrap();
        assert!(is_encrypted_index(&fs::read(path).unwrap()));

        let loaded: Vec<u8> =
            load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher)).unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn test_legacy_plaintext_loads_with_cipher_present() {
        // A file written WITHOUT encryption must still load when a cipher is
        // supplied (the upgrade scenario): the sniff path sees no AEIX header.
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();

        save_encoded_with_crc(&555u64, path).unwrap();
        let loaded: u64 = load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher)).unwrap();
        assert_eq!(loaded, 555);
    }

    #[test]
    fn test_encrypted_file_fails_closed_without_cipher() {
        // Header says encrypted but no cipher available => structured error,
        // never a silent fallback to reading ciphertext as plaintext.
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();

        save_encoded_encrypted(&1u64, path, &cipher).unwrap();

        let result: Result<u64> = load_encoded_maybe_encrypted(path, 4096, "Test", None);
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::Corrupted { .. }
        ));
    }

    #[test]
    fn test_tampered_header_algorithm_id_fails() {
        // Flipping the algorithm_id in the plaintext header must break
        // authentication because the header is AAD.
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();

        save_encoded_encrypted(&321u64, path, &cipher).unwrap();

        let mut raw = fs::read(path).unwrap();
        raw[5] ^= 0xFF; // algorithm_id byte
        fs::write(path, &raw).unwrap();

        let result: Result<u64> = load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher));
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::Corrupted { .. }
        ));
    }

    #[test]
    fn test_truncated_encrypted_file_errors() {
        // A crash-during-save that leaves a partial encrypted file must error,
        // never panic or return wrong data.
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();

        save_encoded_encrypted(&vec![1u8; 64], path, &cipher).unwrap();

        let mut raw = fs::read(path).unwrap();
        raw.truncate(raw.len() / 2);
        fs::write(path, &raw).unwrap();

        let result: Result<Vec<u8>> =
            load_encoded_maybe_encrypted(path, 4096, "Test", Some(&cipher));
        assert!(result.is_err());
    }
}
