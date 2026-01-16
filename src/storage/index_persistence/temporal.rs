//! Temporal index persistence.

use std::fs;
use std::path::Path;

use crc32fast::Hasher;

use super::error::{IndexPersistenceError, Result};
use super::formats::TemporalIndexData;
use super::{MANIFEST_VERSION, TEMPORAL_MAGIC};

/// Save temporal index data to disk with CRC32 checksum using atomic write.
///
/// Format: [bitcode_data][crc32_checksum_4_bytes]
///
/// Uses write-temp-then-rename to prevent corruption on crash.
pub fn save_temporal_index(data: &TemporalIndexData, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(data);

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

/// Load temporal index data from disk and validate CRC32 checksum.
pub fn load_temporal_index(path: &Path) -> Result<TemporalIndexData> {
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
    let data: TemporalIndexData = bitcode::decode(data)?;

    if data.magic != TEMPORAL_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: TEMPORAL_MAGIC,
            got: data.magic,
        });
    }

    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(data)
}

/// Create a new empty TemporalIndexData.
pub fn new_temporal_index_data() -> TemporalIndexData {
    TemporalIndexData {
        magic: TEMPORAL_MAGIC,
        version: MANIFEST_VERSION,
        node_versions: Vec::new(),
        node_anchors: Vec::new(),
        edge_versions: Vec::new(),
        edge_anchors: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::*;
    use tempfile::tempdir;

    #[test]
    fn test_temporal_index_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("temporal.idx");

        let mut data = new_temporal_index_data();
        data.node_versions.push(NodeVersionEntry {
            node_id: 1,
            valid_from: 1000,
            valid_to: Some(2000),
            tx_time: 1000,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: Some(42),
        });
        data.node_anchors.push(NodeAnchorEntry {
            node_id: 1,
            anchor_tx_time: 1000,
            full_state: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: Some(42),
        });

        save_temporal_index(&data, &path).unwrap();
        let loaded = load_temporal_index(&path).unwrap();

        assert_eq!(loaded.node_versions.len(), 1);
        assert_eq!(loaded.node_anchors.len(), 1);
        assert_eq!(loaded.node_versions[0].vector_snapshot_id, Some(42));
    }
}
