//! Temporal index persistence.

use std::fs;
use std::path::Path;

use crc32fast::Hasher;

use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyValue;
use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange};
use crate::storage::version::{
    EdgeVersion, NodeVersion, PropertyDelta, VersionData,
};
#[cfg(test)]
use crate::storage::version::VersionMetadata;

use super::error::{IndexPersistenceError, Result};
use super::formats::{EdgeVersionEntry, NodeVersionEntry, PersistedVersionType, TemporalIndexData};
use super::graph::{persist_property_map, restore_property_map};
use super::{MANIFEST_VERSION, TEMPORAL_MAGIC};

/// Convert NodeVersion to NodeVersionEntry for persistence.
///
/// # Errors
///
/// Returns an error if property conversion fails (e.g., unsupported Array type).
pub fn convert_node_version(version: &NodeVersion) -> Result<NodeVersionEntry> {
    let valid_time = version.temporal.valid_time();
    let tx_time = version.temporal.transaction_time();

    // Extract properties based on version type
    let (version_type, properties, vector_snapshot_id) = match &version.data {
        VersionData::Anchor {
            properties,
            vector_snapshot_id,
        } => (
            PersistedVersionType::Anchor,
            persist_property_map(properties)?,
            vector_snapshot_id.map(|id| id as u64),
        ),
        VersionData::Delta { delta } => {
            // For deltas, we persist changed properties AND removed keys
            let mut builder = crate::core::property::PropertyMapBuilder::new();

            // Add regular changed properties
            for (key, value) in &delta.changed {
                builder = builder.insert_by_key(*key, value.clone());
            }

            // Convert vector deltas to full vectors for persistence
            // VectorDelta::Sparse instances MUST be materialized before persistence
            // to prevent data loss. Use PropertyDelta::materialize_vector_deltas() first.
            for (key, vec_delta) in &delta.vector_deltas {
                match vec_delta {
                    crate::storage::version::VectorDelta::Full(vec) => {
                        builder = builder.insert_by_key(*key, PropertyValue::Vector(vec.clone()));
                    }
                    crate::storage::version::VectorDelta::Sparse { .. } => {
                        // CRITICAL: Cannot persist sparse deltas without base vector
                        // This would cause silent data loss - return error instead
                        return Err(IndexPersistenceError::Serialization(format!(
                            "Cannot persist NodeVersion {}: VectorDelta::Sparse found for property key {:?}. \
                             Call PropertyDelta::materialize_vector_deltas() before persistence to prevent data loss.",
                            version.id.as_u64(),
                            key
                        )));
                    }
                }
            }

            // Collect removed property keys (as interned string indices)
            let removed_keys: Vec<u32> = delta.removed.iter().map(|k| k.as_u32()).collect();

            let props = builder.build();
            (
                PersistedVersionType::Delta {
                    // Phase 2: Extract wallclock for persistence format (i64)
                    base_anchor_tx: tx_time.start().wallclock(),
                    removed_keys,
                },
                persist_property_map(&props)?,
                None,
            )
        }
    };

    // Phase 2: Extract wallclock components for persistence format
    Ok(NodeVersionEntry {
        version_id: version.id.as_u64(),
        node_id: version.node_id.as_u64(),
        label_idx: version.label.as_u32(),
        valid_from: valid_time.start().wallclock(),
        valid_to: if valid_time.is_current() {
            None
        } else {
            Some(valid_time.end().wallclock())
        },
        tx_time: tx_time.start().wallclock(),
        version_type,
        properties,
        vector_snapshot_id,
    })
}

/// Convert EdgeVersion to EdgeVersionEntry for persistence.
///
/// # Errors
///
/// Returns an error if property conversion fails (e.g., unsupported Array type).
pub fn convert_edge_version(version: &EdgeVersion) -> Result<EdgeVersionEntry> {
    let valid_time = version.temporal.valid_time();
    let tx_time = version.temporal.transaction_time();

    // Extract properties based on version type
    let (version_type, properties) = match &version.data {
        VersionData::Anchor { properties, .. } => (
            PersistedVersionType::Anchor,
            persist_property_map(properties)?,
        ),
        VersionData::Delta { delta } => {
            // For deltas, we persist changed properties AND removed keys
            let mut builder = crate::core::property::PropertyMapBuilder::new();

            // Add regular changed properties
            for (key, value) in &delta.changed {
                builder = builder.insert_by_key(*key, value.clone());
            }

            // Convert vector deltas to full vectors for persistence
            // VectorDelta::Sparse instances MUST be materialized before persistence
            for (key, vec_delta) in &delta.vector_deltas {
                match vec_delta {
                    crate::storage::version::VectorDelta::Full(vec) => {
                        builder = builder.insert_by_key(*key, PropertyValue::Vector(vec.clone()));
                    }
                    crate::storage::version::VectorDelta::Sparse { .. } => {
                        // CRITICAL: Cannot persist sparse deltas without base vector
                        return Err(IndexPersistenceError::Serialization(format!(
                            "Cannot persist EdgeVersion {}: VectorDelta::Sparse found for property key {:?}. \
                             Call PropertyDelta::materialize_vector_deltas() before persistence to prevent data loss.",
                            version.id.as_u64(),
                            key
                        )));
                    }
                }
            }

            // Collect removed property keys (as interned string indices)
            let removed_keys: Vec<u32> = delta.removed.iter().map(|k| k.as_u32()).collect();

            let props = builder.build();
            (
                PersistedVersionType::Delta {
                    // Phase 2: Extract wallclock for persistence format (i64)
                    base_anchor_tx: tx_time.start().wallclock(),
                    removed_keys,
                },
                persist_property_map(&props)?,
            )
        }
    };

    // Phase 2: Extract wallclock components for persistence format
    Ok(EdgeVersionEntry {
        version_id: version.id.as_u64(),
        edge_id: version.edge_id.as_u64(),
        source_id: version.source.as_u64(),
        target_id: version.target.as_u64(),
        label_idx: version.label.as_u32(),
        valid_from: valid_time.start().wallclock(),
        valid_to: if valid_time.is_current() {
            None
        } else {
            Some(valid_time.end().wallclock())
        },
        tx_time: tx_time.start().wallclock(),
        version_type,
        properties,
    })
}

/// Restore NodeVersionEntry back to NodeVersion.
///
/// # Arguments
///
/// * `entry` - The persisted node version entry
///
/// # Errors
///
/// Returns an error if property restoration fails (e.g., corrupted interned strings).
pub fn restore_node_version(entry: &NodeVersionEntry) -> Result<NodeVersion> {
    let label = InternedString::from_raw(entry.label_idx);
    let node_id = NodeId::new(entry.node_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!("Invalid node ID {}: {}", entry.node_id, e))
    })?;

    // Restore temporal interval
    // Phase 2: Convert i64 from persistence format to HybridTimestamp
    use crate::core::hlc::HybridTimestamp;
    let valid_start = HybridTimestamp::new_unchecked(entry.valid_from, 0);
    let valid_end = entry
        .valid_to
        .map(|t| HybridTimestamp::new_unchecked(t, 0))
        .unwrap_or(TIMESTAMP_MAX);

    let valid_time = TimeRange::new(valid_start, valid_end).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid valid time range [{}, {:?}]: {}",
            entry.valid_from, entry.valid_to, e
        ))
    })?;

    let tx_time = TimeRange::from(HybridTimestamp::new_unchecked(entry.tx_time, 0));
    let temporal = BiTemporalInterval::new(valid_time, tx_time);

    // Use the preserved version ID from the persisted entry
    let version_id = crate::core::id::VersionId::new(entry.version_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid version ID {}: {}",
            entry.version_id, e
        ))
    })?;

    // Restore version data based on type
    let data = match &entry.version_type {
        PersistedVersionType::Anchor => {
            let properties = restore_property_map(&entry.properties)?;
            let mut version_data = VersionData::anchor(properties);
            if let Some(snapshot_id) = entry.vector_snapshot_id {
                version_data.set_vector_snapshot_id(snapshot_id as usize);
            }
            version_data
        }
        PersistedVersionType::Delta { removed_keys, .. } => {
            // Restore delta - convert properties back to PropertyDelta
            let properties = restore_property_map(&entry.properties)?;
            let mut delta = PropertyDelta::new();

            // Restore changed properties
            for (key, value) in properties.iter() {
                delta.changed.insert(*key, value.clone());
            }

            // Restore removed property keys
            for key_idx in removed_keys {
                delta
                    .removed
                    .insert(crate::core::InternedString::from_raw(*key_idx));
            }

            VersionData::Delta { delta }
        }
    };

    Ok(NodeVersion {
        id: version_id,
        node_id,
        temporal,
        label,
        data,
        next_version: None,
        prev_version: None,
        metadata: crate::storage::version::VersionMetadata::default_for_existing(),
    })
}

/// Restore EdgeVersionEntry back to EdgeVersion.
///
/// # Arguments
///
/// * `entry` - The persisted edge version entry
///
/// # Errors
///
/// Returns an error if property restoration fails (e.g., corrupted interned strings).
pub fn restore_edge_version(entry: &EdgeVersionEntry) -> Result<EdgeVersion> {
    let label = InternedString::from_raw(entry.label_idx);
    let edge_id = EdgeId::new(entry.edge_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!("Invalid edge ID {}: {}", entry.edge_id, e))
    })?;

    let source = NodeId::new(entry.source_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid source node ID {}: {}",
            entry.source_id, e
        ))
    })?;

    let target = NodeId::new(entry.target_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid target node ID {}: {}",
            entry.target_id, e
        ))
    })?;

    // Restore temporal interval
    // Phase 2: Convert i64 from persistence format to HybridTimestamp
    use crate::core::hlc::HybridTimestamp;
    let valid_start = HybridTimestamp::new_unchecked(entry.valid_from, 0);
    let valid_end = entry
        .valid_to
        .map(|t| HybridTimestamp::new_unchecked(t, 0))
        .unwrap_or(TIMESTAMP_MAX);

    let valid_time = TimeRange::new(valid_start, valid_end).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid valid time range [{}, {:?}]: {}",
            entry.valid_from, entry.valid_to, e
        ))
    })?;

    let tx_time = TimeRange::from(HybridTimestamp::new_unchecked(entry.tx_time, 0));
    let temporal = BiTemporalInterval::new(valid_time, tx_time);

    // Use the preserved version ID from the persisted entry
    let version_id = crate::core::id::VersionId::new(entry.version_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid version ID {}: {}",
            entry.version_id, e
        ))
    })?;

    // Restore version data based on type
    let data = match &entry.version_type {
        PersistedVersionType::Anchor => {
            let properties = restore_property_map(&entry.properties)?;
            VersionData::anchor(properties)
        }
        PersistedVersionType::Delta { removed_keys, .. } => {
            // Restore delta - convert properties back to PropertyDelta
            let properties = restore_property_map(&entry.properties)?;
            let mut delta = PropertyDelta::new();

            // Restore changed properties
            for (key, value) in properties.iter() {
                delta.changed.insert(*key, value.clone());
            }

            // Restore removed property keys
            for key_idx in removed_keys {
                delta
                    .removed
                    .insert(crate::core::InternedString::from_raw(*key_idx));
            }

            VersionData::Delta { delta }
        }
    };

    Ok(EdgeVersion {
        id: version_id,
        edge_id,
        temporal,
        label,
        source,
        target,
        data,
        next_version: None,
        prev_version: None,
        metadata: crate::storage::version::VersionMetadata::default_for_existing(),
    })
}

/// Restore temporal index data into HistoricalStorage.
///
/// # Arguments
///
/// * `data` - The temporal index data loaded from disk
/// * `historical` - The HistoricalStorage to populate
///
/// # Errors
///
/// Returns an error if version restoration or insertion fails.
pub fn restore_into_historical_storage(
    data: &TemporalIndexData,
    historical: &mut crate::storage::historical::HistoricalStorage,
) -> Result<()> {
    // Pre-allocate capacity for better performance during bulk restoration
    historical.reserve_restoration_capacity(data.node_versions.len(), data.edge_versions.len());

    // Restore node versions
    for entry in &data.node_versions {
        let version = restore_node_version(entry)?;
        historical
            .insert_restored_node_version(version)
            .map_err(|e| {
                IndexPersistenceError::Serialization(format!(
                    "Failed to insert node version: {}",
                    e
                ))
            })?;
    }

    // Restore edge versions
    for entry in &data.edge_versions {
        let version = restore_edge_version(entry)?;
        historical
            .insert_restored_edge_version(version)
            .map_err(|e| {
                IndexPersistenceError::Serialization(format!(
                    "Failed to insert edge version: {}",
                    e
                ))
            })?;
    }

    // Rebuild version chains now that all versions are loaded.
    // This reconstructs prev_version/next_version links and ensures
    // version heads point to the correct (latest tx_time) version.
    historical.rebuild_version_chains();

    Ok(())
}

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
    use crate::core::id::{NodeId, VersionId};
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::{BiTemporalInterval, TimeRange};
    use crate::storage::index_persistence::formats::*;
    use crate::storage::version::{NodeVersion, VersionData};
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn test_temporal_index_round_trip() {
        use crate::core::GLOBAL_INTERNER;
        let dir = tempdir().unwrap();
        let path = dir.path().join("temporal.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let mut data = new_temporal_index_data();
        data.node_versions.push(NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: label.as_u32(),
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

    #[test]
    fn test_convert_node_version_anchor() {
        // RED: This test will fail because convert_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        let version = NodeVersion {
            id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            temporal: BiTemporalInterval::new(
                TimeRange::new(1000.into(), 2000.into()).unwrap(),
                TimeRange::new(1000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
            ),
            label,
            data: VersionData::Anchor {
                properties: props.clone(),
                vector_snapshot_id: Some(42),
            },
            next_version: None,
            prev_version: None,
            metadata: VersionMetadata::default_for_existing(),
        };

        let entry = convert_node_version(&version).unwrap();

        assert_eq!(entry.node_id, 1);
        assert_eq!(entry.valid_from, 1000);
        assert_eq!(entry.valid_to, Some(2000));
        assert_eq!(entry.tx_time, 1000);
        assert!(matches!(entry.version_type, PersistedVersionType::Anchor));
        assert_eq!(entry.vector_snapshot_id, Some(42));
        assert_eq!(entry.properties.entries.len(), 2);
    }

    #[test]
    fn test_convert_node_version_delta() {
        // RED: This test will fail because convert_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::version::PropertyDelta;
        use std::collections::HashMap;

        let mut changed = HashMap::new();
        changed.insert(
            GLOBAL_INTERNER.intern("age").unwrap(),
            crate::core::property::PropertyValue::Int(31),
        );

        let delta = PropertyDelta {
            changed,
            vector_deltas: Default::default(),
            removed: Default::default(),
        };

        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            temporal: BiTemporalInterval::new(
                TimeRange::new(2000.into(), 3000.into()).unwrap(),
                TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
            ),
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            metadata: VersionMetadata::default_for_existing(),
        };

        let entry = convert_node_version(&version).unwrap();

        assert_eq!(entry.node_id, 1);
        assert_eq!(entry.valid_from, 2000);
        assert_eq!(entry.valid_to, Some(3000));
        assert_eq!(entry.tx_time, 2000);
        assert_eq!(entry.vector_snapshot_id, None); // Deltas don't have snapshots
        // Delta should have changed properties
        assert!(!entry.properties.entries.is_empty());
    }

    #[test]
    fn test_convert_edge_version_anchor() {
        // RED: This test will fail because convert_edge_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;
        use crate::core::id::EdgeId;
        use crate::storage::version::EdgeVersion;

        let props = PropertyMapBuilder::new()
            .insert("weight", 1.5f64)
            .insert("label", "KNOWS")
            .build();

        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let version = EdgeVersion {
            id: VersionId::new(100).unwrap(),
            edge_id: EdgeId::new(10).unwrap(),
            temporal: BiTemporalInterval::new(
                TimeRange::new(1000.into(), 2000.into()).unwrap(),
                TimeRange::new(1000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
            ),
            label,
            source: NodeId::new(1).unwrap(),
            target: NodeId::new(2).unwrap(),
            data: VersionData::Anchor {
                properties: props.clone(),
                vector_snapshot_id: None,
            },
            next_version: None,
            prev_version: None,
            metadata: VersionMetadata::default_for_existing(),
        };

        let entry = convert_edge_version(&version).unwrap();

        assert_eq!(entry.edge_id, 10);
        assert_eq!(entry.source_id, 1);
        assert_eq!(entry.target_id, 2);
        assert_eq!(entry.valid_from, 1000);
        assert_eq!(entry.valid_to, Some(2000));
        assert_eq!(entry.tx_time, 1000);
        assert!(matches!(entry.version_type, PersistedVersionType::Anchor));
        assert_eq!(entry.properties.entries.len(), 2);
    }

    #[test]
    fn test_restore_node_version_anchor() {
        // RED: This test will fail because restore_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        // Create a persisted node version entry
        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        let name_key = GLOBAL_INTERNER.intern("name").unwrap();

        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties.entries.push((
            name_key.as_u32(),
            PersistedPropertyValue::String(GLOBAL_INTERNER.intern("Alice").unwrap().as_u32()),
        ));
        properties
            .entries
            .push((age_key.as_u32(), PersistedPropertyValue::Int(30)));

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let entry = NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: label.as_u32(),
            valid_from: 1000,
            valid_to: Some(2000),
            tx_time: 1000,
            version_type: PersistedVersionType::Anchor,
            properties,
            vector_snapshot_id: Some(42),
        };

        let version = restore_node_version(&entry).unwrap();

        assert_eq!(version.id.as_u64(), 100);
        assert_eq!(version.node_id.as_u64(), 1);
        assert_eq!(version.temporal.valid_time().start().wallclock(), 1000);
        assert_eq!(version.temporal.valid_time().end().wallclock(), 2000);
        assert_eq!(
            version.temporal.transaction_time().start().wallclock(),
            1000
        );
        assert!(version.data.is_anchor());
        assert_eq!(version.data.get_vector_snapshot_id(), Some(42));

        // Check properties were restored
        if let VersionData::Anchor { properties, .. } = &version.data {
            assert_eq!(properties.len(), 2);
            assert_eq!(
                properties.get("name").unwrap(),
                &crate::core::property::PropertyValue::String(Arc::from("Alice"))
            );
        } else {
            panic!("Expected anchor version");
        }
    }

    #[test]
    fn test_restore_node_version_delta() {
        // RED: This test will fail because restore_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties
            .entries
            .push((age_key.as_u32(), PersistedPropertyValue::Int(31)));

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let entry = NodeVersionEntry {
            version_id: 101,
            node_id: 1,
            label_idx: label.as_u32(),
            valid_from: 2000,
            valid_to: Some(3000),
            tx_time: 2000,
            version_type: PersistedVersionType::Delta {
                base_anchor_tx: 1000,
                removed_keys: vec![],
            },
            properties,
            vector_snapshot_id: None,
        };

        let version = restore_node_version(&entry).unwrap();

        assert_eq!(version.node_id.as_u64(), 1);
        assert!(version.data.is_delta());
        assert_eq!(version.data.get_vector_snapshot_id(), None);

        // Check delta was restored
        if let VersionData::Delta { delta } = &version.data {
            assert_eq!(delta.changed.len(), 1);
            assert!(delta.removed.is_empty());
        } else {
            panic!("Expected delta version");
        }
    }

    #[test]
    fn test_restore_edge_version_anchor() {
        // RED: This test will fail because restore_edge_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        let weight_key = GLOBAL_INTERNER.intern("weight").unwrap();
        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties
            .entries
            .push((weight_key.as_u32(), PersistedPropertyValue::Float(1.5)));

        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let entry = EdgeVersionEntry {
            version_id: 200,
            edge_id: 10,
            source_id: 1,
            target_id: 2,
            label_idx: label.as_u32(),
            valid_from: 1000,
            valid_to: Some(2000),
            tx_time: 1000,
            version_type: PersistedVersionType::Anchor,
            properties,
        };

        let version = restore_edge_version(&entry).unwrap();

        assert_eq!(version.id.as_u64(), 200);
        assert_eq!(version.edge_id.as_u64(), 10);
        assert_eq!(version.source.as_u64(), 1);
        assert_eq!(version.target.as_u64(), 2);
        assert_eq!(version.temporal.valid_time().start().wallclock(), 1000);
        assert_eq!(version.temporal.valid_time().end().wallclock(), 2000);
        assert!(version.data.is_anchor());

        // Check properties were restored
        if let VersionData::Anchor { properties, .. } = &version.data {
            assert_eq!(properties.len(), 1);
        } else {
            panic!("Expected anchor version");
        }
    }

    #[test]
    fn test_restore_versions_into_historical_storage() {
        // RED: This test will fail because restore_into_historical_storage doesn't exist yet
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::historical::HistoricalStorage;

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        GLOBAL_INTERNER.intern("name").unwrap();
        GLOBAL_INTERNER.intern("age").unwrap();

        // Create a persisted node version entry
        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        let name_key = GLOBAL_INTERNER.intern("name").unwrap();

        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties.entries.push((
            name_key.as_u32(),
            PersistedPropertyValue::String(GLOBAL_INTERNER.intern("Alice").unwrap().as_u32()),
        ));
        properties
            .entries
            .push((age_key.as_u32(), PersistedPropertyValue::Int(30)));

        let entry = NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: person_label.as_u32(),
            valid_from: 1000,
            valid_to: Some(2000),
            tx_time: 1000,
            version_type: PersistedVersionType::Anchor,
            properties,
            vector_snapshot_id: Some(42),
        };

        // Create temporal data (labels are now stored in entries)
        let mut temporal_data = new_temporal_index_data();
        temporal_data.node_versions.push(entry);

        // Restore into HistoricalStorage
        let mut historical = HistoricalStorage::new();
        restore_into_historical_storage(&temporal_data, &mut historical).unwrap();

        // Verify the version was restored
        let versions = historical.get_node_versions();
        assert_eq!(versions.len(), 1, "Should have 1 node version");

        let version = versions.values().next().unwrap();
        assert_eq!(version.node_id.as_u64(), 1);
        assert_eq!(version.temporal.valid_time().start().wallclock(), 1000);
        assert_eq!(version.temporal.valid_time().end().wallclock(), 2000);
        assert!(version.data.is_anchor());
    }

    // ========================================================================
    // Vector Delta Persistence Tests (Issue #215, Code Review C1/C2)
    // ========================================================================

    #[test]
    fn test_persist_delta_with_full_vector_delta() {
        // Test that VectorDelta::Full can be persisted successfully
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::version::{PropertyDelta, VectorDelta};

        let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Full(Arc::from(embedding.as_slice())),
        );

        let label = GLOBAL_INTERNER.intern("Document").unwrap();
        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            temporal: BiTemporalInterval::new(
                TimeRange::new(2000.into(), 3000.into()).unwrap(),
                TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
            ),
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            metadata: VersionMetadata::default_for_existing(),
        };

        // Should succeed - Full deltas can be persisted
        let entry = convert_node_version(&version).unwrap();
        assert_eq!(entry.version_id, 2);
        // Vector should be in properties as a full vector
        assert!(!entry.properties.entries.is_empty());
    }

    #[test]
    fn test_persist_delta_with_sparse_vector_delta_fails() {
        // Test that VectorDelta::Sparse causes persistence to fail (prevents data loss)
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32), (100, 0.6f32)]),
            },
        );

        let label = GLOBAL_INTERNER.intern("Document").unwrap();
        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            temporal: BiTemporalInterval::new(
                TimeRange::new(2000.into(), 3000.into()).unwrap(),
                TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
            ),
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            metadata: VersionMetadata::default_for_existing(),
        };

        // Should FAIL - Sparse deltas cannot be persisted without materialization
        let result = convert_node_version(&version);
        assert!(result.is_err(), "Should fail to persist Sparse delta");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("VectorDelta::Sparse"),
            "Error should mention Sparse delta: {}",
            err_msg
        );
        assert!(
            err_msg.contains("materialize_vector_deltas"),
            "Error should mention materialization: {}",
            err_msg
        );
    }

    #[test]
    fn test_materialize_vector_deltas() {
        // Test that PropertyDelta::materialize_vector_deltas() correctly converts Sparse to Full
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        // Create base properties with a vector
        let base_embedding = vec![0.1f32; 384];
        let base_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&base_embedding))
            .build();

        // Create a delta with a sparse change
        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32), (100, 0.6f32)]),
            },
        );

        // Materialize the delta
        delta.materialize_vector_deltas(&base_props).unwrap();

        // After materialization:
        // 1. vector_deltas should be empty
        // 2. changed should contain the full vector
        assert_eq!(
            delta.vector_deltas.len(),
            0,
            "vector_deltas should be empty after materialization"
        );
        assert_eq!(
            delta.changed.len(),
            1,
            "changed should contain the materialized vector"
        );

        let materialized = delta.changed.get(&embedding_key).unwrap();
        let materialized_vec = materialized.as_vector().unwrap();
        assert_eq!(materialized_vec.len(), 384);
        assert_eq!(
            materialized_vec[0], 0.5f32,
            "First element should be updated"
        );
        assert_eq!(
            materialized_vec[100], 0.6f32,
            "Element 100 should be updated"
        );
        assert_eq!(
            materialized_vec[50], 0.1f32,
            "Unchanged element should remain"
        );
    }

    #[test]
    fn test_materialize_vector_deltas_missing_base() {
        // Test that materialization fails gracefully when base property is missing
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();
        let base_props = PropertyMapBuilder::new().build(); // Empty base

        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32)]),
            },
        );

        // Should fail - base property not found
        let result = delta.materialize_vector_deltas(&base_props);
        assert!(result.is_err(), "Should fail when base property missing");
    }

    #[test]
    fn test_persist_materialized_delta_succeeds() {
        // Round-trip test: create sparse delta, materialize it, then persist
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        // Create base properties
        let base_embedding = vec![0.1f32; 384];
        let base_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&base_embedding))
            .build();

        // Create delta with sparse change
        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32), (100, 0.6f32)]),
            },
        );

        // Materialize BEFORE persistence
        delta.materialize_vector_deltas(&base_props).unwrap();

        // Create version with materialized delta
        let label = GLOBAL_INTERNER.intern("Document").unwrap();
        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            temporal: BiTemporalInterval::new(
                TimeRange::new(2000.into(), 3000.into()).unwrap(),
                TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
            ),
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            metadata: VersionMetadata::default_for_existing(),
        };

        // Should succeed - delta is now materialized
        let entry = convert_node_version(&version).unwrap();
        assert_eq!(entry.version_id, 2);
        assert!(
            !entry.properties.entries.is_empty(),
            "Should have materialized vector property"
        );
    }
}
