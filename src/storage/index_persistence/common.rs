//! Common utilities for index persistence.
//!
//! Provides generic helpers for saving and loading data with CRC32 checksums,
//! with optional encryption-at-rest support.

use bitcode::{Decode, Encode};
use crc32fast::Hasher;
use std::fs;
use std::path::Path;
use std::sync::{Arc, RwLock};

use super::atomic_write;
use super::error::{IndexPersistenceError, Result};
use crate::encryption::cipher::Cipher;

// ── Key-version-addressable index cipher keyring (Issue #488) ─────────
//
// Before key rotation there is exactly one index generation. During a
// rotation the data dir holds a MIX of files encrypted under the old and the
// new key, distinguished only by the plaintext header `key_version` (Issue
// #481). The keyring maps `key_version -> cipher` so the read path can decrypt
// each file with the generation that actually wrote it, while writes always
// use the CURRENT (newest) generation and stamp its `key_version` into the
// header. A single-generation keyring reproduces the pre-rotation behavior
// exactly (it decrypts any header with its one cipher and writes `key_version`
// == 1), so the non-rotation path is unchanged.

/// One key generation: a `key_version` and the cipher derived from that
/// generation's index DEK.
#[derive(Clone)]
struct KeyGeneration {
    key_version: u32,
    cipher: Arc<dyn Cipher>,
}

#[derive(Clone)]
struct KeyringInner {
    /// All live generations (1 before rotation, 2 during, 1 after). Small.
    generations: Vec<KeyGeneration>,
    /// Version stamped into freshly written files (the newest generation).
    current_version: u32,
    /// Back-compat mode: a lone generation created from a single cipher with
    /// no explicit version decrypts ANY header `key_version` with that cipher,
    /// matching #481's version-agnostic single-cipher read exactly.
    match_any: bool,
}

/// A cheaply-cloneable, shared-mutable set of index ciphers addressed by
/// `key_version` (Issue #488).
///
/// Clones share the same underlying state, so a rotation engine can add the
/// new generation (making live reads dispatch on the header and live writes
/// stamp the new version) and later retire the old generation while the
/// persistence manager holding a clone observes the change immediately.
#[derive(Clone)]
pub(crate) struct IndexKeyring {
    inner: Arc<RwLock<KeyringInner>>,
}

impl IndexKeyring {
    /// A single-generation keyring from one cipher (the non-rotation path).
    ///
    /// Reads decrypt any header `key_version` with this cipher and writes stamp
    /// `ENC_INDEX_KEY_VERSION_V1` — byte-for-byte identical to #481.
    pub(crate) fn single(cipher: Arc<dyn Cipher>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(KeyringInner {
                generations: vec![KeyGeneration {
                    key_version: ENC_INDEX_KEY_VERSION_V1,
                    cipher,
                }],
                current_version: ENC_INDEX_KEY_VERSION_V1,
                match_any: true,
            })),
        }
    }

    /// A single-generation keyring pinned to an explicit `key_version`.
    ///
    /// Reads decrypt ANY header `key_version` with this one cipher (`match_any`,
    /// byte-identical to [`Self::single`]); only the write-stamp / reported
    /// [`current_version`](Self::current_version) is pinned to `key_version`.
    /// This is the constructor the durable `open()` path uses to PROVISION the
    /// keyring at the max on-disk key version (Issue #488 version-provisioning):
    /// without it `open()` always reports `current_version == 1`, so a rotated
    /// dataset's v2 files classify as "unknown" (a `verify` false-FAIL) and the
    /// next rotation re-uses version 2 and wedges on the P0.3 identity check.
    /// Callers that need strict per-version dispatch build the ring and then
    /// [`add_generation`](Self::add_generation), which leaves `match_any`.
    pub(crate) fn single_versioned(cipher: Arc<dyn Cipher>, key_version: u32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(KeyringInner {
                generations: vec![KeyGeneration {
                    key_version,
                    cipher,
                }],
                current_version: key_version,
                match_any: true,
            })),
        }
    }

    /// Read (decrypt) cipher for a header `key_version`, or `None` if no
    /// generation matches. A `match_any` single keyring returns its only
    /// cipher for every version (back-compat).
    fn cipher_for_version(&self, key_version: u32) -> Option<Arc<dyn Cipher>> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        if inner.match_any {
            return inner.generations.first().map(|g| g.cipher.clone());
        }
        inner
            .generations
            .iter()
            .find(|g| g.key_version == key_version)
            .map(|g| g.cipher.clone())
    }

    /// Current (write) cipher and the `key_version` it stamps.
    pub(crate) fn current(&self) -> Option<(Arc<dyn Cipher>, u32)> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let v = inner.current_version;
        inner
            .generations
            .iter()
            .find(|g| g.key_version == v)
            .map(|g| (g.cipher.clone(), v))
    }

    /// The version freshly written files are stamped with.
    pub(crate) fn current_version(&self) -> u32 {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current_version
    }

    /// Whether a generation with `key_version` is currently held (i.e. reads of
    /// files stamped with it can be decrypted). A `match_any` single keyring
    /// holds every version.
    #[cfg(test)]
    pub(crate) fn has_version(&self, key_version: u32) -> bool {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.match_any
            || inner
                .generations
                .iter()
                .any(|g| g.key_version == key_version)
    }

    /// The current (write) cipher, if any (for callers that only need the
    /// cipher, e.g. checkpoint's single-generation writes).
    pub(crate) fn current_cipher(&self) -> Option<Arc<dyn Cipher>> {
        self.current().map(|(c, _)| c)
    }

    /// Add a new generation, making it current. Switches the keyring to strict
    /// per-version dispatch. Used by the rotation engine at `begin`.
    pub(crate) fn add_generation(&self, key_version: u32, cipher: Arc<dyn Cipher>) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.match_any = false;
        inner.generations.retain(|g| g.key_version != key_version);
        inner.generations.push(KeyGeneration {
            key_version,
            cipher,
        });
        inner.current_version = key_version;
    }

    /// Retire every generation except `key_version`, which becomes the sole,
    /// current generation. Used by the rotation engine at `complete`/`cancel`.
    pub(crate) fn retain_only(&self, key_version: u32) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.generations.retain(|g| g.key_version == key_version);
        inner.current_version = key_version;
        inner.match_any = false;
    }
}

/// Read the header `key_version` of an encrypted index buffer without
/// decrypting. Returns `None` if `bytes` is not an encrypted index file.
pub(crate) fn index_file_key_version(bytes: &[u8]) -> Option<u32> {
    if !is_encrypted_index(bytes) {
        return None;
    }
    Some(u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]))
}

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
// plaintext index file. Verified empirically (bitcode 0.6.9): the FIRST
// on-disk byte of any plaintext index encoding is the bitcode scheme byte
// `0x00` (bitcode emits a leading scheme byte BEFORE any in-struct magic), or
// `0x28` for a zstd-compressed graph buffer — never `0x41` (`'A'`, the first
// byte of `AEIX`). (The in-struct `G...`/`AMAP` magics live several bytes in,
// after that scheme byte, so they are irrelevant to the first-byte test.)
// This lets a single directory hold a mix of plaintext and encrypted files
// (the upgrade scenario) and makes encryption-disabled a pure no-op.

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
    encrypt_index_bytes_versioned(plaintext, cipher, ENC_INDEX_KEY_VERSION_V1)
}

/// Like [`encrypt_index_bytes`] but stamps an explicit `key_version` into the
/// header (Issue #488 key rotation). `encrypt_index_bytes` is exactly this with
/// `key_version == ENC_INDEX_KEY_VERSION_V1`.
pub(crate) fn encrypt_index_bytes_versioned(
    plaintext: &[u8],
    cipher: &Arc<dyn Cipher>,
    key_version: u32,
) -> Result<Vec<u8>> {
    let header = build_enc_header(cipher.algorithm_id(), key_version);
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

    // Diagnostics only (Issue #481, P1.2): the algorithm id is already part of
    // the AAD, so a cross-algorithm file would fail authentication regardless.
    // Comparing it up front turns a generic "Decryption failed" into a clear
    // "algorithm mismatch", which is the actual operator misconfiguration (e.g.
    // switching `algorithm` in place over an existing encrypted dataset). No
    // key material is involved — only the two 1-byte algorithm identifiers.
    let file_algorithm_id = header[5];
    let configured_algorithm_id = cipher.algorithm_id();
    if file_algorithm_id != configured_algorithm_id {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!(
                "algorithm mismatch (file={file_algorithm_id}, configured={configured_algorithm_id})"
            )
            .into(),
        });
    }

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

// ── Keyring-aware read/write path (Issue #488) ───────────────────────
//
// These mirror the single-cipher helpers above but dispatch the decrypt
// cipher on the file's header `key_version` via an [`IndexKeyring`], and stamp
// the keyring's current version on writes. A single-generation keyring makes
// them behave exactly like the single-cipher path.

/// Decrypt an encrypted index buffer, selecting the cipher by the header's
/// `key_version` from `keyring` (Issue #488). `bytes` MUST satisfy
/// [`is_encrypted_index`].
pub(crate) fn decrypt_index_bytes_with_keyring(
    bytes: &[u8],
    path: &Path,
    keyring: Option<&IndexKeyring>,
) -> Result<Vec<u8>> {
    debug_assert!(is_encrypted_index(bytes));
    let key_version = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
    let cipher = keyring.and_then(|k| k.cipher_for_version(key_version));
    decrypt_index_bytes(bytes, path, cipher.as_ref())
}

/// Like [`read_index_file`] but decrypts via a keyring (Issue #488).
pub(crate) fn read_index_file_with_keyring(
    path: &Path,
    max_size: u64,
    context: &str,
    keyring: Option<&IndexKeyring>,
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
        decrypt_index_bytes_with_keyring(&bytes, path, keyring)
    } else {
        Ok(bytes)
    }
}

/// Like [`read_and_verify_crc_maybe_encrypted`] but decrypts via a keyring.
pub(crate) fn read_and_verify_crc_maybe_encrypted_with_keyring(
    path: &Path,
    max_size: u64,
    context: &str,
    keyring: Option<&IndexKeyring>,
) -> Result<Vec<u8>> {
    let content = read_index_file_with_keyring(path, max_size, context, keyring)?;
    verify_and_strip_crc(content, path)
}

/// Like [`load_encoded_maybe_encrypted`] but decrypts via a keyring.
pub(crate) fn load_encoded_maybe_encrypted_with_keyring<T: for<'a> Decode<'a>>(
    path: &Path,
    max_size: u64,
    context: &str,
    keyring: Option<&IndexKeyring>,
) -> Result<T> {
    let data = read_and_verify_crc_maybe_encrypted_with_keyring(path, max_size, context, keyring)?;
    let decoded: T = bitcode::decode(&data)?;
    Ok(decoded)
}

/// Like [`save_encoded_maybe_encrypted`] but encrypts with the keyring's
/// current generation, stamping its `key_version` into the header (Issue #488).
/// A `None` keyring writes plaintext.
pub(crate) fn save_encoded_maybe_encrypted_with_keyring<T: Encode>(
    data: &T,
    path: &Path,
    keyring: Option<&IndexKeyring>,
) -> Result<()> {
    match keyring.and_then(IndexKeyring::current) {
        Some((cipher, key_version)) => {
            save_encoded_encrypted_versioned(data, path, &cipher, key_version)
        }
        None => save_encoded_with_crc(data, path),
    }
}

/// Like [`save_encoded_encrypted`] but stamps an explicit `key_version`.
pub(crate) fn save_encoded_encrypted_versioned<T: Encode>(
    data: &T,
    path: &Path,
    cipher: &Arc<dyn Cipher>,
    key_version: u32,
) -> Result<()> {
    let encoded = bitcode::encode(data);

    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    let mut plaintext = encoded;
    plaintext.extend_from_slice(&checksum.to_le_bytes());

    let encrypted = encrypt_index_bytes_versioned(&plaintext, cipher, key_version)?;
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
    fn test_algorithm_mismatch_clear_error() {
        // Issue #481 (P1.2): a file whose header algorithm_id differs from the
        // configured cipher's must surface a clear "algorithm mismatch" error
        // (diagnostics only — the algo byte is already AAD) rather than a
        // generic "Decryption failed". We simulate a cross-algorithm file by
        // rewriting the header algorithm_id to a value the configured cipher
        // does not use, which the up-front check rejects before decryption.
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher(); // AES-256-GCM, algorithm_id == 1
        save_encoded_encrypted(&7u64, path, &cipher).unwrap();

        let mut raw = fs::read(path).unwrap();
        let configured = cipher.algorithm_id();
        raw[5] = configured.wrapping_add(1); // pretend a different algorithm
        fs::write(path, &raw).unwrap();

        let err =
            load_encoded_maybe_encrypted::<u64>(path, 4096, "Test", Some(&cipher)).unwrap_err();
        match err {
            IndexPersistenceError::Corrupted { source, .. } => {
                let msg = source.to_string();
                assert!(
                    msg.contains("algorithm mismatch"),
                    "expected algorithm-mismatch message, got: {msg}"
                );
            }
            other => panic!("expected Corrupted algorithm-mismatch error, got {other:?}"),
        }
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

    #[test]
    fn test_unsupported_format_version_fails() {
        // A future/unknown header format version must fail closed with a clear
        // Corrupted error, before any decryption is attempted with the wrong
        // framing — never a panic, never a silent misparse.
        let file = NamedTempFile::new().unwrap();
        let path = file.path();
        let cipher = test_cipher();

        save_encoded_encrypted(&77u64, path, &cipher).unwrap();

        let mut raw = fs::read(path).unwrap();
        raw[4] = 99; // bogus format_version (header byte 4)
        fs::write(path, &raw).unwrap();

        let err =
            load_encoded_maybe_encrypted::<u64>(path, 4096, "Test", Some(&cipher)).unwrap_err();
        match err {
            IndexPersistenceError::Corrupted { source, .. } => {
                let msg = source.to_string();
                assert!(
                    msg.contains("Unsupported encrypted index format version"),
                    "expected unsupported-format-version message, got: {msg}"
                );
            }
            other => panic!("expected Corrupted unsupported-version error, got {other:?}"),
        }
    }

    #[test]
    fn test_is_encrypted_index_boundaries() {
        // Empty and any buffer shorter than a full 10-byte header is NOT
        // treated as encrypted, even when the leading bytes match the magic —
        // detection requires a complete header so a tiny legacy file can never
        // be misclassified.
        assert!(!is_encrypted_index(b""));
        assert!(!is_encrypted_index(&ENC_INDEX_MAGIC[..])); // 4 bytes: magic but < header
        let short = {
            let mut v = ENC_INDEX_MAGIC.to_vec();
            v.extend_from_slice(&[0u8; ENC_HEADER_LEN - 4 - 1]); // total 9 bytes
            v
        };
        assert_eq!(short.len(), ENC_HEADER_LEN - 1);
        assert!(!is_encrypted_index(&short), "9-byte header must not match");

        // Exactly ENC_HEADER_LEN bytes with the magic => encrypted (boundary).
        let exact = {
            let mut v = ENC_INDEX_MAGIC.to_vec();
            v.extend_from_slice(&[0u8; ENC_HEADER_LEN - 4]);
            v
        };
        assert_eq!(exact.len(), ENC_HEADER_LEN);
        assert!(is_encrypted_index(&exact), "exact-length header must match");

        // Full-length buffer whose first 4 bytes are not the magic => legacy
        // plaintext (e.g. the bitcode scheme byte 0x00, never 'A').
        let mut wrong_magic = vec![0u8; ENC_HEADER_LEN + 8];
        wrong_magic[0] = 0x00;
        assert!(!is_encrypted_index(&wrong_magic));
    }
}
