//! Vector index persistence.
//!
//! Vector indexes use a hybrid approach:
//! - usearch native format for the HNSW index itself
//! - bitcode for metadata and ID mappings

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crc32fast::Hasher;

use super::error::{IndexPersistenceError, Result};
use super::formats::{
    PersistedHnswConfig, VectorIndexMeta, VectorMappingsData, VectorSnapshotMeta,
};
use super::{MANIFEST_VERSION, VECTOR_META_MAGIC};

/// Helper function to save data with CRC32 checksum using atomic write.
///
/// Uses write-temp-then-rename to prevent corruption on crash.
fn save_with_crc(data: &[u8], path: &Path) -> Result<()> {
    let mut hasher = Hasher::new();
    hasher.update(data);
    let checksum = hasher.finalize();

    let mut data_with_checksum = data.to_vec();
    data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

    super::atomic_write(path, &data_with_checksum)?;
    Ok(())
}

/// Helper function to load data and validate CRC32 checksum.
fn load_with_crc(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;

    if bytes.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    let (data, checksum_bytes) = bytes.split_at(bytes.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

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

    Ok(data.to_vec())
}

/// Save vector index metadata with CRC32 checksum.
pub fn save_vector_meta(meta: &VectorIndexMeta, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(meta);
    save_with_crc(&encoded, path)
}

/// Load vector index metadata and validate CRC32 checksum.
pub fn load_vector_meta(path: &Path) -> Result<VectorIndexMeta> {
    let data = load_with_crc(path)?;
    let meta: VectorIndexMeta = bitcode::decode(&data)?;

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
pub fn save_vector_mappings(mappings: &VectorMappingsData, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(mappings);
    save_with_crc(&encoded, path)
}

/// Load vector ID mappings and validate CRC32 checksum.
pub fn load_vector_mappings(path: &Path) -> Result<VectorMappingsData> {
    let data = load_with_crc(path)?;
    let mappings: VectorMappingsData = bitcode::decode(&data)?;
    Ok(mappings)
}

/// Save vector snapshot metadata with CRC32 checksum.
pub fn save_snapshot_meta(meta: &VectorSnapshotMeta, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(meta);
    save_with_crc(&encoded, path)
}

/// Load vector snapshot metadata and validate CRC32 checksum.
pub fn load_snapshot_meta(path: &Path) -> Result<VectorSnapshotMeta> {
    let data = load_with_crc(path)?;
    let meta: VectorSnapshotMeta = bitcode::decode(&data)?;
    Ok(meta)
}

/// Create new vector index metadata.
pub fn new_vector_meta(
    property_name: &str,
    dimensions: u32,
    metric: u8,
    config: PersistedHnswConfig,
) -> VectorIndexMeta {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
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
