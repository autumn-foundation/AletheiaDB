//! Index Persistence Operations
//!
//! This module coordinates the high-level operations for saving and loading indexes.
//! It serves as the "Controller" for the index persistence layer, orchestrating the interaction
//! between `CurrentStorage`, `HistoricalStorage`, and the disk.
//!
//! # Persistence Strategy
//!
//! Index persistence allows AletheiaDB to restart quickly (fast cold start) without replaying
//! the entire Write-Ahead Log (WAL).
//!
//! The process involves two main phases:
//!
//! 1. **Shutdown Persistence**: When the database shuts down, all in-memory indexes are flushed to disk.
//! 2. **Startup Loading**: On restart, these indexes are memory-mapped or loaded into memory,
//!    reconstructing the database state much faster than WAL replay.
//!
//! # Dependency Order
//!
//! The order of operations is critical due to internal dependencies:
//!
//! 1. **String Interner**: Must be saved/loaded first. All other indexes use `InternedString` IDs.
//! 2. **Graph Index**: Contains the current state (nodes/edges). Defines the max IDs.
//! 3. **Temporal Index**: Contains historical versions. Links back to nodes/edges.
//! 4. **Vector Indexes**: Auxiliary indexes for semantic search.
//! 5. **Manifest**: The final commit record. If present, it guarantees a successful previous save.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::GLOBAL_INTERNER;
use crate::core::error::{Result, StorageError};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, TxId, VersionId};
use crate::core::temporal::time;
use crate::core::version::VersionMetadata;
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::IndexPersistenceManager;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;

use super::tracker::PersistenceTracker;

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;

/// Persist vector indexes to disk.
///
/// This function:
/// 1. Saves the global string interner (required for label/property lookups).
/// 2. Iterates over all active vector indexes.
/// 3. For each index, creates a directory and saves:
///    - `current.usearch`: The native HNSW index.
///    - `meta.idx`: Metadata including dimensions, metric, and configuration.
///    - `mappings.idx`: Mapping between internal `NodeId`s and usearch's `u64` keys.
pub(crate) fn persist_vector_indexes(
    current: &Arc<CurrentStorage>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: Option<&Arc<PersistenceTracker>>,
    current_lsn: u64,
) -> Result<()> {
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

    if let Some(tracker) = tracker {
        tracker.reset_vector_mutations();
        tracker.update_vector_lsn(current_lsn);
    }
    Ok(())
}

/// Load vector indexes from disk.
///
/// This function:
/// 1. Scans the vector index directory.
/// 2. For each subdirectory (representing a property), loads metadata and mappings.
/// 3. Reconstructs the HNSW index using `usearch`.
/// 4. Restores the `NodeId` <-> `u64` key mappings.
/// 5. Registers the index with `CurrentStorage`, making it immediately available for search.
///
/// Errors during loading of individual indexes are logged but do not abort the process,
/// allowing valid indexes to be loaded even if some are corrupted.
pub(crate) fn load_vector_indexes(
    current: &Arc<CurrentStorage>,
    manager: &IndexPersistenceManager,
) -> Result<()> {
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
///
/// This function saves the current state of the graph (nodes and edges) to a compact binary format.
///
/// Steps:
/// 1. Serialize all nodes and edges, including their properties and versions.
/// 2. Export CSR (Compressed Sparse Row) adjacency lists for fast graph traversal.
/// 3. Save the string interner (updated with any new strings encountered during serialization).
/// 4. Write everything to `adjacency.idx` in the graph directory.
///
/// This ensures that on restart, the graph structure can be reloaded 6-30x faster than
/// replaying the Write-Ahead Log (WAL).
pub(crate) fn persist_graph_index(
    current: &Arc<CurrentStorage>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: Option<&Arc<PersistenceTracker>>,
    current_lsn: u64,
) -> Result<()> {
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

    // Persist string interner AFTER graph conversion.
    // Property serialization can intern previously unseen string values, so
    // the interner snapshot must be updated before writing graph data that
    // references those IDs.
    manager.save_string_interner().map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
    })?;

    // Save to disk
    let graph_path = manager.graph_path().join("adjacency.idx");

    std::fs::create_dir_all(manager.graph_path()).map_err(|e| {
        StorageError::PersistenceError(format!("Failed to create graph directory: {}", e))
    })?;

    save_graph_index(&graph_data, &graph_path).map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save graph index: {}", e))
    })?;

    if let Some(tracker) = tracker {
        tracker.reset_graph_mutations();
        tracker.update_graph_lsn(current_lsn);
    }
    Ok(())
}

/// Persist temporal index to disk.
pub(crate) fn persist_temporal_index(
    historical: &Arc<RwLock<HistoricalStorage>>,
    _temporal_indexes: &Arc<TemporalIndexes>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
    current_lsn: u64,
) -> Result<()> {
    use crate::storage::index_persistence::temporal::{
        convert_edge_version, convert_node_version, new_temporal_index_data, save_temporal_index,
    };

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

    // Persist string interner AFTER converting temporal data.
    // Conversion can intern previously unseen string values from version payloads.
    // Saving the interner before conversion can leave temporal entries pointing to
    // IDs that are missing from the persisted interner snapshot.
    manager.save_string_interner().map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
    })?;

    // Save to disk
    let temporal_path = manager.indexes_path().join("temporal").join("versions.idx");
    save_temporal_index(&temporal_data, &temporal_path).map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save temporal index: {}", e))
    })?;

    tracker.reset_temporal_mutations();
    tracker.update_temporal_lsn(current_lsn);
    Ok(())
}

/// Persist string interner to disk.
pub(crate) fn persist_string_interner(
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
    current_lsn: u64,
) -> Result<()> {
    manager.save_string_interner().map_err(|e| {
        StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
    })?;

    tracker.reset_string_mutations();
    tracker.update_string_lsn(current_lsn);
    Ok(())
}

/// Persist temporal adjacency index to disk.
pub(crate) fn persist_temporal_adjacency_index(
    historical: &Arc<RwLock<HistoricalStorage>>,
    manager: &Arc<IndexPersistenceManager>,
) -> Result<()> {
    use crate::storage::index_persistence::temporal_adjacency::save_temporal_adjacency_index;

    // Get the temporal adjacency index from historical storage
    let historical_read = historical.read();
    if let Some(adj_index) = historical_read.get_temporal_adjacency_index() {
        save_temporal_adjacency_index(adj_index, manager.base_path()).map_err(|e| {
            StorageError::PersistenceError(format!(
                "Failed to save temporal adjacency index: {}",
                e
            ))
        })?;
    }

    Ok(())
}

/// Persist all indexes on shutdown.
///
/// This is the master function for clean shutdown persistence. It ensures that all
/// indexes are flushed to disk in the correct order, creating a consistent snapshot.
///
/// # Workflow
/// 1. Persist String Interner (basic vocabulary)
/// 2. Persist Graph Index (current nodes/edges)
/// 3. Persist Temporal Index (historical versions)
/// 4. Persist Temporal Adjacency (if enabled)
/// 5. Persist Vector Indexes (semantic search)
/// 6. Save Manifest (commit point)
///
/// # Manifest
/// The manifest is written last. On startup, we first check for a valid manifest.
/// If it exists and matches the WAL LSN, we know the shutdown was clean and we can
/// safely load the indexes.
pub(crate) fn persist_all_indexes(
    current: &Arc<CurrentStorage>,
    historical: &Arc<RwLock<HistoricalStorage>>,
    temporal_indexes: &Arc<TemporalIndexes>,
    wal: &Arc<ConcurrentWalSystem>,
    manager: &Arc<IndexPersistenceManager>,
    tracker: &Arc<PersistenceTracker>,
) -> Result<()> {
    let current_lsn = wal.current_lsn().0;

    // Persist all indexes - log errors but continue with remaining indexes
    if let Err(e) = persist_string_interner(manager, tracker, current_lsn) {
        eprintln!("Failed to persist string interner: {}", e);
    }
    if let Err(e) = persist_graph_index(current, manager, Some(tracker), current_lsn) {
        eprintln!("Failed to persist graph index: {}", e);
    }
    if let Err(e) =
        persist_temporal_index(historical, temporal_indexes, manager, tracker, current_lsn)
    {
        eprintln!("Failed to persist temporal index: {}", e);
    }
    if let Err(e) = persist_temporal_adjacency_index(historical, manager) {
        eprintln!("Failed to persist temporal adjacency index: {}", e);
    }
    if let Err(e) = persist_vector_indexes(current, manager, Some(tracker), current_lsn) {
        eprintln!("Failed to persist vector indexes: {}", e);
    }

    // Save manifest
    use crate::storage::index_persistence::formats::{
        GraphIndexManifestEntry, IndexManifest, StringInternerManifestEntry,
        TemporalAdjacencyIndexManifestEntry,
    };

    // Use safe LSN from tracker (min of all components)
    // On full persist, this should theoretically equal current_lsn if all succeeded.
    // If some failed, we fallback to the safe LSN.
    let safe_lsn = tracker.get_safe_manifest_lsn();
    let mut manifest = IndexManifest::new(safe_lsn);

    // Add string interner entry
    manifest.string_interner = Some(StringInternerManifestEntry {
        interner_file: "strings/interner.idx".to_string(),
        string_count: crate::core::GLOBAL_INTERNER.len() as u64,
    });

    // Add graph index entry if we have nodes/edges
    let node_count = current.all_nodes().count();
    let edge_count = current.all_edges().count();
    if node_count > 0 || edge_count > 0 {
        manifest.graph_index = Some(GraphIndexManifestEntry {
            adjacency_file: "graph/adjacency.idx".to_string(),
            node_count: node_count as u64,
            edge_count: edge_count as u64,
        });
    }

    // Add temporal adjacency index entry if configured
    let hist_read = historical.read();
    if let Some(adj_index) = hist_read.get_temporal_adjacency_index() {
        let total_entries: usize = adj_index
            .outgoing
            .iter()
            .map(|entry| entry.value().len())
            .sum();
        let node_count = adj_index.outgoing.len();

        if total_entries > 0 {
            manifest.temporal_adjacency_index = Some(TemporalAdjacencyIndexManifestEntry {
                adjacency_file: "temporal_adjacency/adjacency.idx".to_string(),
                entry_count: total_entries as u64,
                node_count: node_count as u64,
            });
        }
    }

    manager
        .save_manifest(&manifest)
        .map_err(|e| StorageError::PersistenceError(format!("Failed to save manifest: {}", e)))?;

    Ok(())
}

/// Load all indexes on startup.
///
/// This function coordinates the restoration of the entire database state from persisted indexes.
/// It follows a specific dependency order:
///
/// 1. **String Interner**: Loaded first so that `InternedString` IDs in subsequent indexes can be resolved.
/// 2. **Graph Index**: Restores the current state (nodes, edges, properties).
/// 3. **ID Generators**: Initialized based on the maximum IDs found in the graph index to prevent collisions.
/// 4. **Temporal Index**: Restores historical versions into `HistoricalStorage`.
/// 5. **Vector Indexes**: Rebuilds HNSW indexes and attaches them to the graph.
/// 6. **Adjacency Index**: Restores optimized graph traversal structures (CSR).
///
/// # Error Handling
///
/// This function is designed to be **best-effort**. It swallows most errors
/// (logging them as warnings) to allow the database to start up even if
/// some indexes are corrupted or missing. It does not return a Result
/// because it handles all errors internally, typically by falling back to
/// an empty state for the corrupted component.
pub(crate) fn load_indexes_startup(
    manager: &IndexPersistenceManager,
    current: &Arc<CurrentStorage>,
    historical: &Arc<RwLock<HistoricalStorage>>,
    node_id_gen: &Arc<IdGenerator>,
    edge_id_gen: &Arc<IdGenerator>,
    version_id_gen: &Arc<IdGenerator>,
) -> Option<u64> {
    // Try to load manifest and string interner, but don't fail if manifest doesn't exist yet
    // (manifest is only saved on shutdown, not during background persistence)
    let manifest_lsn = match manager.load_manifest_and_strings() {
        Ok(manifest) => Some(manifest.lsn), // Successfully loaded
        Err(e) => {
            if !e.is_not_found() {
                eprintln!("Warning: Failed to load manifest: {}", e);
            }
            None // Not found or error
        }
    };

    // Try to restore graph data even if manifest loading failed
    let graph_path = manager.graph_path().join("adjacency.idx");
    if graph_path.exists() {
        use crate::storage::index_persistence::graph::{load_graph_index, restore_property_map};

        match load_graph_index(&graph_path) {
            Ok(graph_data) => {
                let current_time = time::now();
                let mut max_node_id = 0u64;
                let mut max_edge_id = 0u64;

                // Track restoration statistics
                let total_nodes = graph_data.nodes.len();
                let total_edges = graph_data.edges.len();
                let mut nodes_loaded = 0usize;
                let mut edges_loaded = 0usize;
                let mut nodes_failed_label = 0usize;
                let mut nodes_failed_properties = 0usize;
                let mut nodes_failed_version = 0usize;
                let mut edges_failed_label = 0usize;
                let mut edges_failed_properties = 0usize;
                let mut edges_failed_version = 0usize;

                // Pre-calculate max IDs before inserting to avoid race conditions
                let mut max_version_id = 0u64;
                for persisted_node in &graph_data.nodes {
                    max_node_id = max_node_id.max(persisted_node.id);
                    max_version_id = max_version_id.max(persisted_node.version_id);
                }
                for persisted_edge in &graph_data.edges {
                    max_edge_id = max_edge_id.max(persisted_edge.id);
                    max_version_id = max_version_id.max(persisted_edge.version_id);
                }

                // Initialize ID generators BEFORE inserting entities to prevent collisions
                // IdGenerator uses AtomicU64, so reset_to is lock-free and thread-safe.
                if max_node_id > 0 {
                    node_id_gen.reset_to(max_node_id + 1);
                }
                if max_edge_id > 0 {
                    edge_id_gen.reset_to(max_edge_id + 1);
                }
                // Initialize version ID generator from max persisted version_id
                if max_version_id > 0 {
                    version_id_gen.reset_to(max_version_id + 1);
                }

                // Restore nodes with explicit error tracking
                for persisted_node in &graph_data.nodes {
                    // Validate label exists in string interner
                    let label_str = match GLOBAL_INTERNER.resolve_with(
                        crate::core::InternedString::from_raw(persisted_node.label_idx),
                        |s| s.to_string(),
                    ) {
                        Some(s) => s,
                        None => {
                            nodes_failed_label += 1;
                            eprintln!(
                                "Warning: Skipping node {}: label index {} not found in string interner",
                                persisted_node.id, persisted_node.label_idx
                            );
                            continue;
                        }
                    };

                    // Restore properties
                    let properties = match restore_property_map(&persisted_node.properties) {
                        Ok(p) => p,
                        Err(e) => {
                            nodes_failed_properties += 1;
                            eprintln!(
                                "Warning: Skipping node {} (label '{}'): property restoration failed: {}",
                                persisted_node.id, label_str, e
                            );
                            continue;
                        }
                    };

                    // Restore version ID from persisted data (CRITICAL for temporal provenance)
                    let version_id = match VersionId::new(persisted_node.version_id) {
                        Ok(v) => v,
                        Err(e) => {
                            nodes_failed_version += 1;
                            eprintln!(
                                "Warning: Skipping node {} (label '{}'): invalid version ID {}: {}",
                                persisted_node.id, label_str, persisted_node.version_id, e
                            );
                            continue;
                        }
                    };

                    let node = Node {
                        id: NodeId::new_unchecked(persisted_node.id),
                        label: crate::core::InternedString::from_raw(persisted_node.label_idx),
                        properties,
                        current_version: version_id,
                        metadata: VersionMetadata {
                            created_by_tx: TxId::new(0), // Restored from disk
                            commit_timestamp: Some(current_time),
                        },
                    };

                    let _ = current.insert_node_direct(node, current_time);
                    nodes_loaded += 1;
                }

                // Restore edges with explicit error tracking
                for persisted_edge in &graph_data.edges {
                    // Validate label exists in string interner
                    let label_str = match GLOBAL_INTERNER.resolve_with(
                        crate::core::InternedString::from_raw(persisted_edge.label_idx),
                        |s| s.to_string(),
                    ) {
                        Some(s) => s,
                        None => {
                            edges_failed_label += 1;
                            eprintln!(
                                "Warning: Skipping edge {}: label index {} not found in string interner",
                                persisted_edge.id, persisted_edge.label_idx
                            );
                            continue;
                        }
                    };

                    // Restore properties
                    let properties = match restore_property_map(&persisted_edge.properties) {
                        Ok(p) => p,
                        Err(e) => {
                            edges_failed_properties += 1;
                            eprintln!(
                                "Warning: Skipping edge {} (label '{}'): property restoration failed: {}",
                                persisted_edge.id, label_str, e
                            );
                            continue;
                        }
                    };

                    // Restore version ID from persisted data (CRITICAL for temporal provenance)
                    let version_id = match VersionId::new(persisted_edge.version_id) {
                        Ok(v) => v,
                        Err(e) => {
                            edges_failed_version += 1;
                            eprintln!(
                                "Warning: Skipping edge {} (label '{}'): invalid version ID {}: {}",
                                persisted_edge.id, label_str, persisted_edge.version_id, e
                            );
                            continue;
                        }
                    };

                    let edge = Edge {
                        id: EdgeId::new_unchecked(persisted_edge.id),
                        source: NodeId::new_unchecked(persisted_edge.source_id),
                        target: NodeId::new_unchecked(persisted_edge.target_id),
                        label: crate::core::InternedString::from_raw(persisted_edge.label_idx),
                        properties,
                        current_version: version_id,
                        metadata: VersionMetadata {
                            created_by_tx: TxId::new(0), // Restored from disk
                            commit_timestamp: Some(current_time),
                        },
                    };

                    let _ = current.insert_edge_direct(edge);
                    edges_loaded += 1;
                }

                // Log restoration summary
                let nodes_skipped = total_nodes - nodes_loaded;
                let edges_skipped = total_edges - edges_loaded;

                if nodes_skipped > 0 || edges_skipped > 0 {
                    eprintln!(
                        "Index restoration completed with data loss:\n                         Nodes: {}/{} loaded ({} skipped - {} label errors, {} property errors, {} version errors)\n                         Edges: {}/{} loaded ({} skipped - {} label errors, {} property errors, {} version errors)",
                        nodes_loaded,
                        total_nodes,
                        nodes_skipped,
                        nodes_failed_label,
                        nodes_failed_properties,
                        nodes_failed_version,
                        edges_loaded,
                        total_edges,
                        edges_skipped,
                        edges_failed_label,
                        edges_failed_properties,
                        edges_failed_version
                    );
                } else if total_nodes > 0 || total_edges > 0 {
                    eprintln!(
                        "Index restoration completed successfully: {} nodes, {} edges loaded",
                        nodes_loaded, edges_loaded
                    );
                }

                // Import CSR adjacency structures if available, otherwise rebuild
                if !graph_data.outgoing_offsets.is_empty()
                    && !graph_data.incoming_offsets.is_empty()
                {
                    current.import_csr(
                        graph_data.outgoing_node_ids,
                        graph_data.outgoing_offsets,
                        graph_data.outgoing_neighbors,
                        graph_data.incoming_node_ids,
                        graph_data.incoming_offsets,
                        graph_data.incoming_neighbors,
                    );
                } else {
                    // Fallback for older index files without CSR data
                    current.compact_adjacency();
                }
            }
            Err(_e) => {
                // Graph index loading failed - start with empty graph
                // This is normal if no index files exist yet
            }
        }
    }

    // Load temporal index (version history)
    let temporal_path = manager.temporal_path().join("versions.idx");
    if temporal_path.exists() {
        use crate::storage::index_persistence::temporal::{
            load_temporal_index, restore_into_historical_storage,
        };

        match load_temporal_index(&temporal_path) {
            Ok(temporal_data) => {
                // Restore versions into historical storage
                // Labels are now stored directly in the persisted entries
                let mut historical_guard = historical.write();
                match restore_into_historical_storage(&temporal_data, &mut historical_guard) {
                    Ok(()) => {
                        eprintln!(
                            "Temporal index restored: {} node versions, {} edge versions",
                            temporal_data.node_versions.len(),
                            temporal_data.edge_versions.len()
                        );
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to restore temporal versions: {}", e);
                    }
                }
                drop(historical_guard);
            }
            Err(e) => {
                eprintln!("Warning: Failed to load temporal index: {}", e);
            }
        }
    }

    // Load vector indexes
    if let Err(e) = load_vector_indexes(current, manager) {
        eprintln!("Warning: Failed to load vector indexes: {}", e);
    }

    // Load temporal adjacency index
    use crate::storage::index_persistence::temporal_adjacency::load_temporal_adjacency_index;

    let adjacency_file = manager
        .base_path()
        .join("temporal_adjacency")
        .join("adjacency.idx");
    if adjacency_file.exists() {
        match load_temporal_adjacency_index(manager.base_path()) {
            Ok(adj_index) => {
                let mut hist_write = historical.write();
                hist_write.set_temporal_adjacency_index(adj_index);
                eprintln!("Loaded temporal adjacency index from disk");
            }
            Err(e) => {
                eprintln!("Warning: Failed to load temporal adjacency index: {}", e);
            }
        }
    }

    manifest_lsn
}
