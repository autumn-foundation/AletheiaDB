//! Vector index persistence.
//!
//! Vector indexes use a hybrid approach:
//! - usearch native format for the HNSW index itself
//! - bitcode for metadata and ID mappings

use std::path::Path;
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
    // Metadata should be small, but use standard limit for consistency
    let meta: VectorIndexMeta = super::common::load_encoded_maybe_encrypted(
        path,
        super::MAX_VECTOR_INDEX_FILE_SIZE,
        "Vector index",
        cipher,
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
    super::common::load_encoded_maybe_encrypted(
        path,
        super::MAX_VECTOR_INDEX_FILE_SIZE,
        "Vector index",
        cipher,
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
    let meta_path = path.join("meta.idx");
    let mappings_path = path.join("mappings.idx");
    // usearch index is just a file path, we don't load it into memory here generally
    let index_path = path.join("current.usearch");

    let meta = load_vector_meta(&meta_path)?;
    let mappings = load_vector_mappings(&mappings_path)?;

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
}
