//! Temporal index persistence.

use std::fs;
use std::path::Path;

use super::error::{IndexPersistenceError, Result};
use super::formats::TemporalIndexData;
use super::{MANIFEST_VERSION, TEMPORAL_MAGIC};

/// Save temporal index data to disk.
pub fn save_temporal_index(data: &TemporalIndexData, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(data);
    fs::write(path, encoded)?;
    Ok(())
}

/// Load temporal index data from disk.
pub fn load_temporal_index(path: &Path) -> Result<TemporalIndexData> {
    let bytes = fs::read(path)?;
    let data: TemporalIndexData = bitcode::decode(&bytes)?;

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
