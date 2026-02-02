//! Index manifest persistence.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crc32fast::Hasher;

use super::error::{IndexPersistenceError, Result};
use super::formats::IndexManifest;
use super::{MANIFEST_MAGIC, MANIFEST_VERSION};

impl IndexManifest {
    /// Create a new empty manifest.
    pub fn new(lsn: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs() as i64;

        Self {
            magic: MANIFEST_MAGIC,
            version: MANIFEST_VERSION,
            created_at: now,
            last_modified: now,
            lsn,
            vector_indexes: Vec::new(),
            graph_index: None,
            temporal_index: None,
            temporal_adjacency_index: None,
            string_interner: None,
        }
    }

    /// Update the last_modified timestamp.
    pub fn touch(&mut self) {
        self.last_modified = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs() as i64;
    }

    /// Update the LSN.
    pub fn set_lsn(&mut self, lsn: u64) {
        self.lsn = lsn;
        self.touch();
    }
}

/// Save manifest to disk with CRC32 checksum using atomic write.
///
/// Format: `[bitcode_data][crc32_checksum_4_bytes]`
///
/// Uses write-temp-then-rename to prevent corruption on crash.
pub fn save_manifest(manifest: &IndexManifest, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(manifest);

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

/// Load manifest from disk and validate CRC32 checksum.
pub fn load_manifest(path: &Path) -> Result<IndexManifest> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > super::MAX_MANIFEST_FILE_SIZE {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "Manifest file size {} exceeds limit {}",
                metadata.len(),
                super::MAX_MANIFEST_FILE_SIZE
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
    let manifest: IndexManifest = bitcode::decode(data)?;

    // Validate magic bytes
    if manifest.magic != MANIFEST_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: MANIFEST_MAGIC,
            got: manifest.magic,
        });
    }

    // Validate version
    if manifest.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: manifest.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::*;
    use tempfile::tempdir;

    #[test]
    fn test_manifest_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        let mut manifest = IndexManifest::new(42);
        manifest.string_interner = Some(StringInternerManifestEntry {
            interner_file: "strings/interner.idx".to_string(),
            string_count: 100,
        });
        manifest.vector_indexes.push(VectorIndexManifestEntry {
            property_name: "embedding".to_string(),
            dimensions: 384,
            metric: 0,
            current_file: "vector/embedding/current.usearch".to_string(),
            mappings_file: "vector/embedding/current.mappings".to_string(),
            snapshot_count: 5,
            temporal_enabled: true,
        });

        save_manifest(&manifest, &path).unwrap();
        let loaded = load_manifest(&path).unwrap();

        assert_eq!(loaded.magic, MANIFEST_MAGIC);
        assert_eq!(loaded.lsn, 42);
        assert_eq!(loaded.vector_indexes.len(), 1);
        assert_eq!(loaded.vector_indexes[0].property_name, "embedding");
        assert!(loaded.string_interner.is_some());
    }

    #[test]
    fn test_manifest_touch_updates_timestamp() {
        let mut manifest = IndexManifest::new(0);
        let original = manifest.last_modified;

        std::thread::sleep(std::time::Duration::from_millis(10));
        manifest.touch();

        assert!(manifest.last_modified >= original);
    }

    #[test]
    fn test_manifest_crc_corruption_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        // Save a valid manifest
        let manifest = IndexManifest::new(42);
        save_manifest(&manifest, &path).unwrap();

        // Corrupt the data (change a byte in the middle)
        let mut bytes = fs::read(&path).unwrap();
        bytes[10] ^= 0xFF; // Flip all bits in one byte
        fs::write(&path, bytes).unwrap();

        // Loading should fail with corruption error
        let result = load_manifest(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }

    #[test]
    fn test_manifest_truncated_file_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        // Write a file that's too small
        fs::write(&path, b"ab").unwrap();

        let result = load_manifest(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }
}
