use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::utils::error::StorageError;
use parking_lot::RwLock;
use std::sync::Arc;

use super::loader::IndexPersistenceManager;
use super::tracker::PersistenceTracker;

/// Persist vector indexes to disk.
pub fn persist_vector_indexes(
    current: &Arc<CurrentStorage>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
) -> crate::utils::error::Result<()> {
    use crate::storage::index_persistence::formats::PersistedHnswConfig;
    use crate::storage::index_persistence::vector::{
        new_vector_mappings, new_vector_meta, save_vector_mappings, save_vector_meta,
    };

    // Save string interner first (required by all indexes)
    manager.save_string_interner().map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
    })?;

    // Get list of all vector indexes
    let vector_indexes_info = current.list_vector_indexes();

    // Persist each vector index
    for info in vector_indexes_info {
        let property_name = &info.property_name;

        // Create vector directory
        let vec_path = manager.vector_path(property_name);
        std::fs::create_dir_all(&vec_path).map_err(|e| {
            StorageError::PersistenceError(format!(
                "Failed to create vector index directory for {}: {}",
                property_name, e
            ))
        })?;

        // Get the index, config, vector count, and mappings
        let (index, config, vector_count, id_mappings) = current
            .get_vector_index_for_persistence(property_name)
            .ok_or_else(|| {
                StorageError::PersistenceError(format!(
                    "Failed to get vector index for persistence: {}",
                    property_name
                ))
            })?;

        // Save HNSW index using usearch native format
        let usearch_path = vec_path.join("current.usearch");

        // Use the VectorIndex trait's save method
        use crate::index::vector::VectorIndex;
        index.save(&usearch_path).map_err(|e| {
            StorageError::PersistenceError(format!("Failed to save usearch index: {}", e))
        })?;

        // Create and save metadata
        let hnsw_config = PersistedHnswConfig {
            m: config.m as u16,
            ef_construction: config.ef_construction as u16,
            ef_search: config.ef_search as u16,
        };

        let mut vector_meta = new_vector_meta(
            property_name,
            config.dimensions as u32,
            config.metric.to_u8(),
            hnsw_config,
        );

        // Set the actual vector count
        vector_meta.vector_count = vector_count as u64;

        save_vector_meta(&vector_meta, &vec_path.join("meta.idx")).map_err(|e| {
            StorageError::PersistenceError(format!(
                "Failed to save vector metadata for {}: {}",
                property_name, e
            ))
        })?;

        // Create and save mappings
        use crate::storage::index_persistence::formats::VectorMapping;
        let mut vector_mappings = new_vector_mappings();
        vector_mappings.count = id_mappings.len() as u64;
        vector_mappings.mappings = id_mappings
            .into_iter()
            .map(|(node_id, usearch_key)| VectorMapping {
                node_id,
                usearch_key,
            })
            .collect();

        save_vector_mappings(&vector_mappings, &vec_path.join("mappings.idx")).map_err(|e| {
            StorageError::PersistenceError(format!(
                "Failed to save vector mappings for {}: {}",
                property_name, e
            ))
        })?;
    }

    tracker.reset_vector_mutations();
    Ok(())
}

/// Load vector indexes from disk.
pub fn load_vector_indexes(
    current: &CurrentStorage,
    manager: &Arc<IndexPersistenceManager>,
) -> crate::utils::error::Result<()> {
    use crate::index::vector::{DistanceMetric, HnswConfig, HnswIndex};
    use crate::storage::index_persistence::vector::{load_vector_mappings, load_vector_meta};

    // Get vector directory
    let vector_base = manager.indexes_path().join("vector");
    if !vector_base.exists() {
        return Ok(()); // No vector indexes to load
    }

    // Iterate through all subdirectories (one per property)
    let entries = std::fs::read_dir(&vector_base).map_err(|e| {
        StorageError::PersistenceError(format!("Failed to read vector directory: {}", e))
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            StorageError::PersistenceError(format!("Failed to read directory entry: {}", e))
        })?;

        let vec_path = entry.path();
        if !vec_path.is_dir() {
            continue;
        }

        let property_name = vec_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                StorageError::PersistenceError("Invalid vector directory name".to_string())
            })?;

        // Load metadata
        let meta_path = vec_path.join("meta.idx");
        if !meta_path.exists() {
            eprintln!(
                "Warning: Skipping vector index '{}': metadata not found",
                property_name
            );
            continue;
        }

        let meta = load_vector_meta(&meta_path).map_err(|e| {
            StorageError::PersistenceError(format!(
                "Failed to load vector metadata for {}: {}",
                property_name, e
            ))
        })?;

        // Convert metric from u8 to DistanceMetric using from_u8()
        let metric = match DistanceMetric::from_u8(meta.metric) {
            Ok(m) => m,
            Err(_) => {
                eprintln!(
                    "Warning: Skipping vector index '{}': unknown metric {}",
                    property_name, meta.metric
                );
                continue;
            }
        };

        // Create config from metadata
        let config = HnswConfig::new(meta.dimensions as usize, metric)
            .with_m(meta.hnsw_config.m as usize)
            .with_ef_construction(meta.hnsw_config.ef_construction as usize)
            .with_ef_search(meta.hnsw_config.ef_search as usize);

        // Load or create index
        let usearch_path = vec_path.join("current.usearch");
        let index = if usearch_path.exists() {
            // Load existing index
            HnswIndex::load(&usearch_path, config.clone()).map_err(|e| {
                StorageError::PersistenceError(format!(
                    "Failed to load usearch index for {}: {}",
                    property_name, e
                ))
            })?
        } else {
            // Create new empty index
            HnswIndex::new(config.clone()).map_err(|e| {
                StorageError::PersistenceError(format!(
                    "Failed to create HNSW index for {}: {}",
                    property_name, e
                ))
            })?
        };

        // Load mappings and restore them to the index
        let mappings_path = vec_path.join("mappings.idx");
        if mappings_path.exists() {
            let mappings_data = load_vector_mappings(&mappings_path).map_err(|e| {
                StorageError::PersistenceError(format!(
                    "Failed to load vector mappings for {}: {}",
                    property_name, e
                ))
            })?;

            // Restore ID mappings
            // Note: The usearch index already has the vectors loaded from disk,
            // but we need to restore the NodeId <-> usearch_key mappings
            use crate::core::id::NodeId;
            for mapping in &mappings_data.mappings {
                match NodeId::new(mapping.node_id) {
                    Ok(node_id) => {
                        index.restore_mapping(node_id, mapping.usearch_key);
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: Skipping invalid NodeId {} in vector index '{}': {}",
                            mapping.node_id, property_name, e
                        );
                    }
                }
            }
        }

        // Register index with CurrentStorage
        current.register_vector_index(property_name, index, config);

        println!(
            "✓ Loaded vector index '{}': {} dimensions, {} vectors",
            property_name, meta.dimensions, meta.vector_count
        );
    }

    Ok(())
}

/// Persist graph index to disk.
pub fn persist_graph_index(
    current: &Arc<CurrentStorage>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
) -> crate::utils::error::Result<()> {
    use crate::storage::index_persistence::graph::{
        new_graph_index_data, persist_property_map, save_graph_index,
    };
    use crate::storage::index_persistence::{PersistedEdge, PersistedNode};

    let mut graph_data = new_graph_index_data();

    // Stream all nodes without collecting into intermediate Vec (prevents OOM on large graphs)
    for node in current.all_nodes() {
        let properties = persist_property_map(&node.properties).map_err(|e| {
            StorageError::PersistenceError(format!("Failed to persist node properties: {}", e))
        })?;

        graph_data.nodes.push(PersistedNode {
            id: node.id.as_u64(),
            label_idx: node.label.as_u32(),
            version_id: node.current_version.as_u64(),
            properties,
        });
    }
    graph_data.node_count = graph_data.nodes.len() as u64;

    // Stream all edges without collecting into intermediate Vec (prevents OOM on large graphs)
    for edge in current.all_edges() {
        let properties = persist_property_map(&edge.properties).map_err(|e| {
            StorageError::PersistenceError(format!("Failed to persist edge properties: {}", e))
        })?;

        graph_data.edges.push(PersistedEdge {
            id: edge.id.as_u64(),
            source_id: edge.source.as_u64(),
            target_id: edge.target.as_u64(),
            label_idx: edge.label.as_u32(),
            version_id: edge.current_version.as_u64(),
            properties,
        });
    }
    graph_data.edge_count = graph_data.edges.len() as u64;

    // Export CSR adjacency structures for fast loading
    let (outgoing_node_ids, outgoing_offsets, outgoing_neighbors) = current.export_outgoing_csr();
    let (incoming_node_ids, incoming_offsets, incoming_neighbors) = current.export_incoming_csr();

    graph_data.outgoing_node_ids = outgoing_node_ids;
    graph_data.outgoing_offsets = outgoing_offsets;
    graph_data.outgoing_neighbors = outgoing_neighbors;
    graph_data.incoming_node_ids = incoming_node_ids;
    graph_data.incoming_offsets = incoming_offsets;
    graph_data.incoming_neighbors = incoming_neighbors;

    // Save to disk
    let graph_path = manager.graph_path().join("adjacency.idx");

    std::fs::create_dir_all(manager.graph_path()).map_err(|e| {
        StorageError::PersistenceError(format!("Failed to create graph directory: {}", e))
    })?;

    save_graph_index(&graph_data, &graph_path).map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save graph index: {}", e))
    })?;

    tracker.reset_graph_mutations();
    Ok(())
}

/// Persist temporal index to disk.
pub fn persist_temporal_index(
    historical: &Arc<RwLock<HistoricalStorage>>,
    _temporal_indexes: &Arc<TemporalIndexes>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
) -> crate::utils::error::Result<()> {
    use crate::storage::index_persistence::temporal::{
        convert_edge_version, convert_node_version, new_temporal_index_data, save_temporal_index,
    };

    // First save string interner (versions depend on interned strings)
    manager.save_string_interner().map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
    })?;

    // Get read lock on historical storage
    let historical_guard = historical.read();

    // Convert all node versions
    let mut node_versions = Vec::new();
    for version in historical_guard.get_node_versions().values() {
        let entry = convert_node_version(version).map_err(|e| {
            StorageError::PersistenceError(format!(
                "Failed to convert node version {}: {}",
                version.id.as_u64(),
                e
            ))
        })?;
        node_versions.push(entry);
    }

    // Convert all edge versions
    let mut edge_versions = Vec::new();
    for version in historical_guard.get_edge_versions().values() {
        let entry = convert_edge_version(version).map_err(|e| {
            StorageError::PersistenceError(format!(
                "Failed to convert edge version {}: {}",
                version.id.as_u64(),
                e
            ))
        })?;
        edge_versions.push(entry);
    }

    // Create temporal index data
    let mut temporal_data = new_temporal_index_data();
    temporal_data.node_versions = node_versions;
    temporal_data.edge_versions = edge_versions;

    // Note: Anchors are not stored separately - they're identified by version_type in the entries

    // Drop the lock before disk I/O
    drop(historical_guard);

    // Save to disk
    let temporal_path = manager.indexes_path().join("temporal").join("versions.idx");
    save_temporal_index(&temporal_data, &temporal_path).map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save temporal index: {}", e))
    })?;

    tracker.reset_temporal_mutations();
    Ok(())
}

/// Persist string interner to disk.
pub fn persist_string_interner(
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
) -> crate::utils::error::Result<()> {
    manager.save_string_interner().map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
    })?;

    tracker.reset_string_mutations();
    Ok(())
}

/// Persist all indexes on shutdown.
pub fn persist_all_indexes(
    current: &Arc<CurrentStorage>,
    historical: &Arc<RwLock<HistoricalStorage>>,
    temporal_indexes: &Arc<TemporalIndexes>,
    wal: &Arc<ConcurrentWalSystem>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
) -> crate::utils::error::Result<()> {
    // Persist all indexes - log errors but continue with remaining indexes
    if let Err(e) = persist_string_interner(manager, tracker) {
        eprintln!("Failed to persist string interner: {}", e);
    }
    if let Err(e) = persist_graph_index(current, manager, tracker) {
        eprintln!("Failed to persist graph index: {}", e);
    }
    if let Err(e) = persist_temporal_index(historical, temporal_indexes, manager, tracker) {
        eprintln!("Failed to persist temporal index: {}", e);
    }
    if let Err(e) = persist_vector_indexes(current, manager, tracker) {
        eprintln!("Failed to persist vector indexes: {}", e);
    }

    // Save manifest
    build_and_save_manifest(wal, current, manager)
}

/// Builds and saves the index manifest file.
///
/// This function is shared between manual persistence (`persist_indexes`) and
/// shutdown persistence (`persist_all_indexes`) to avoid code duplication.
pub fn build_and_save_manifest(
    wal: &Arc<ConcurrentWalSystem>,
    current: &Arc<CurrentStorage>,
    manager: &Arc<IndexPersistenceManager>,
) -> crate::utils::error::Result<()> {
    use crate::storage::index_persistence::formats::{
        GraphIndexManifestEntry, IndexManifest, StringInternerManifestEntry,
    };

    let current_lsn = wal.current_lsn().0;
    let mut manifest = IndexManifest::new(current_lsn);

    // Add string interner entry
    manifest.string_interner = Some(StringInternerManifestEntry {
        interner_file: "strings/interner.idx".to_string(),
        string_count: crate::core::GLOBAL_INTERNER.len() as u64,
    });

    // Add graph index entry if we have nodes/edges
    // Use node_count() and edge_count() for O(1) performance instead of iterating.
    let node_count = current.node_count();
    let edge_count = current.edge_count();
    if node_count > 0 || edge_count > 0 {
        manifest.graph_index = Some(GraphIndexManifestEntry {
            adjacency_file: "graph/adjacency.idx".to_string(),
            node_count: node_count as u64,
            edge_count: edge_count as u64,
        });
    }

    manager
        .save_manifest(&manifest)
        .map_err(|e| StorageError::PersistenceError(format!("Failed to save manifest: {}", e)))?;

    Ok(())
}
