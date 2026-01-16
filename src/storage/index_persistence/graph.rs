//! Graph index persistence.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use crate::core::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};

use super::error::{IndexPersistenceError, Result};
use super::formats::{GraphIndexData, PersistedPropertyMap, PersistedPropertyValue};
use super::{GRAPH_MAGIC, MANIFEST_VERSION};

/// Convert PropertyValue to PersistedPropertyValue.
///
/// # Errors
///
/// Returns an error if:
/// - The property contains an Array (not yet supported)
/// - String interning fails (interner out of capacity)
pub fn persist_property_value(value: &PropertyValue) -> Result<PersistedPropertyValue> {
    Ok(match value {
        PropertyValue::Null => PersistedPropertyValue::Null,
        PropertyValue::Bool(b) => PersistedPropertyValue::Bool(*b),
        PropertyValue::Int(i) => PersistedPropertyValue::Int(*i),
        PropertyValue::Float(f) => PersistedPropertyValue::Float(*f),
        PropertyValue::String(s) => {
            let interned = GLOBAL_INTERNER.intern(s.as_ref()).map_err(|e| {
                IndexPersistenceError::Serialization(format!("Failed to intern string: {}", e))
            })?;
            PersistedPropertyValue::String(interned.as_u32())
        }
        PropertyValue::Bytes(b) => PersistedPropertyValue::Bytes(b.to_vec()),
        PropertyValue::Vector(v) => PersistedPropertyValue::Vector(v.to_vec()),
        // Array variant exists but is not yet supported in serialization
        PropertyValue::Array(_) => {
            return Err(IndexPersistenceError::Serialization(
                "Array properties are not yet supported for persistence. \
                 This prevents silent data loss. Support will be added in a future update."
                    .to_string(),
            ));
        }
    })
}

/// Convert PersistedPropertyValue back to PropertyValue.
///
/// # Errors
///
/// Returns an error if:
/// - An interned string ID cannot be resolved (data corruption)
pub fn restore_property_value(persisted: &PersistedPropertyValue) -> Result<PropertyValue> {
    Ok(match persisted {
        PersistedPropertyValue::Null => PropertyValue::Null,
        PersistedPropertyValue::Bool(b) => PropertyValue::Bool(*b),
        PersistedPropertyValue::Int(i) => PropertyValue::Int(*i),
        PersistedPropertyValue::Float(f) => PropertyValue::Float(*f),
        PersistedPropertyValue::String(idx) => {
            let s = GLOBAL_INTERNER
                .resolve(crate::core::InternedString::from_raw(*idx))
                .ok_or_else(|| {
                    IndexPersistenceError::Serialization(format!(
                        "Failed to resolve interned string with ID: {}. \
                         This likely indicates data corruption.",
                        idx
                    ))
                })?;
            PropertyValue::String(s)
        }
        PersistedPropertyValue::Bytes(b) => PropertyValue::Bytes(Arc::from(b.as_slice())),
        PersistedPropertyValue::Vector(v) => PropertyValue::Vector(Arc::from(v.as_slice())),
    })
}

/// Convert PropertyMap to PersistedPropertyMap.
///
/// # Errors
///
/// Returns an error if any property value fails to persist.
pub fn persist_property_map(props: &PropertyMap) -> Result<PersistedPropertyMap> {
    let mut entries = Vec::with_capacity(props.len());
    for (k, v) in props.iter() {
        // k is already an InternedString (PropertyKey)
        entries.push((k.as_u32(), persist_property_value(v)?));
    }
    Ok(PersistedPropertyMap { entries })
}

/// Convert PersistedPropertyMap back to PropertyMap.
///
/// # Errors
///
/// Returns an error if:
/// - Any property key cannot be resolved (data corruption)
/// - Any property value fails to restore
pub fn restore_property_map(persisted: &PersistedPropertyMap) -> Result<PropertyMap> {
    let mut builder = PropertyMapBuilder::new();
    for (key_idx, value) in &persisted.entries {
        let key_id = crate::core::InternedString::from_raw(*key_idx);
        let key_arc = GLOBAL_INTERNER.resolve(key_id).ok_or_else(|| {
            IndexPersistenceError::Serialization(format!(
                "Failed to resolve interned property key with ID: {}. \
                 This likely indicates data corruption.",
                key_idx
            ))
        })?;
        builder = builder.insert(key_arc.as_ref(), restore_property_value(value)?);
    }
    Ok(builder.build())
}

/// Save graph index data to disk.
pub fn save_graph_index(data: &GraphIndexData, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(data);
    fs::write(path, encoded)?;
    Ok(())
}

/// Load graph index data from disk.
pub fn load_graph_index(path: &Path) -> Result<GraphIndexData> {
    let bytes = fs::read(path)?;
    let data: GraphIndexData = bitcode::decode(&bytes)?;

    if data.magic != GRAPH_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: GRAPH_MAGIC,
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

/// Create a new empty GraphIndexData.
pub fn new_graph_index_data() -> GraphIndexData {
    GraphIndexData {
        magic: GRAPH_MAGIC,
        version: MANIFEST_VERSION,
        node_count: 0,
        edge_count: 0,
        nodes: Vec::new(),
        edges: Vec::new(),
        outgoing_offsets: Vec::new(),
        outgoing_neighbors: Vec::new(),
        incoming_offsets: Vec::new(),
        incoming_neighbors: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::PersistedNode;
    use tempfile::tempdir;

    #[test]
    fn test_property_value_round_trip() {
        // Test various property types
        let values = vec![
            PropertyValue::Null,
            PropertyValue::Bool(true),
            PropertyValue::Int(42),
            PropertyValue::Float(2.71), // e approximation, not PI
            PropertyValue::String(Arc::from("test")),
            PropertyValue::Bytes(Arc::from(vec![1u8, 2, 3].as_slice())),
            PropertyValue::Vector(Arc::from(vec![1.0f32, 2.0, 3.0].as_slice())),
        ];

        for value in values {
            let persisted = persist_property_value(&value).unwrap();
            let restored = restore_property_value(&persisted).unwrap();

            // Compare string representation for simplicity
            assert_eq!(format!("{:?}", value), format!("{:?}", restored));
        }
    }

    #[test]
    fn test_graph_index_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");

        let mut data = new_graph_index_data();
        data.node_count = 2;
        data.nodes.push(PersistedNode {
            id: 1,
            label_idx: GLOBAL_INTERNER.intern("Person").unwrap().as_u32(),
            properties: PersistedPropertyMap { entries: vec![] },
        });
        data.nodes.push(PersistedNode {
            id: 2,
            label_idx: GLOBAL_INTERNER.intern("Document").unwrap().as_u32(),
            properties: PersistedPropertyMap { entries: vec![] },
        });

        save_graph_index(&data, &path).unwrap();
        let loaded = load_graph_index(&path).unwrap();

        assert_eq!(loaded.node_count, 2);
        assert_eq!(loaded.nodes.len(), 2);
    }

    #[test]
    fn test_array_property_errors() {
        // Test that Array properties properly error instead of silently losing data
        let array_value = PropertyValue::Array(Arc::from(vec![
            PropertyValue::Int(1),
            PropertyValue::Int(2),
            PropertyValue::Int(3),
        ]));

        let result = persist_property_value(&array_value);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Array properties are not yet supported"));
    }

    #[test]
    fn test_missing_string_interned_id_errors() {
        // Test that missing interned string IDs properly error
        let persisted = PersistedPropertyValue::String(999999); // Non-existent ID

        let result = restore_property_value(&persisted);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to resolve interned string"));
    }
}
