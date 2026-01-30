//! Graph index persistence.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use crc32fast::Hasher;

use crate::core::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};

use super::error::{IndexPersistenceError, Result};
use super::formats::{
    GraphIndexData, GraphIndexDelta, PersistedPropertyMap, PersistedPropertyValue,
};
use super::{DELTA_MAGIC, GRAPH_MAGIC, MANIFEST_VERSION};

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
        // SparseVector variant exists but is not yet supported in index persistence
        PropertyValue::SparseVector(_) => {
            return Err(IndexPersistenceError::Serialization(
                "SparseVector properties are not yet supported for index persistence. \
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
/// - Vector dimensions exceed MAX_VECTOR_DIMENSIONS (DoS protection)
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
        PersistedPropertyValue::Vector(v) => {
            // Validate vector size to prevent DoS via maliciously large vectors
            if v.len() > super::MAX_VECTOR_DIMENSIONS {
                return Err(IndexPersistenceError::SizeLimitExceeded {
                    message: format!(
                        "Vector dimension {} exceeds maximum allowed dimension {}",
                        v.len(),
                        super::MAX_VECTOR_DIMENSIONS
                    ),
                });
            }
            PropertyValue::Vector(Arc::from(v.as_slice()))
        }
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

/// Save graph index data to disk with CRC32 checksum using atomic write.
///
/// Format: `[bitcode_data][crc32_checksum_4_bytes]`
///
/// Uses write-temp-then-rename to prevent corruption on crash.
pub fn save_graph_index(data: &GraphIndexData, path: &Path) -> Result<()> {
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

/// Load graph index data from disk and validate CRC32 checksum.
///
/// Automatically detects zstd compression by checking for magic bytes.
pub fn load_graph_index(path: &Path) -> Result<GraphIndexData> {
    let bytes = fs::read(path)?;

    // Check minimum size (must have at least 4 bytes for CRC)
    if bytes.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data_slice, checksum_bytes) = bytes.split_at(bytes.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Check for zstd compression (magic bytes: 0x28B52FFD in big-endian)
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
    let decompressed_data;
    let data_to_verify = if data_slice.len() >= 4 && data_slice[..4] == ZSTD_MAGIC {
        // Decompress the data
        decompressed_data = zstd::decode_all(data_slice).map_err(|e| {
            IndexPersistenceError::Serialization(format!("zstd decompression failed: {}", e))
        })?;
        &decompressed_data[..]
    } else {
        data_slice
    };

    // Verify checksum (against decompressed data if compressed)
    let mut hasher = Hasher::new();
    hasher.update(data_to_verify);
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
    let data: GraphIndexData = bitcode::decode(data_to_verify)?;

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
        outgoing_node_ids: Vec::new(),
        outgoing_offsets: Vec::new(),
        outgoing_neighbors: Vec::new(),
        incoming_node_ids: Vec::new(),
        incoming_offsets: Vec::new(),
        incoming_neighbors: Vec::new(),
    }
}

/// Save graph index data with zstd compression.
///
/// # Arguments
///
/// * `data` - The graph index data to save
/// * `path` - The file path to write to
/// * `compression_level` - zstd compression level (0-22, default 3)
///
/// # Errors
///
/// Returns an error if serialization, compression, or file write fails.
pub fn save_graph_index_compressed(
    data: &GraphIndexData,
    path: &Path,
    compression_level: i32,
) -> Result<()> {
    let encoded = bitcode::encode(data);

    // Calculate CRC32 of uncompressed data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    // Compress the encoded data
    let compressed = zstd::encode_all(&encoded[..], compression_level).map_err(|e| {
        IndexPersistenceError::Serialization(format!("zstd compression failed: {}", e))
    })?;

    // Write: compressed_data + checksum (of uncompressed)
    let mut data_with_checksum = compressed;
    data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

    super::atomic_write(path, &data_with_checksum)?;
    Ok(())
}

/// Load graph index data using memory-mapped file for efficient large file handling.
///
/// This function uses memory-mapping to avoid loading the entire file into memory,
/// which can be more efficient for large index files and enables working with
/// indexes larger than available RAM.
///
/// # Arguments
///
/// * `path` - The file path to read from
///
/// # Errors
///
/// Returns an error if:
/// - File cannot be opened or memory-mapped
/// - File is corrupted (CRC mismatch)
/// - Decompression fails
/// - Deserialization fails
/// - Magic bytes or version are invalid
///
/// # Examples
///
/// ```ignore
/// use gallifreydb::storage::index_persistence::graph::load_graph_index_mmap;
///
/// let data = load_graph_index_mmap(&path)?;
/// ```
pub fn load_graph_index_mmap(path: &Path) -> Result<GraphIndexData> {
    use memmap2::Mmap;
    use std::fs::File;

    // Open file and create memory map
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };

    // Check minimum size (must have at least 4 bytes for CRC)
    if mmap.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data_slice, checksum_bytes) = mmap.split_at(mmap.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Check for zstd compression (magic bytes: 0x28B52FFD in big-endian)
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
    let decompressed_data;
    let data_to_verify = if data_slice.len() >= 4 && data_slice[..4] == ZSTD_MAGIC {
        // Decompress the data
        decompressed_data = zstd::decode_all(data_slice).map_err(|e| {
            IndexPersistenceError::Serialization(format!("zstd decompression failed: {}", e))
        })?;
        &decompressed_data[..]
    } else {
        data_slice
    };

    // Verify checksum (against decompressed data if compressed)
    let mut hasher = Hasher::new();
    hasher.update(data_to_verify);
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
    let data: GraphIndexData = bitcode::decode(data_to_verify)?;

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

/// Save graph index delta with zstd compression.
///
/// This function saves only the changes between the base and modified graph data,
/// significantly reducing the size of incremental saves. Tracks additions, modifications,
/// and deletions for complete change tracking.
///
/// # Arguments
///
/// * `base` - The base graph index data (previous snapshot)
/// * `modified` - The modified graph index data (current state)
/// * `path` - The file path to write the delta to
/// * `compression_level` - zstd compression level (0-22, default 3)
///
/// # Errors
///
/// Returns an error if serialization, compression, or file write fails.
///
/// # Examples
///
/// ```ignore
/// use gallifreydb::storage::index_persistence::graph::{
///     save_graph_index_delta, load_graph_index, save_graph_index_compressed
/// };
///
/// // Save base snapshot
/// save_graph_index_compressed(&base_data, &base_path, 3)?;
///
/// // ... make changes to create modified_data ...
///
/// // Save only the delta (additions, modifications, deletions)
/// save_graph_index_delta(&base_data, &modified_data, &delta_path, 3)?;
/// ```
pub fn save_graph_index_delta(
    base: &GraphIndexData,
    modified: &GraphIndexData,
    path: &Path,
    compression_level: i32,
) -> Result<()> {
    // Build lookup maps for efficient comparison
    let base_nodes: std::collections::HashMap<u64, &super::formats::PersistedNode> =
        base.nodes.iter().map(|n| (n.id, n)).collect();
    let modified_nodes: std::collections::HashMap<u64, &super::formats::PersistedNode> =
        modified.nodes.iter().map(|n| (n.id, n)).collect();

    let base_edges: std::collections::HashMap<u64, &super::formats::PersistedEdge> =
        base.edges.iter().map(|e| (e.id, e)).collect();
    let modified_edges: std::collections::HashMap<u64, &super::formats::PersistedEdge> =
        modified.edges.iter().map(|e| (e.id, e)).collect();

    // Detect added nodes (exist in modified but not in base)
    let added_nodes: Vec<_> = modified
        .nodes
        .iter()
        .filter(|node| !base_nodes.contains_key(&node.id))
        .cloned()
        .collect();

    // Detect modified nodes (exist in both but with different content)
    let modified_nodes_vec: Vec<_> = modified
        .nodes
        .iter()
        .filter(|node| {
            base_nodes
                .get(&node.id)
                .is_some_and(|base_node| *base_node != *node)
        })
        .cloned()
        .collect();

    // Detect deleted nodes (exist in base but not in modified)
    let deleted_node_ids: Vec<_> = base
        .nodes
        .iter()
        .filter(|node| !modified_nodes.contains_key(&node.id))
        .map(|node| node.id)
        .collect();

    // Detect added edges (exist in modified but not in base)
    let added_edges: Vec<_> = modified
        .edges
        .iter()
        .filter(|edge| !base_edges.contains_key(&edge.id))
        .cloned()
        .collect();

    // Detect modified edges (exist in both but with different content)
    let modified_edges_vec: Vec<_> = modified
        .edges
        .iter()
        .filter(|edge| {
            base_edges
                .get(&edge.id)
                .is_some_and(|base_edge| *base_edge != *edge)
        })
        .cloned()
        .collect();

    // Detect deleted edges (exist in base but not in modified)
    let deleted_edge_ids: Vec<_> = base
        .edges
        .iter()
        .filter(|edge| !modified_edges.contains_key(&edge.id))
        .map(|edge| edge.id)
        .collect();

    // Create delta structure
    let delta = GraphIndexDelta {
        magic: DELTA_MAGIC,
        version: MANIFEST_VERSION,
        added_nodes,
        modified_nodes: modified_nodes_vec,
        deleted_node_ids,
        added_edges,
        modified_edges: modified_edges_vec,
        deleted_edge_ids,
        new_node_count: modified.node_count,
        new_edge_count: modified.edge_count,
    };

    // Encode delta
    let encoded = bitcode::encode(&delta);

    // Calculate CRC32 of uncompressed data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    // Compress the encoded data
    let compressed = zstd::encode_all(&encoded[..], compression_level).map_err(|e| {
        IndexPersistenceError::Serialization(format!("zstd compression failed: {}", e))
    })?;

    // Write: compressed_data + checksum (of uncompressed)
    let mut data_with_checksum = compressed;
    data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

    super::atomic_write(path, &data_with_checksum)?;
    Ok(())
}

/// Load graph index data by loading base and applying delta.
///
/// This function loads a base graph index and applies a delta to reconstruct
/// the modified state. Applies all change types: deletions, modifications, and additions.
/// This is more efficient than loading the full modified state when only a small
/// portion of the graph has changed.
///
/// # Delta Application Order
///
/// 1. **Deletions**: Remove nodes/edges that were deleted
/// 2. **Modifications**: Update properties/labels of existing nodes/edges
/// 3. **Additions**: Add new nodes/edges
/// 4. **Counts**: Update node_count and edge_count
///
/// # Arguments
///
/// * `base_path` - Path to the base graph index file
/// * `delta_path` - Path to the delta file
///
/// # Errors
///
/// Returns an error if:
/// - Base or delta file cannot be loaded
/// - Files are corrupted (CRC mismatch)
/// - Decompression fails
/// - Deserialization fails
/// - Magic bytes or version are invalid
///
/// # Examples
///
/// ```ignore
/// use gallifreydb::storage::index_persistence::graph::load_graph_index_with_delta;
///
/// let reconstructed_data = load_graph_index_with_delta(&base_path, &delta_path)?;
/// ```
pub fn load_graph_index_with_delta(base_path: &Path, delta_path: &Path) -> Result<GraphIndexData> {
    // Load base index
    let mut base = load_graph_index(base_path)?;

    // Load and decompress delta
    let bytes = fs::read(delta_path)?;

    // Check minimum size (must have at least 4 bytes for CRC)
    if bytes.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: delta_path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data_slice, checksum_bytes) = bytes.split_at(bytes.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: delta_path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Check for zstd compression (magic bytes: 0x28B52FFD in big-endian)
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
    let decompressed_data;
    let data_to_verify = if data_slice.len() >= 4 && data_slice[..4] == ZSTD_MAGIC {
        // Decompress the data
        decompressed_data = zstd::decode_all(data_slice).map_err(|e| {
            IndexPersistenceError::Serialization(format!("zstd decompression failed: {}", e))
        })?;
        &decompressed_data[..]
    } else {
        data_slice
    };

    // Verify checksum (against decompressed data if compressed)
    let mut hasher = Hasher::new();
    hasher.update(data_to_verify);
    let computed_checksum = hasher.finalize();

    if computed_checksum != stored_checksum {
        return Err(IndexPersistenceError::Corrupted {
            path: delta_path.to_path_buf(),
            source: format!(
                "CRC32 checksum mismatch: expected {}, got {}",
                stored_checksum, computed_checksum
            )
            .into(),
        });
    }

    // Decode delta
    let delta: GraphIndexDelta = bitcode::decode(data_to_verify)?;

    // Validate magic and version
    if delta.magic != DELTA_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: delta_path.to_path_buf(),
            expected: DELTA_MAGIC,
            got: delta.magic,
        });
    }

    if delta.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: delta.version,
            supported: MANIFEST_VERSION,
        });
    }

    // Apply delta changes

    // 1. Handle deletions first (remove from base)
    base.nodes
        .retain(|node| !delta.deleted_node_ids.contains(&node.id));
    base.edges
        .retain(|edge| !delta.deleted_edge_ids.contains(&edge.id));

    // 2. Handle modifications (update existing entries)
    for modified_node in &delta.modified_nodes {
        if let Some(existing) = base.nodes.iter_mut().find(|n| n.id == modified_node.id) {
            *existing = modified_node.clone();
        }
    }
    for modified_edge in &delta.modified_edges {
        if let Some(existing) = base.edges.iter_mut().find(|e| e.id == modified_edge.id) {
            *existing = modified_edge.clone();
        }
    }

    // 3. Handle additions (append to base)
    base.nodes.extend(delta.added_nodes);
    base.edges.extend(delta.added_edges);

    // 4. Update counts
    base.node_count = delta.new_node_count;
    base.edge_count = delta.new_edge_count;

    Ok(base)
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
            version_id: 1,
            properties: PersistedPropertyMap { entries: vec![] },
        });
        data.nodes.push(PersistedNode {
            id: 2,
            label_idx: GLOBAL_INTERNER.intern("Document").unwrap().as_u32(),
            version_id: 2,
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Array properties are not yet supported")
        );
    }

    #[test]
    fn test_missing_string_interned_id_errors() {
        // Test that missing interned string IDs properly error
        let persisted = PersistedPropertyValue::String(999999); // Non-existent ID

        let result = restore_property_value(&persisted);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to resolve interned string")
        );
    }

    #[test]
    fn test_vector_size_limit_dos_protection() {
        // Test that vectors exceeding MAX_VECTOR_DIMENSIONS are rejected
        let oversized_vector = vec![0.0f32; super::super::MAX_VECTOR_DIMENSIONS + 1];
        let persisted = PersistedPropertyValue::Vector(oversized_vector);

        let result = restore_property_value(&persisted);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Size limit exceeded"));
        assert!(err.to_string().contains("Vector dimension"));
    }

    #[test]
    fn test_vector_at_size_limit_allowed() {
        // Test that vectors exactly at MAX_VECTOR_DIMENSIONS are allowed
        let max_vector = vec![1.0f32; super::super::MAX_VECTOR_DIMENSIONS];
        let persisted = PersistedPropertyValue::Vector(max_vector);

        let result = restore_property_value(&persisted);
        assert!(result.is_ok());
        if let PropertyValue::Vector(v) = result.unwrap() {
            assert_eq!(v.len(), super::super::MAX_VECTOR_DIMENSIONS);
        } else {
            panic!("Expected vector property");
        }
    }
}

#[cfg(test)]
#[path = "graph_delta_tests.rs"]
mod delta_tests;
