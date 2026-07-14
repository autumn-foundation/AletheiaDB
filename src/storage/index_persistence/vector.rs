//! Vector index persistence.
//!
//! Vector indexes use a hybrid approach:
//! - usearch native format for the HNSW index itself
//! - bitcode for metadata and ID mappings

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::error::{IndexPersistenceError, Result};
use super::formats::{
    PersistedHnswConfig, VectorIndexData, VectorIndexMeta, VectorMappingsData, VectorSnapshotMeta,
};
use super::{MANIFEST_VERSION, VECTOR_META_MAGIC};
use crate::encryption::cipher::Cipher;

/// Save vector index metadata with CRC32 checksum.
///
/// Ensures the integrity of the stored vector metadata by computing and appending a CRC32
/// checksum. This allows AletheiaDB to verify data hasn't been corrupted at rest.
///
/// # Errors
///
/// Returns an error if serialization or disk I/O fails.
pub fn save_vector_meta(meta: &VectorIndexMeta, path: &Path) -> Result<()> {
    save_vector_meta_with_cipher(meta, path, None)
}

/// Save vector index metadata, optionally encrypting it at rest (Issue #481).
///
/// # Errors
///
/// Returns an error if serialization, encryption, or disk I/O fails.
pub fn save_vector_meta_with_cipher(
    meta: &VectorIndexMeta,
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    super::common::save_encoded_maybe_encrypted(meta, path, cipher)
}

/// Save vector index metadata, encrypting with the current generation of an
/// [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
pub(crate) fn save_vector_meta_with_keyring(
    meta: &VectorIndexMeta,
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    super::common::save_encoded_maybe_encrypted_with_keyring(meta, path, keyring)
}

/// Load vector index metadata and validate CRC32 checksum.
///
/// Reads the encoded vector metadata and verifies its CRC32 checksum before decoding
/// to ensure we don't load corrupted configuration.
///
/// # Errors
///
/// Returns an error if the file is missing, corrupted, exceeds `MAX_VECTOR_INDEX_FILE_SIZE`,
/// or if the manifest version/magic bytes do not match.
pub fn load_vector_meta(path: &Path) -> Result<VectorIndexMeta> {
    load_vector_meta_with_cipher(path, None)
}

/// Load vector index metadata, transparently decrypting it if written
/// encrypted (Issue #481).
///
/// # Errors
///
/// Same as [`load_vector_meta`], plus a structured error if the file is
/// encrypted but no cipher is supplied or decryption fails.
pub fn load_vector_meta_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<VectorIndexMeta> {
    let ring = cipher.map(|c| super::common::IndexKeyring::single(c.clone()));
    load_vector_meta_with_keyring(path, ring.as_ref())
}

/// Load vector index metadata, decrypting via an
/// [`IndexKeyring`](super::common::IndexKeyring) that dispatches on the header
/// `key_version` (Issue #488 key rotation).
pub(crate) fn load_vector_meta_with_keyring(
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<VectorIndexMeta> {
    // Metadata should be small, but use standard limit for consistency
    let meta: VectorIndexMeta = super::common::load_encoded_maybe_encrypted_with_keyring(
        path,
        super::MAX_VECTOR_INDEX_FILE_SIZE,
        "Vector index",
        keyring,
    )?;

    if meta.magic != VECTOR_META_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: VECTOR_META_MAGIC,
            got: meta.magic,
        });
    }

    if meta.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: meta.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(meta)
}

/// Save vector ID mappings with CRC32 checksum.
///
/// Persists the explicit translation table between AletheiaDB's internal `NodeId`s
/// and `usearch`'s external `u64` keys. Includes a CRC32 checksum for integrity.
///
/// # Errors
///
/// Returns an error if serialization or disk I/O fails.
pub fn save_vector_mappings(mappings: &VectorMappingsData, path: &Path) -> Result<()> {
    save_vector_mappings_with_cipher(mappings, path, None)
}

/// Save vector ID mappings, optionally encrypting them at rest (Issue #481).
///
/// # Errors
///
/// Returns an error if serialization, encryption, or disk I/O fails.
pub fn save_vector_mappings_with_cipher(
    mappings: &VectorMappingsData,
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    super::common::save_encoded_maybe_encrypted(mappings, path, cipher)
}

/// Save vector ID mappings, encrypting with the current generation of an
/// [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
pub(crate) fn save_vector_mappings_with_keyring(
    mappings: &VectorMappingsData,
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    super::common::save_encoded_maybe_encrypted_with_keyring(mappings, path, keyring)
}

/// Load vector ID mappings and validate CRC32 checksum.
///
/// Restores the `NodeId` <-> `u64` translation table into memory, verifying
/// its integrity via CRC32 checksum before decoding.
///
/// # Errors
///
/// Returns an error if the file is missing, corrupted, or exceeds `MAX_VECTOR_INDEX_FILE_SIZE`.
pub fn load_vector_mappings(path: &Path) -> Result<VectorMappingsData> {
    load_vector_mappings_with_cipher(path, None)
}

/// Load vector ID mappings, transparently decrypting them if written encrypted
/// (Issue #481).
///
/// # Errors
///
/// Same as [`load_vector_mappings`], plus a structured error if the file is
/// encrypted but no cipher is supplied or decryption fails.
pub fn load_vector_mappings_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<VectorMappingsData> {
    let ring = cipher.map(|c| super::common::IndexKeyring::single(c.clone()));
    load_vector_mappings_with_keyring(path, ring.as_ref())
}

/// Load vector ID mappings, decrypting via an
/// [`IndexKeyring`](super::common::IndexKeyring) that dispatches on the header
/// `key_version` (Issue #488 key rotation).
pub(crate) fn load_vector_mappings_with_keyring(
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<VectorMappingsData> {
    super::common::load_encoded_maybe_encrypted_with_keyring(
        path,
        super::MAX_VECTOR_INDEX_FILE_SIZE,
        "Vector index",
        keyring,
    )
}

/// Save vector snapshot metadata with CRC32 checksum.
///
/// Used for point-in-time vector index checkpoints. Includes a CRC32 checksum
/// to prevent loading a corrupted checkpoint.
///
/// # Errors
///
/// Returns an error if serialization or disk I/O fails.
#[allow(dead_code)]
pub fn save_snapshot_meta(meta: &VectorSnapshotMeta, path: &Path) -> Result<()> {
    super::common::save_encoded_with_crc(meta, path)
}

/// Load vector snapshot metadata and validate CRC32 checksum.
///
/// Restores point-in-time vector index checkpoint metadata, verifying
/// its integrity via CRC32 checksum before decoding.
///
/// # Errors
///
/// Returns an error if the file is missing, corrupted, or exceeds `MAX_VECTOR_INDEX_FILE_SIZE`.
#[allow(dead_code)]
pub fn load_snapshot_meta(path: &Path) -> Result<VectorSnapshotMeta> {
    super::common::load_encoded_with_crc(path, super::MAX_VECTOR_INDEX_FILE_SIZE, "Vector index")
}

/// Load a complete vector index (meta + mappings + index path) from a directory.
///
/// # Arguments
///
/// * `path` - Directory containing the vector index files
///
/// # Errors
///
/// Returns an error if any required file (meta.idx, mappings.idx) is missing or corrupted.
pub fn load_vector_index(path: &Path) -> Result<VectorIndexData> {
    load_vector_index_with_cipher(path, None)
}

/// Load a complete vector index (meta + mappings + index path) from a
/// directory, transparently decrypting the `meta.idx` / `mappings.idx` files if
/// they were written encrypted (Issue #481).
///
/// # Errors
///
/// Returns an error if any required file (meta.idx, mappings.idx) is missing or
/// corrupted, or is encrypted but no cipher is supplied / decryption fails.
pub fn load_vector_index_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<VectorIndexData> {
    let meta_path = path.join("meta.idx");
    let mappings_path = path.join("mappings.idx");
    // usearch index is just a file path, we don't load it into memory here generally
    let index_path = path.join("current.usearch");

    let meta = load_vector_meta_with_cipher(&meta_path, cipher)?;
    let mappings = load_vector_mappings_with_cipher(&mappings_path, cipher)?;

    Ok(VectorIndexData {
        meta,
        mappings,
        index_path,
    })
}

/// Create new vector index metadata.
///
/// Initializes a new configuration object for an HNSW vector index, capturing the
/// creation timestamp, index dimensions, distance metric, and the `usearch`
/// hyper-parameters (`m`, `ef_construction`, `ef_search`).
pub fn new_vector_meta(
    property_name: &str,
    dimensions: u32,
    metric: u8,
    config: PersistedHnswConfig,
) -> VectorIndexMeta {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs() as i64;

    VectorIndexMeta {
        magic: VECTOR_META_MAGIC,
        version: MANIFEST_VERSION,
        property_name: property_name.to_string(),
        dimensions,
        metric,
        hnsw_config: config,
        vector_count: 0,
        created_at: now,
        last_modified: now,
    }
}

/// Create empty vector mappings.
///
/// Initializes a new, empty mapping structure for translating between AletheiaDB's
/// `NodeId`s and the native `u64` keys required by the `usearch` index.
pub fn new_vector_mappings() -> VectorMappingsData {
    VectorMappingsData {
        version: MANIFEST_VERSION,
        count: 0,
        mappings: Vec::new(),
        deleted_ids: Vec::new(),
    }
}

// ── Native HNSW (`current.usearch`) encryption-at-rest (Issue #481) ───────
//
// The native usearch index is written and memory-mapped directly by the
// bundled `usearch` C++ library through a filesystem PATH — it has no
// in-memory-buffer API we can route through our AEAD cipher. To encrypt it at
// rest anyway, we let usearch produce its two plaintext files at a temporary
// path (`<base>` + `<base>.usearch.mappings`), then whole-file-encrypt each
// (with the same `AEIX` plaintext header used for every other index file, fed
// as AAD) and atomically write the ciphertext to the real paths. On load, if
// the real file carries the `AEIX` header we decrypt both files to a temp
// path in the same directory, hand THAT path to usearch, and remove the temp
// files afterwards (a drop guard cleans them up on every exit path). When no
// cipher is configured the plaintext path is byte-identical to a plain
// `index.save(path)` / `HnswIndex::load(path)` — a pure no-op — and a legacy
// plaintext `current.usearch` (no header) still loads when a cipher IS
// configured (the upgrade scenario), via header sniffing.

/// RAII guard that best-effort removes a set of temporary files when dropped,
/// so a decrypt-to-temp (or a usearch save-to-temp) never leaks partial
/// plaintext on disk — on success OR on any error/early-return path.
struct TempFileGuard {
    paths: Vec<PathBuf>,
}

impl TempFileGuard {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            // Best-effort: a missing temp file (never created / already gone)
            // is fine; we deliberately ignore errors so cleanup never panics.
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Build a unique temporary sibling path for the native usearch shuffle,
/// living in the SAME directory as `real_path` so the subsequent
/// `atomic_write` rename stays on one filesystem. The `.usearch` extension is
/// kept so usearch's own `path.with_extension("usearch.mappings")` sidecar
/// naming stays consistent between the save and load temp bases.
fn native_temp_base(real_path: &Path) -> PathBuf {
    use rand::Rng;
    let suffix: u64 = rand::thread_rng().r#gen();
    let dir = real_path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".aeix-usearch-tmp-{suffix}.usearch"))
}

/// Whole-file-encrypt the plaintext bytes at `src`, stamping `key_version` into
/// the `[AEIX header]`, and atomically write the ciphertext blob to `dst`
/// (Issue #488 key rotation; the native usearch write path).
fn encrypt_file_whole_versioned(
    src: &Path,
    dst: &Path,
    cipher: &Arc<dyn Cipher>,
    key_version: u32,
) -> Result<()> {
    let plaintext = std::fs::read(src)?;
    let encrypted = super::common::encrypt_index_bytes_versioned(&plaintext, cipher, key_version)?;
    super::atomic_write(dst, &encrypted)
}

/// Read `src`, transparently decrypting if it carries the `AEIX` header (a
/// legacy plaintext file is copied through unchanged, covering the upgrade /
/// torn-write case) via an [`IndexKeyring`](super::common::IndexKeyring) that
/// dispatches on the header `key_version`, and atomically write the plaintext to
/// `dst` (Issue #488 key rotation; the native usearch read path).
fn decrypt_file_to_with_keyring(
    src: &Path,
    dst: &Path,
    max_size: u64,
    context: &str,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    let plaintext = super::common::read_index_file_with_keyring(src, max_size, context, keyring)?;
    super::atomic_write(dst, &plaintext)
}

/// Save the native HNSW index, encrypting both files with the current
/// generation of an [`IndexKeyring`](super::common::IndexKeyring) and stamping
/// its `key_version` (Issue #488 key rotation). A `None`/empty keyring writes
/// plaintext (identical to `index.save`).
pub(crate) fn save_hnsw_index_with_keyring(
    index: &crate::index::vector::HnswIndex,
    real_path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> crate::core::error::Result<()> {
    use crate::index::vector::VectorIndex;

    let Some((cipher, key_version)) = keyring.and_then(super::common::IndexKeyring::current) else {
        return index.save(real_path);
    };

    let temp_base = native_temp_base(real_path);
    let temp_mappings = temp_base.with_extension("usearch.mappings");
    let _guard = TempFileGuard::new(vec![temp_base.clone(), temp_mappings.clone()]);

    index.save(&temp_base)?;

    let real_mappings = real_path.with_extension("usearch.mappings");
    encrypt_file_whole_versioned(&temp_base, real_path, &cipher, key_version)
        .map_err(native_enc_err)?;
    encrypt_file_whole_versioned(&temp_mappings, &real_mappings, &cipher, key_version)
        .map_err(native_enc_err)?;
    Ok(())
}

/// Load the native HNSW index, transparently decrypting `current.usearch` and
/// its sidecar first if they carry the `AEIX` header (Issue #481).
///
/// A legacy plaintext file (no header) is handed straight to usearch with no
/// temp shuffle, so the plaintext path is unchanged.
/// Load the native HNSW index, decrypting via an
/// [`IndexKeyring`](super::common::IndexKeyring) that dispatches on the header
/// `key_version` (Issue #488 key rotation). A legacy plaintext file loads
/// directly.
pub(crate) fn load_hnsw_index_with_keyring(
    real_path: &Path,
    config: crate::index::vector::HnswConfig,
    keyring: Option<&super::common::IndexKeyring>,
) -> crate::core::error::Result<crate::index::vector::HnswIndex> {
    use crate::index::vector::HnswIndex;

    if !native_file_is_encrypted(real_path) {
        return HnswIndex::load(real_path, config);
    }

    if keyring.is_none() {
        return Err(crate::core::error::Error::Vector(
            crate::core::error::VectorError::IndexError(
                "usearch index is encrypted but no encryption key is configured".to_string(),
            ),
        ));
    }

    let temp_base = native_temp_base(real_path);
    let temp_mappings = temp_base.with_extension("usearch.mappings");
    let real_mappings = real_path.with_extension("usearch.mappings");
    let _guard = TempFileGuard::new(vec![temp_base.clone(), temp_mappings.clone()]);

    decrypt_file_to_with_keyring(
        real_path,
        &temp_base,
        super::MAX_MMAP_FILE_SIZE,
        "usearch index",
        keyring,
    )
    .map_err(native_enc_err)?;
    decrypt_file_to_with_keyring(
        &real_mappings,
        &temp_mappings,
        super::MAX_VECTOR_INDEX_FILE_SIZE,
        "usearch mappings",
        keyring,
    )
    .map_err(native_enc_err)?;

    HnswIndex::load(&temp_base, config)
}

/// Sniff the first bytes of `path` for the `AEIX` encrypted-index header
/// without reading the whole (potentially huge) native index file.
fn native_file_is_encrypted(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; super::common::ENC_HEADER_LEN];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut header)) {
        Ok(()) => super::common::is_encrypted_index(&header),
        Err(_) => false,
    }
}

/// Map an index-persistence error from the native encrypt/decrypt shuffle into
/// the vector-index error channel used by `HnswIndex::save`/`load`. The message
/// never contains key material (it is derived from `IndexPersistenceError`,
/// which carries only paths and cipher-opaque failure strings).
fn native_enc_err(e: IndexPersistenceError) -> crate::core::error::Error {
    crate::core::error::Error::Vector(crate::core::error::VectorError::IndexError(format!(
        "native usearch index encryption/decryption failed: {e}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::{PersistedSnapshotType, VectorMapping};
    use tempfile::tempdir;

    #[test]
    fn test_vector_meta_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("meta.idx");

        let config = PersistedHnswConfig {
            m: 16,
            ef_construction: 128,
            ef_search: 64,
        };
        let meta = new_vector_meta("embedding", 384, 0, config);

        save_vector_meta(&meta, &path).unwrap();
        let loaded = load_vector_meta(&path).unwrap();

        assert_eq!(loaded.property_name, "embedding");
        assert_eq!(loaded.dimensions, 384);
        assert_eq!(loaded.hnsw_config.m, 16);
    }

    #[test]
    fn test_vector_mappings_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mappings.idx");

        let mut mappings = new_vector_mappings();
        mappings.count = 3;
        mappings.mappings.push(VectorMapping {
            node_id: 1,
            usearch_key: 100,
        });
        mappings.mappings.push(VectorMapping {
            node_id: 2,
            usearch_key: 101,
        });
        mappings.mappings.push(VectorMapping {
            node_id: 3,
            usearch_key: 102,
        });
        mappings.deleted_ids.push(99);

        save_vector_mappings(&mappings, &path).unwrap();
        let loaded = load_vector_mappings(&path).unwrap();

        assert_eq!(loaded.count, 3);
        assert_eq!(loaded.mappings.len(), 3);
        assert_eq!(loaded.deleted_ids, vec![99]);
    }

    // ── Encryption-at-rest tests (Issue #481) ────────────────────────

    fn enc_test_cipher() -> Arc<dyn Cipher> {
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;
        let mut key = Zeroizing::new([0u8; 32]);
        key[3] = 0x9C;
        Arc::new(Aes256GcmCipher::new(&key))
    }

    #[test]
    fn test_vector_meta_encrypted_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("meta.idx");
        let cipher = enc_test_cipher();

        let config = PersistedHnswConfig {
            m: 16,
            ef_construction: 128,
            ef_search: 64,
        };
        let meta = new_vector_meta("embedding", 384, 0, config);

        save_vector_meta_with_cipher(&meta, &path, Some(&cipher)).unwrap();
        assert!(super::super::common::is_encrypted_index(
            &std::fs::read(&path).unwrap()
        ));

        let loaded = load_vector_meta_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.property_name, "embedding");
        assert_eq!(loaded.dimensions, 384);

        // Fails closed without a cipher; legacy plaintext still loads with one.
        assert!(load_vector_meta_with_cipher(&path, None).is_err());
        save_vector_meta(&meta, &path).unwrap();
        assert_eq!(
            load_vector_meta_with_cipher(&path, Some(&cipher))
                .unwrap()
                .dimensions,
            384
        );
    }

    #[test]
    fn test_vector_mappings_encrypted_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mappings.idx");
        let cipher = enc_test_cipher();

        let mut mappings = new_vector_mappings();
        mappings.count = 1;
        mappings.mappings.push(VectorMapping {
            node_id: 5,
            usearch_key: 500,
        });

        save_vector_mappings_with_cipher(&mappings, &path, Some(&cipher)).unwrap();
        assert!(super::super::common::is_encrypted_index(
            &std::fs::read(&path).unwrap()
        ));

        let loaded = load_vector_mappings_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.count, 1);
        assert_eq!(loaded.mappings[0].usearch_key, 500);
    }

    #[test]
    fn test_snapshot_meta_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.meta");

        let meta = VectorSnapshotMeta {
            snapshot_id: 42,
            snapshot_type: PersistedSnapshotType::Full,
            timestamp: 1234567890,
            vector_count: 1000,
            config: PersistedHnswConfig {
                m: 16,
                ef_construction: 128,
                ef_search: 64,
            },
            base_snapshot_id: None,
        };

        save_snapshot_meta(&meta, &path).unwrap();
        let loaded = load_snapshot_meta(&path).unwrap();

        assert_eq!(loaded.snapshot_id, 42);
        assert_eq!(loaded.vector_count, 1000);
        assert!(matches!(loaded.snapshot_type, PersistedSnapshotType::Full));
    }

    /// `true` if any plaintext native-usearch temp file was leaked into `dir`.
    fn any_aeix_temp_present(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".aeix-usearch-tmp")
        })
    }

    #[test]
    fn test_native_usearch_encrypted_round_trip_and_fail_closed() {
        use crate::core::id::NodeId;
        use crate::index::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorIndex};

        let dir = tempdir().unwrap();
        let path = dir.path().join("current.usearch");
        let cipher = enc_test_cipher();

        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        let index = HnswIndex::new(config.clone()).unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index
            .add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0])
            .unwrap();

        let ring = super::super::common::IndexKeyring::single(cipher.clone());
        save_hnsw_index_with_keyring(&index, &path, Some(&ring)).unwrap();

        // Both the native index file and its mappings sidecar are encrypted at
        // rest (carry the AEIX header).
        assert!(
            native_file_is_encrypted(&path),
            "native index not encrypted on disk"
        );
        let sidecar = path.with_extension("usearch.mappings");
        assert!(
            native_file_is_encrypted(&sidecar),
            "mappings sidecar not encrypted on disk"
        );
        // No decrypted-plaintext temp files leaked after save.
        assert!(
            !any_aeix_temp_present(dir.path()),
            "plaintext temp file leaked after save"
        );

        // Correct key => decrypts to a temp file, loads, and the vectors survive.
        let loaded = load_hnsw_index_with_keyring(&path, config.clone(), Some(&ring)).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            !any_aeix_temp_present(dir.path()),
            "plaintext temp file leaked after load"
        );

        // Fails closed: encrypted on disk but no cipher supplied. The error
        // message names the missing key and leaks no plaintext / no temp file.
        let err = load_hnsw_index_with_keyring(&path, config.clone(), None).unwrap_err();
        assert!(
            format!("{err}").contains("no encryption key"),
            "expected fail-closed missing-key error, got: {err}"
        );
        assert!(
            !any_aeix_temp_present(dir.path()),
            "temp leaked on fail-closed load"
        );

        // Wrong key => authentication failure, never the original data, and the
        // decrypted-plaintext temp files are cleaned up on the error path.
        let wrong = {
            use crate::encryption::Aes256GcmCipher;
            use zeroize::Zeroizing;
            let mut k = Zeroizing::new([0u8; 32]);
            k[7] = 0x5A;
            Arc::new(Aes256GcmCipher::new(&k)) as Arc<dyn Cipher>
        };
        let wrong_ring = super::super::common::IndexKeyring::single(wrong);
        assert!(load_hnsw_index_with_keyring(&path, config, Some(&wrong_ring)).is_err());
        assert!(
            !any_aeix_temp_present(dir.path()),
            "temp leaked on wrong-key load"
        );
    }
}
