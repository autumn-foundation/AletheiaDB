//! Index manifest persistence.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::{IndexPersistenceError, Result};
use super::formats::IndexManifest;
use super::{MANIFEST_MAGIC, MANIFEST_VERSION};

impl IndexManifest {
    /// Create a new empty manifest.
    pub fn new(lsn: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
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
            string_interner: None,
        }
    }

    /// Update the last_modified timestamp.
    pub fn touch(&mut self) {
        self.last_modified = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
    }

    /// Update the LSN.
    pub fn set_lsn(&mut self, lsn: u64) {
        self.lsn = lsn;
        self.touch();
    }
}

/// Save manifest to disk.
pub fn save_manifest(manifest: &IndexManifest, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(manifest);
    fs::write(path, encoded)?;
    Ok(())
}

/// Load manifest from disk.
pub fn load_manifest(path: &Path) -> Result<IndexManifest> {
    let bytes = fs::read(path)?;
    let manifest: IndexManifest = bitcode::decode(&bytes)?;

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
}
