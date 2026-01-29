use super::GallifreyDB;
use crate::api::transaction::types::TxId;
use crate::core::GLOBAL_INTERNER;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::time;
use crate::core::version::VersionMetadata;
use crate::storage::index_persistence::IndexPersistenceManager;
use crate::storage::index_persistence::graph::{load_graph_index, restore_property_map};
use crate::storage::index_persistence::operations::load_vector_indexes;
use crate::storage::index_persistence::temporal::{
    load_temporal_index, restore_into_historical_storage,
};
use crate::utils::error::Result;
use crate::utils::lock::MutexExt;
use std::sync::Arc;

pub(super) fn restore_indexes(
    db: &GallifreyDB,
    manager: &Arc<IndexPersistenceManager>,
) -> Result<()> {
    // Try to load manifest and string interner, but don't fail if manifest doesn't exist yet
    // (manifest is only saved on shutdown, not during background persistence)
    match manager.load_manifest_and_strings() {
        Ok(_) => {}                      // Successfully loaded
        Err(e) if e.is_not_found() => {} // Expected on first run
        Err(e) => eprintln!("Warning: Failed to load manifest: {}", e),
    }

    // Try to restore graph data even if manifest loading failed
    let graph_path = manager.graph_path().join("adjacency.idx");
    if graph_path.exists() {
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
                if max_node_id > 0 && let Ok(mut node_gen) = db.node_id_gen.lock_or_err() {
                    *node_gen = crate::core::id::IdGenerator::with_start(max_node_id + 1);
                }
                if max_edge_id > 0 && let Ok(mut edge_gen) = db.edge_id_gen.lock_or_err() {
                    *edge_gen = crate::core::id::IdGenerator::with_start(max_edge_id + 1);
                }
                // Initialize version ID generator from max persisted version_id
                if max_version_id > 0 && let Ok(mut version_gen) = db.version_id_gen.lock_or_err() {
                    *version_gen = crate::core::id::IdGenerator::with_start(max_version_id + 1);
                }

                // Restore nodes with explicit error tracking
                for persisted_node in &graph_data.nodes {
                    // Validate label exists in string interner
                    let label_str = match GLOBAL_INTERNER.resolve(
                        crate::core::InternedString::from_raw(persisted_node.label_idx),
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

                    let _ = db.current.insert_node_direct(node, current_time);
                    nodes_loaded += 1;
                }

                // Restore edges with explicit error tracking
                for persisted_edge in &graph_data.edges {
                    // Validate label exists in string interner
                    let label_str = match GLOBAL_INTERNER.resolve(
                        crate::core::InternedString::from_raw(persisted_edge.label_idx),
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

                    let _ = db.current.insert_edge_direct(edge);
                    edges_loaded += 1;
                }

                // Log restoration summary
                let nodes_skipped = total_nodes - nodes_loaded;
                let edges_skipped = total_edges - edges_loaded;

                if nodes_skipped > 0 || edges_skipped > 0 {
                    eprintln!(
                        "Index restoration completed with data loss:\n\
                         Nodes: {}/{} loaded ({} skipped - {} label errors, {} property errors, {} version errors)\n\
                         Edges: {}/{} loaded ({} skipped - {} label errors, {} property errors, {} version errors)",
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
                    db.current.import_csr(
                        graph_data.outgoing_node_ids,
                        graph_data.outgoing_offsets,
                        graph_data.outgoing_neighbors,
                        graph_data.incoming_node_ids,
                        graph_data.incoming_offsets,
                        graph_data.incoming_neighbors,
                    );
                } else {
                    // Fallback for older index files without CSR data
                    db.current.compact_adjacency();
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
        match load_temporal_index(&temporal_path) {
            Ok(temporal_data) => {
                // Restore versions into historical storage
                // Labels are now stored directly in the persisted entries
                let mut historical_guard = db.historical.write();
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
    if let Err(e) = load_vector_indexes(&db.current, manager) {
        eprintln!("Warning: Failed to load vector indexes: {}", e);
    }

    Ok(())
}
