//! Checkpoint system with full state snapshot via index persistence.
//!
//! This module integrates the checkpoint system with index persistence to enable:
//! - Full state snapshots (not just metadata)
//! - Fast recovery by loading indexes from disk instead of replaying WAL
//! - LSN consistency between WAL, checkpoints, and persisted indexes
//!
//! # Architecture
//!
//! ```text
//! Checkpoint Creation:
//!   CurrentStorage ──┬── IndexPersistenceManager.save_graph()
//!   HistoricalStorage ├── IndexPersistenceManager.save_temporal()
//!   StringInterner ───┴── IndexPersistenceManager.save_strings()
//!                        │
//!                        └── Manifest (with LSN)
//!
//! Recovery:
//!   Manifest (LSN) ──────► Determine WAL replay start
//!   IndexPersistenceManager ──► Load full state
//!   WAL.read_from(manifest.lsn + 1) ──► Apply incremental changes
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use gallifreydb::storage::checkpoint::{CheckpointManager, CheckpointConfig};
//!
//! // Create checkpoint manager
//! let config = CheckpointConfig::default().data_dir("data/mydb");
//! let mut manager = CheckpointManager::new(config)?;
//!
//! // Create checkpoint (persists full state)
//! manager.create_checkpoint(current_lsn, &current, &historical)?;
//!
//! // Recover from checkpoint
//! let (current, historical, lsn) = manager.recover(&wal)?;
//! ```

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::core::GLOBAL_INTERNER;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::{
    GRAPH_MAGIC, IndexPersistenceError, IndexPersistenceManager, MANIFEST_VERSION, TEMPORAL_MAGIC,
    formats::{
        GraphIndexData, GraphIndexManifestEntry, IndexManifest, PersistedEdge, PersistedNode,
        StringInternerManifestEntry, TemporalIndexData, TemporalIndexManifestEntry,
    },
    graph::{persist_property_map, restore_property_map},
};
use crate::storage::version::VersionData;
use crate::storage::wal::LSN;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::utils::error::{Result, StorageError};

/// Convert IndexPersistenceError to our Result type.
fn persistence_err(e: IndexPersistenceError) -> crate::utils::error::Error {
    StorageError::CheckpointError {
        reason: e.to_string(),
    }
    .into()
}

/// Configuration for checkpoint behavior.
#[derive(Debug, Clone)]
pub struct CheckpointConfig {
    /// Base data directory for persistence
    pub data_dir: PathBuf,
    /// Minimum time between checkpoints
    pub checkpoint_interval: Duration,
    /// Minimum WAL entries before checkpoint
    pub min_wal_entries: u64,
    /// Whether to compress persisted indexes
    pub enable_compression: bool,
    /// Compression level (0-22, default 3)
    pub compression_level: i32,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            checkpoint_interval: Duration::from_secs(300), // 5 minutes
            min_wal_entries: 1000,
            enable_compression: true,
            compression_level: 3,
        }
    }
}

impl CheckpointConfig {
    /// Create configuration with a specific data directory.
    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            ..Default::default()
        }
    }
}

/// Manages checkpoints with full state persistence via index persistence.
///
/// This is the main coordinator between checkpoints and index persistence,
/// enabling fast recovery by loading indexes from disk instead of replaying WAL.
pub struct CheckpointManager {
    /// Configuration for checkpoint behavior
    config: CheckpointConfig,
    /// Index persistence manager for disk I/O
    persistence_manager: IndexPersistenceManager,
    /// Last checkpoint time
    last_checkpoint_time: SystemTime,
    /// Last checkpoint LSN
    last_checkpoint_lsn: LSN,
}

impl CheckpointManager {
    /// Create a new checkpoint manager.
    ///
    /// # Arguments
    ///
    /// * `config` - Checkpoint configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the data directory cannot be created.
    pub fn new(config: CheckpointConfig) -> Result<Self> {
        let persistence_manager = IndexPersistenceManager::new(&config.data_dir);
        persistence_manager
            .ensure_directories()
            .map_err(persistence_err)?;

        Ok(Self {
            config,
            persistence_manager,
            last_checkpoint_time: UNIX_EPOCH,
            last_checkpoint_lsn: LSN::initial(),
        })
    }

    /// Check if a checkpoint should be created.
    ///
    /// Returns true if:
    /// - Time since last checkpoint exceeds `checkpoint_interval`
    /// - Number of WAL entries since last checkpoint exceeds `min_wal_entries`
    pub fn should_checkpoint(&self, current_lsn: LSN) -> bool {
        // Check time threshold
        let time_elapsed = SystemTime::now()
            .duration_since(self.last_checkpoint_time)
            .unwrap_or(Duration::MAX);

        if time_elapsed >= self.config.checkpoint_interval {
            return true;
        }

        // Check LSN threshold
        let lsn_diff = current_lsn.0.saturating_sub(self.last_checkpoint_lsn.0);
        lsn_diff >= self.config.min_wal_entries
    }

    /// Create a checkpoint with full state persistence.
    ///
    /// This persists:
    /// - String interner (all interned strings)
    /// - Graph index (all nodes and edges with properties)
    /// - Temporal index (all version chains)
    /// - Manifest (LSN and metadata)
    ///
    /// # Arguments
    ///
    /// * `lsn` - Current LSN for consistency tracking
    /// * `current` - Current storage to persist
    /// * `historical` - Historical storage to persist
    ///
    /// # Errors
    ///
    /// Returns an error if persistence fails.
    pub fn create_checkpoint(
        &mut self,
        lsn: LSN,
        current: &CurrentStorage,
        historical: &HistoricalStorage,
    ) -> Result<CheckpointStats> {
        let start_time = std::time::Instant::now();
        let mut bytes_written = 0u64;

        // 1. Save string interner first (other indexes depend on it)
        self.persistence_manager
            .save_string_interner()
            .map_err(persistence_err)?;
        bytes_written += std::fs::metadata(self.persistence_manager.interner_path())
            .map(|m| m.len())
            .unwrap_or(0);

        // 2. Save graph index (current state)
        let graph_data = self.extract_graph_data(current)?;
        let graph_path = self.persistence_manager.graph_path().join("adjacency.idx");
        if self.config.enable_compression {
            crate::storage::index_persistence::graph::save_graph_index_compressed(
                &graph_data,
                &graph_path,
                self.config.compression_level,
            )
            .map_err(persistence_err)?;
        } else {
            crate::storage::index_persistence::graph::save_graph_index(&graph_data, &graph_path)
                .map_err(persistence_err)?;
        }
        bytes_written += std::fs::metadata(&graph_path).map(|m| m.len()).unwrap_or(0);

        // 3. Save temporal index (historical versions)
        let temporal_data = self.extract_temporal_data(historical)?;
        let temporal_path = self
            .persistence_manager
            .temporal_path()
            .join("versions.idx");
        crate::storage::index_persistence::temporal::save_temporal_index(
            &temporal_data,
            &temporal_path,
        )
        .map_err(persistence_err)?;
        bytes_written += std::fs::metadata(&temporal_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // 4. Build and save manifest
        let mut manifest = IndexManifest::new(lsn.0);

        // Add graph index entry
        manifest.graph_index = Some(GraphIndexManifestEntry {
            adjacency_file: "graph/adjacency.idx".to_string(),
            node_count: graph_data.node_count,
            edge_count: graph_data.edge_count,
        });

        // Add temporal index entry
        manifest.temporal_index = Some(TemporalIndexManifestEntry {
            node_versions_file: "temporal/versions.idx".to_string(),
            edge_versions_file: "temporal/versions.idx".to_string(),
            version_count: (temporal_data.node_versions.len() + temporal_data.edge_versions.len())
                as u64,
        });

        // Add string interner entry
        let string_count = GLOBAL_INTERNER.len() as u64;
        manifest.string_interner = Some(StringInternerManifestEntry {
            interner_file: "strings/interner.idx".to_string(),
            string_count,
        });

        self.persistence_manager
            .save_manifest(&manifest)
            .map_err(persistence_err)?;
        bytes_written += std::fs::metadata(self.persistence_manager.manifest_path())
            .map(|m| m.len())
            .unwrap_or(0);

        // Update tracking
        self.last_checkpoint_time = SystemTime::now();
        self.last_checkpoint_lsn = lsn;

        Ok(CheckpointStats {
            duration: start_time.elapsed(),
            bytes_written,
            lsn,
            node_count: graph_data.node_count as usize,
            edge_count: graph_data.edge_count as usize,
            version_count: temporal_data.node_versions.len() + temporal_data.edge_versions.len(),
        })
    }

    /// Recover database state from persisted indexes and WAL.
    ///
    /// Recovery process:
    /// 1. Check if persisted indexes exist
    /// 2. If yes: Load indexes from disk, then replay WAL from manifest LSN + 1
    /// 3. If no: Start with empty storage, replay entire WAL
    ///
    /// # Arguments
    ///
    /// * `wal` - WAL system for replaying entries after checkpoint
    ///
    /// # Returns
    ///
    /// Tuple of (CurrentStorage, HistoricalStorage, final_lsn)
    ///
    /// # Errors
    ///
    /// Returns an error if loading or WAL replay fails.
    pub fn recover(
        &mut self,
        wal: &ConcurrentWalSystem,
    ) -> Result<(CurrentStorage, HistoricalStorage, LSN)> {
        // Check if persisted indexes exist
        if !self.persistence_manager.indexes_exist() {
            // No persisted state - use legacy recovery (WAL replay from start)
            return self.recover_from_wal_only(wal);
        }

        // Load manifest and strings
        let manifest = self
            .persistence_manager
            .load_manifest_and_strings()
            .map_err(persistence_err)?;
        let checkpoint_lsn = LSN(manifest.lsn);

        // Load graph index
        let current = self.load_current_storage(&manifest)?;

        // Load temporal index
        let historical = self.load_historical_storage(&manifest)?;

        // Replay WAL entries after checkpoint LSN
        let start_lsn = checkpoint_lsn.next();
        let (current, historical, final_lsn) =
            self.replay_wal(wal, current, historical, start_lsn)?;

        // Update tracking
        self.last_checkpoint_lsn = checkpoint_lsn;

        Ok((current, historical, final_lsn))
    }

    /// Check if persisted indexes exist.
    pub fn has_persisted_state(&self) -> bool {
        self.persistence_manager.indexes_exist()
    }

    /// Get the LSN from the persisted manifest.
    ///
    /// Returns None if no manifest exists.
    pub fn get_persisted_lsn(&self) -> Option<LSN> {
        if !self.persistence_manager.indexes_exist() {
            return None;
        }

        self.persistence_manager
            .load_manifest_and_strings()
            .ok()
            .map(|m| LSN(m.lsn))
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    /// Extract graph data from CurrentStorage for persistence.
    fn extract_graph_data(&self, current: &CurrentStorage) -> Result<GraphIndexData> {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Extract all nodes
        for node in current.all_nodes() {
            let persisted = PersistedNode {
                id: node.id.as_u64(),
                label_idx: node.label.as_u32(),
                properties: persist_property_map(&node.properties).map_err(persistence_err)?,
            };
            nodes.push(persisted);
        }

        // Extract all edges
        for edge in current.all_edges() {
            let persisted = PersistedEdge {
                id: edge.id.as_u64(),
                source_id: edge.source.as_u64(),
                target_id: edge.target.as_u64(),
                label_idx: edge.label.as_u32(),
                properties: persist_property_map(&edge.properties).map_err(persistence_err)?,
            };
            edges.push(persisted);
        }

        Ok(GraphIndexData {
            magic: GRAPH_MAGIC,
            version: MANIFEST_VERSION,
            node_count: nodes.len() as u64,
            edge_count: edges.len() as u64,
            nodes,
            edges,
            // CSR adjacency will be rebuilt during loading
            outgoing_offsets: Vec::new(),
            outgoing_neighbors: Vec::new(),
            incoming_offsets: Vec::new(),
            incoming_neighbors: Vec::new(),
        })
    }

    /// Extract temporal data from HistoricalStorage for persistence.
    fn extract_temporal_data(&self, historical: &HistoricalStorage) -> Result<TemporalIndexData> {
        use crate::core::property::PropertyMapBuilder;
        use crate::storage::index_persistence::formats::{
            EdgeAnchorEntry, EdgeVersionEntry, NodeAnchorEntry, NodeVersionEntry,
            PersistedVersionType,
        };

        let mut node_versions = Vec::new();
        let mut node_anchors = Vec::new();
        let mut edge_versions = Vec::new();
        let mut edge_anchors = Vec::new();

        // Extract node versions
        for (version_id, version) in historical.get_node_versions() {
            let (version_type, properties, vector_snapshot_id) = match &version.data {
                VersionData::Anchor {
                    properties,
                    vector_snapshot_id,
                } => {
                    // Also add to anchors list
                    node_anchors.push(NodeAnchorEntry {
                        node_id: version.node_id.as_u64(),
                        anchor_tx_time: version.temporal.transaction_time().start().wallclock(),
                        full_state: persist_property_map(properties).map_err(persistence_err)?,
                        vector_snapshot_id: vector_snapshot_id.map(|id| id as u64),
                    });
                    (
                        PersistedVersionType::Anchor,
                        persist_property_map(properties).map_err(persistence_err)?,
                        vector_snapshot_id.map(|id| id as u64),
                    )
                }
                VersionData::Delta { delta } => {
                    // Convert delta to PropertyMap for persistence
                    let mut builder = PropertyMapBuilder::new();
                    for (key, value) in &delta.changed {
                        builder = builder.insert_by_key(*key, value.clone());
                    }
                    let changed_props = builder.build();
                    let removed_keys: Vec<u32> = delta.removed.iter().map(|k| k.as_u32()).collect();

                    (
                        PersistedVersionType::Delta {
                            base_anchor_tx: version.temporal.transaction_time().start().wallclock(),
                            removed_keys,
                        },
                        persist_property_map(&changed_props).map_err(persistence_err)?,
                        None,
                    )
                }
            };

            let valid_time = version.temporal.valid_time();
            let entry = NodeVersionEntry {
                version_id: version_id.as_u64(),
                node_id: version.node_id.as_u64(),
                valid_from: valid_time.start().wallclock(),
                valid_to: if valid_time.is_current() {
                    None
                } else {
                    Some(valid_time.end().wallclock())
                },
                tx_time: version.temporal.transaction_time().start().wallclock(),
                version_type,
                properties,
                vector_snapshot_id,
            };
            node_versions.push(entry);
        }

        // Extract edge versions
        for (version_id, version) in historical.get_edge_versions() {
            let (version_type, properties) = match &version.data {
                VersionData::Anchor { properties, .. } => {
                    // Also add to anchors list
                    edge_anchors.push(EdgeAnchorEntry {
                        edge_id: version.edge_id.as_u64(),
                        anchor_tx_time: version.temporal.transaction_time().start().wallclock(),
                        full_state: persist_property_map(properties).map_err(persistence_err)?,
                    });
                    (
                        PersistedVersionType::Anchor,
                        persist_property_map(properties).map_err(persistence_err)?,
                    )
                }
                VersionData::Delta { delta } => {
                    // Convert delta to PropertyMap for persistence
                    let mut builder = PropertyMapBuilder::new();
                    for (key, value) in &delta.changed {
                        builder = builder.insert_by_key(*key, value.clone());
                    }
                    let changed_props = builder.build();
                    let removed_keys: Vec<u32> = delta.removed.iter().map(|k| k.as_u32()).collect();

                    (
                        PersistedVersionType::Delta {
                            base_anchor_tx: version.temporal.transaction_time().start().wallclock(),
                            removed_keys,
                        },
                        persist_property_map(&changed_props).map_err(persistence_err)?,
                    )
                }
            };

            let valid_time = version.temporal.valid_time();
            let entry = EdgeVersionEntry {
                version_id: version_id.as_u64(),
                edge_id: version.edge_id.as_u64(),
                source_id: version.source.as_u64(),
                target_id: version.target.as_u64(),
                valid_from: valid_time.start().wallclock(),
                valid_to: if valid_time.is_current() {
                    None
                } else {
                    Some(valid_time.end().wallclock())
                },
                tx_time: version.temporal.transaction_time().start().wallclock(),
                version_type,
                properties,
            };
            edge_versions.push(entry);
        }

        Ok(TemporalIndexData {
            magic: TEMPORAL_MAGIC,
            version: MANIFEST_VERSION,
            node_versions,
            node_anchors,
            edge_versions,
            edge_anchors,
        })
    }

    /// Load CurrentStorage from persisted graph index.
    fn load_current_storage(&self, manifest: &IndexManifest) -> Result<CurrentStorage> {
        let current = CurrentStorage::new();

        if let Some(ref graph_entry) = manifest.graph_index {
            let graph_path = self
                .persistence_manager
                .indexes_path()
                .join(&graph_entry.adjacency_file);
            let graph_data =
                crate::storage::index_persistence::graph::load_graph_index(&graph_path)
                    .map_err(persistence_err)?;

            // Track next version ID for restored entities
            let mut next_version_id: u64 = 1;

            // Restore nodes
            for persisted_node in &graph_data.nodes {
                let node_id = NodeId::new(persisted_node.id)?;
                let label = InternedString::from_raw(persisted_node.label_idx);
                let properties =
                    restore_property_map(&persisted_node.properties).map_err(persistence_err)?;
                let version_id = VersionId::new(next_version_id)?;
                next_version_id += 1;

                let node = Node::new(node_id, label, properties, version_id);
                current.insert_node_direct(node, crate::core::temporal::time::now())?;
            }

            // Restore edges
            for persisted_edge in &graph_data.edges {
                let edge_id = EdgeId::new(persisted_edge.id)?;
                let source = NodeId::new(persisted_edge.source_id)?;
                let target = NodeId::new(persisted_edge.target_id)?;
                let label = InternedString::from_raw(persisted_edge.label_idx);
                let properties =
                    restore_property_map(&persisted_edge.properties).map_err(persistence_err)?;
                let version_id = VersionId::new(next_version_id)?;
                next_version_id += 1;

                let edge = Edge::new(edge_id, label, source, target, properties, version_id);
                current.insert_edge_direct(edge)?;
            }

            // Initialize version ID generator
            current.init_version_id_generator(next_version_id);

            // Initialize ID generators to continue from max IDs
            if let Some(max_node_id) = graph_data.nodes.iter().map(|n| n.id).max() {
                current.init_node_id_generator(max_node_id + 1);
            }
            if let Some(max_edge_id) = graph_data.edges.iter().map(|e| e.id).max() {
                current.init_edge_id_generator(max_edge_id + 1);
            }
        }

        Ok(current)
    }

    /// Load HistoricalStorage from persisted temporal index.
    fn load_historical_storage(&self, manifest: &IndexManifest) -> Result<HistoricalStorage> {
        use crate::storage::version::PropertyDelta;

        let mut historical = HistoricalStorage::new();

        if let Some(ref temporal_entry) = manifest.temporal_index {
            let temporal_path = self
                .persistence_manager
                .indexes_path()
                .join(&temporal_entry.node_versions_file);
            let temporal_data =
                crate::storage::index_persistence::temporal::load_temporal_index(&temporal_path)
                    .map_err(persistence_err)?;

            // Reserve capacity for efficient bulk insertion
            historical.reserve_restoration_capacity(
                temporal_data.node_versions.len(),
                temporal_data.edge_versions.len(),
            );

            // Track max version ID for generator initialization
            let mut _max_version_id: u64 = 0;

            // Restore node versions
            for entry in &temporal_data.node_versions {
                _max_version_id = _max_version_id.max(entry.version_id);

                let version_id = VersionId::new(entry.version_id)?;
                let node_id = NodeId::new(entry.node_id)?;

                use crate::core::hlc::HybridTimestamp;
                use crate::core::temporal::{TIMESTAMP_MAX, TimeRange};

                let valid_start = HybridTimestamp::new_unchecked(entry.valid_from, 0);
                let valid_end = entry
                    .valid_to
                    .map(|t| HybridTimestamp::new_unchecked(t, 0))
                    .unwrap_or(TIMESTAMP_MAX);
                let valid_time = TimeRange::new(valid_start, valid_end).map_err(|e| {
                    StorageError::CheckpointError {
                        reason: format!("Invalid valid time range: {}", e),
                    }
                })?;

                let tx_start = HybridTimestamp::new_unchecked(entry.tx_time, 0);
                let tx_time = TimeRange::from(tx_start);

                let temporal = crate::core::temporal::BiTemporalInterval::new(valid_time, tx_time);

                let properties =
                    restore_property_map(&entry.properties).map_err(persistence_err)?;
                let label = self.find_node_label_from_anchors(&temporal_data, entry.node_id)?;

                let data = match &entry.version_type {
                    crate::storage::index_persistence::formats::PersistedVersionType::Anchor => {
                        let vector_snapshot_id = entry.vector_snapshot_id.map(|id| id as usize);
                        VersionData::Anchor {
                            properties,
                            vector_snapshot_id,
                        }
                    }
                    crate::storage::index_persistence::formats::PersistedVersionType::Delta {
                        removed_keys,
                        ..
                    } => {
                        // Convert properties to PropertyDelta
                        let mut delta = PropertyDelta::new();
                        for (key, value) in properties.iter() {
                            delta.changed.insert(*key, value.clone());
                        }
                        for key_idx in removed_keys {
                            delta.removed.insert(InternedString::from_raw(*key_idx));
                        }
                        VersionData::Delta { delta }
                    }
                };

                let version = crate::storage::version::NodeVersion {
                    id: version_id,
                    node_id,
                    label,
                    temporal,
                    data,
                    next_version: None,
                    prev_version: None,
                };

                historical.insert_restored_node_version(version)?;
            }

            // Restore edge versions
            for entry in &temporal_data.edge_versions {
                _max_version_id = _max_version_id.max(entry.version_id);

                let version_id = VersionId::new(entry.version_id)?;
                let edge_id = EdgeId::new(entry.edge_id)?;
                let source = NodeId::new(entry.source_id)?;
                let target = NodeId::new(entry.target_id)?;

                use crate::core::hlc::HybridTimestamp;
                use crate::core::temporal::{TIMESTAMP_MAX, TimeRange};

                let valid_start = HybridTimestamp::new_unchecked(entry.valid_from, 0);
                let valid_end = entry
                    .valid_to
                    .map(|t| HybridTimestamp::new_unchecked(t, 0))
                    .unwrap_or(TIMESTAMP_MAX);
                let valid_time = TimeRange::new(valid_start, valid_end).map_err(|e| {
                    StorageError::CheckpointError {
                        reason: format!("Invalid valid time range: {}", e),
                    }
                })?;

                let tx_start = HybridTimestamp::new_unchecked(entry.tx_time, 0);
                let tx_time = TimeRange::from(tx_start);

                let temporal = crate::core::temporal::BiTemporalInterval::new(valid_time, tx_time);

                let properties =
                    restore_property_map(&entry.properties).map_err(persistence_err)?;
                let label = self.find_edge_label(&temporal_data, entry.edge_id)?;

                let data = match &entry.version_type {
                    crate::storage::index_persistence::formats::PersistedVersionType::Anchor => {
                        VersionData::Anchor {
                            properties,
                            vector_snapshot_id: None,
                        }
                    }
                    crate::storage::index_persistence::formats::PersistedVersionType::Delta {
                        removed_keys,
                        ..
                    } => {
                        // Convert properties to PropertyDelta
                        let mut delta = PropertyDelta::new();
                        for (key, value) in properties.iter() {
                            delta.changed.insert(*key, value.clone());
                        }
                        for key_idx in removed_keys {
                            delta.removed.insert(InternedString::from_raw(*key_idx));
                        }
                        VersionData::Delta { delta }
                    }
                };

                let version = crate::storage::version::EdgeVersion {
                    id: version_id,
                    edge_id,
                    source,
                    target,
                    label,
                    temporal,
                    data,
                    next_version: None,
                    prev_version: None,
                };

                historical.insert_restored_edge_version(version)?;
            }
        }

        Ok(historical)
    }

    /// Find node label from anchors (needed for version restoration).
    fn find_node_label_from_anchors(
        &self,
        _temporal_data: &TemporalIndexData,
        _node_id: u64,
    ) -> Result<InternedString> {
        // TODO: Store labels in temporal data or look up from graph data
        // For now, use a placeholder - this should be fixed in a follow-up
        Ok(GLOBAL_INTERNER
            .intern("unknown")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?)
    }

    /// Find edge label (needed for version restoration).
    fn find_edge_label(
        &self,
        _temporal_data: &TemporalIndexData,
        _edge_id: u64,
    ) -> Result<InternedString> {
        // TODO: Store labels in temporal data
        // For now, use a placeholder - this should be fixed in a follow-up
        Ok(GLOBAL_INTERNER
            .intern("unknown")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?)
    }

    /// Recover from WAL only (no persisted state).
    fn recover_from_wal_only(
        &mut self,
        wal: &ConcurrentWalSystem,
    ) -> Result<(CurrentStorage, HistoricalStorage, LSN)> {
        // Create fresh storage
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Replay entire WAL
        self.replay_wal(wal, current, historical, LSN::initial())
    }

    /// Replay WAL entries starting from a given LSN.
    fn replay_wal(
        &self,
        wal: &ConcurrentWalSystem,
        current: CurrentStorage,
        mut historical: HistoricalStorage,
        start_lsn: LSN,
    ) -> Result<(CurrentStorage, HistoricalStorage, LSN)> {
        use crate::api::transaction::types::TxId;
        use crate::storage::version::VersionMetadata;
        use crate::storage::wal::WalOperation;

        const RECOVERY_TX_ID: u64 = 0;

        let mut max_node_id: u64 = 0;
        let mut max_edge_id: u64 = 0;
        let mut max_version_id: u64 = 0;
        let mut next_version_id: u64 = 1;

        let wal_entries = wal.read_from(start_lsn)?;

        for entry in wal_entries {
            match entry.operation {
                WalOperation::CreateNode {
                    node_id,
                    label,
                    properties,
                    temporal,
                } => {
                    max_node_id = max_node_id.max(node_id.as_u64());

                    let interned_label =
                        GLOBAL_INTERNER
                            .intern(&label)
                            .map_err(|e| StorageError::WalError {
                                reason: format!("Failed to intern label: {}", e),
                            })?;

                    let commit_timestamp = temporal.transaction_time().start();
                    let metadata =
                        VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                    let version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    let node = Node::with_metadata(
                        node_id,
                        interned_label,
                        properties.clone(),
                        version_id,
                        metadata,
                    );

                    current.insert_node_direct(node, commit_timestamp)?;
                    historical.add_node_version(
                        node_id,
                        version_id,
                        temporal,
                        interned_label,
                        properties,
                    )?;

                    max_version_id = max_version_id.max(next_version_id - 1);
                }
                WalOperation::CreateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    temporal,
                } => {
                    max_edge_id = max_edge_id.max(edge_id.as_u64());

                    let interned_label =
                        GLOBAL_INTERNER
                            .intern(&label)
                            .map_err(|e| StorageError::WalError {
                                reason: format!("Failed to intern label: {}", e),
                            })?;

                    let commit_timestamp = temporal.transaction_time().start();
                    let metadata =
                        VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);
                    let version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    let edge = Edge::with_metadata(
                        edge_id,
                        interned_label,
                        source,
                        target,
                        properties.clone(),
                        version_id,
                        metadata,
                    );

                    current.insert_edge_direct(edge)?;
                    historical.add_edge_version(
                        edge_id,
                        version_id,
                        temporal,
                        interned_label,
                        source,
                        target,
                        properties,
                    )?;

                    max_version_id = max_version_id.max(next_version_id - 1);
                }
                WalOperation::UpdateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    temporal,
                } => {
                    max_version_id = max_version_id.max(version_id.as_u64());
                    next_version_id = next_version_id.max(version_id.as_u64() + 1);

                    let interned_label =
                        GLOBAL_INTERNER
                            .intern(&label)
                            .map_err(|e| StorageError::WalError {
                                reason: format!("Failed to intern label: {}", e),
                            })?;

                    let commit_timestamp = temporal.transaction_time().start();
                    let metadata =
                        VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

                    let node = Node::with_metadata(
                        node_id,
                        interned_label,
                        properties.clone(),
                        version_id,
                        metadata,
                    );

                    current.update_node_direct(node, commit_timestamp)?;

                    if let Some(prev_version_id) = historical.get_current_node_version(node_id) {
                        historical.close_node_version_transaction_time(
                            prev_version_id,
                            commit_timestamp,
                        )?;
                    }

                    historical.add_node_version(
                        node_id,
                        version_id,
                        temporal,
                        interned_label,
                        properties,
                    )?;
                }
                WalOperation::UpdateEdge {
                    edge_id,
                    version_id,
                    label,
                    properties,
                    temporal,
                } => {
                    max_version_id = max_version_id.max(version_id.as_u64());
                    next_version_id = next_version_id.max(version_id.as_u64() + 1);

                    let current_edge = current.get_edge(edge_id)?;

                    let interned_label =
                        GLOBAL_INTERNER
                            .intern(&label)
                            .map_err(|e| StorageError::WalError {
                                reason: format!("Failed to intern label: {}", e),
                            })?;

                    let commit_timestamp = temporal.transaction_time().start();
                    let metadata =
                        VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

                    let edge = Edge::with_metadata(
                        edge_id,
                        interned_label,
                        current_edge.source,
                        current_edge.target,
                        properties.clone(),
                        version_id,
                        metadata,
                    );

                    current.update_edge_direct(edge)?;

                    if let Some(prev_version_id) = historical.get_current_edge_version(edge_id) {
                        historical.close_edge_version_transaction_time(
                            prev_version_id,
                            commit_timestamp,
                        )?;
                    }

                    historical.add_edge_version(
                        edge_id,
                        version_id,
                        temporal,
                        interned_label,
                        current_edge.source,
                        current_edge.target,
                        properties,
                    )?;
                }
                WalOperation::DeleteNode { node_id, temporal } => {
                    let node = current.get_node(node_id)?;
                    let commit_timestamp = temporal.transaction_time().start();

                    if let Some(current_version_id) = historical.get_current_node_version(node_id) {
                        historical.close_node_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let tombstone_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    let tombstone_temporal = temporal.close_valid_time(commit_timestamp);

                    historical.add_node_version(
                        node_id,
                        tombstone_version_id,
                        tombstone_temporal,
                        node.label,
                        node.properties.clone(),
                    )?;

                    current.delete_node_direct(node_id, commit_timestamp)?;
                    max_version_id = max_version_id.max(next_version_id - 1);
                }
                WalOperation::DeleteEdge { edge_id, temporal } => {
                    let edge = current.get_edge(edge_id)?;
                    let commit_timestamp = temporal.transaction_time().start();

                    if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
                        historical.close_edge_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    let tombstone_version_id = VersionId::new(next_version_id)?;
                    next_version_id += 1;

                    let tombstone_temporal = temporal.close_valid_time(commit_timestamp);

                    historical.add_edge_version(
                        edge_id,
                        tombstone_version_id,
                        tombstone_temporal,
                        edge.label,
                        edge.source,
                        edge.target,
                        edge.properties.clone(),
                    )?;

                    current.delete_edge_direct(edge_id)?;
                    max_version_id = max_version_id.max(next_version_id - 1);
                }
                WalOperation::Checkpoint { .. } => {
                    // Checkpoint markers are informational only during replay
                }
            }
        }

        let final_lsn = wal.current_lsn();

        // Only update ID generators if we replayed entries with higher IDs
        // This preserves the values set during load_current_storage if no WAL replay happened
        if max_node_id > 0 {
            current.init_node_id_generator(max_node_id + 1);
        }
        if max_edge_id > 0 {
            current.init_edge_id_generator(max_edge_id + 1);
        }
        if max_version_id > 0 {
            current.init_version_id_generator(max_version_id + 1);
        }

        Ok((current, historical, final_lsn))
    }
}

/// Statistics from a checkpoint operation.
#[derive(Debug, Clone)]
pub struct CheckpointStats {
    /// Time taken for checkpoint creation
    pub duration: Duration,
    /// Total bytes written to disk
    pub bytes_written: u64,
    /// LSN at checkpoint time
    pub lsn: LSN,
    /// Number of nodes persisted
    pub node_count: usize,
    /// Number of edges persisted
    pub edge_count: usize,
    /// Number of versions persisted
    pub version_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::core::id::NodeId;
    use crate::core::temporal::{BiTemporalInterval, time};
    use crate::storage::wal::WalOperation;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::TempDir;

    // ========================================================================
    // TDD Tests: Checkpoint-Index Persistence Integration
    // ========================================================================

    /// Test basic checkpoint creation and stats.
    #[test]
    fn test_checkpoint_creation_basic() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig::with_data_dir(temp_dir.path());
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        let stats = manager.create_checkpoint(LSN(100), &current, &historical)?;

        assert_eq!(stats.lsn, LSN(100));
        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.edge_count, 0);
        assert!(stats.bytes_written > 0); // At least manifest should be written

        Ok(())
    }

    /// Test checkpoint persists nodes and edges.
    #[test]
    fn test_checkpoint_persists_graph_data() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig::with_data_dir(temp_dir.path());
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        // Create some nodes
        for i in 1..=10 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Node{}", i))
                .build();
            let node_id = NodeId::new(i)?;
            let label = GLOBAL_INTERNER
                .intern("Person")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;
            let version_id = VersionId::new(i)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;
        }

        let historical = HistoricalStorage::new();
        let stats = manager.create_checkpoint(LSN(50), &current, &historical)?;

        assert_eq!(stats.node_count, 10);

        // Verify files were created
        assert!(manager.persistence_manager.manifest_path().exists());
        assert!(manager.persistence_manager.interner_path().exists());
        assert!(
            manager
                .persistence_manager
                .graph_path()
                .join("adjacency.idx")
                .exists()
        );

        Ok(())
    }

    /// Test checkpoint recovery loads persisted state.
    #[test]
    fn test_checkpoint_recovery_loads_state() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Phase 1: Create checkpoint with data
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();

            // Create 5 nodes
            for i in 1..=5 {
                let props = PropertyMapBuilder::new()
                    .insert("name", format!("Node{}", i))
                    .build();
                let node_id = NodeId::new(i)?;
                let label =
                    GLOBAL_INTERNER
                        .intern("Document")
                        .map_err(|e| StorageError::WalError {
                            reason: e.to_string(),
                        })?;
                let version_id = VersionId::new(i)?;
                let node = Node::new(node_id, label, props, version_id);
                current.insert_node_direct(node, time::now())?;
            }

            let historical = HistoricalStorage::new();
            manager.create_checkpoint(LSN(25), &current, &historical)?;
        }

        // Phase 2: Recover from checkpoint
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            // Create empty WAL (no entries after checkpoint)
            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, _recovered_historical, lsn) = manager.recover(&wal)?;

            // Verify state was restored
            assert_eq!(recovered_current.node_count(), 5);
            assert_eq!(lsn, LSN::initial()); // Empty WAL

            // Verify node data
            for i in 1..=5 {
                let node = recovered_current.get_node(NodeId::new(i)?)?;
                let name = node.get_property("name").unwrap().as_str().unwrap();
                assert_eq!(name, format!("Node{}", i));
            }
        }

        Ok(())
    }

    /// Test recovery with WAL replay after checkpoint.
    ///
    /// This test verifies that:
    /// 1. Nodes from the checkpoint are restored
    /// 2. WAL entries after the checkpoint LSN are replayed
    #[test]
    fn test_checkpoint_recovery_with_wal_replay() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Create WAL first to have a proper LSN sequence
        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Phase 1: Add initial nodes to WAL (to establish LSN sequence)
        for i in 1..=3 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Initial{}", i))
                .build();
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: "Person".to_string(),
                properties: props,
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Create checkpoint at last written WAL LSN
        // current_lsn() returns the *next* LSN to be allocated, so subtract 1
        let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();

            // Create 3 nodes directly (matching what was logged to WAL)
            for i in 1..=3 {
                let props = PropertyMapBuilder::new()
                    .insert("name", format!("Initial{}", i))
                    .build();
                let node_id = NodeId::new(i)?;
                let label =
                    GLOBAL_INTERNER
                        .intern("Person")
                        .map_err(|e| StorageError::WalError {
                            reason: e.to_string(),
                        })?;
                let version_id = VersionId::new(i)?;
                let node = Node::new(node_id, label, props, version_id);
                current.insert_node_direct(node, time::now())?;
            }

            let historical = HistoricalStorage::new();
            manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
        }

        // Phase 2: Add more WAL entries after checkpoint
        for i in 4..=5 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("WalNode{}", i))
                .build();
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: "Person".to_string(),
                properties: props,
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Phase 3: Recover (should load checkpoint + replay WAL)
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Should have 3 from checkpoint + 2 from WAL = 5 total
        assert_eq!(recovered_current.node_count(), 5);

        // Verify checkpoint nodes
        for i in 1..=3 {
            let node = recovered_current.get_node(NodeId::new(i)?)?;
            let name = node.get_property("name").unwrap().as_str().unwrap();
            assert_eq!(name, format!("Initial{}", i));
        }

        // Verify WAL-replayed nodes
        for i in 4..=5 {
            let node = recovered_current.get_node(NodeId::new(i)?)?;
            let name = node.get_property("name").unwrap().as_str().unwrap();
            assert_eq!(name, format!("WalNode{}", i));
        }

        Ok(())
    }

    /// Test LSN consistency between checkpoint and manifest.
    #[test]
    fn test_lsn_consistency() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig::with_data_dir(temp_dir.path());
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Create checkpoint at LSN 42
        manager.create_checkpoint(LSN(42), &current, &historical)?;

        // Verify persisted LSN
        let persisted_lsn = manager.get_persisted_lsn();
        assert_eq!(persisted_lsn, Some(LSN(42)));

        Ok(())
    }

    /// Test should_checkpoint logic.
    #[test]
    fn test_should_checkpoint_logic() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            data_dir: temp_dir.path().to_path_buf(),
            checkpoint_interval: Duration::from_secs(3600), // 1 hour
            min_wal_entries: 100,
            ..Default::default()
        };
        let mut manager = CheckpointManager::new(config)?;

        // Should checkpoint initially (never checkpointed)
        assert!(manager.should_checkpoint(LSN(1)));

        // Simulate a checkpoint
        manager.last_checkpoint_time = SystemTime::now();
        manager.last_checkpoint_lsn = LSN(50);

        // Should NOT checkpoint (not enough time or entries)
        assert!(!manager.should_checkpoint(LSN(60)));

        // Should checkpoint when LSN threshold exceeded
        assert!(manager.should_checkpoint(LSN(200)));

        Ok(())
    }

    /// Test recovery without persisted state (fresh start).
    #[test]
    fn test_recovery_without_persisted_state() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        // Create WAL with some entries (no checkpoint)
        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        for i in 1..=3 {
            let props = PropertyMapBuilder::new().insert("value", i as i64).build();
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: "Test".to_string(),
                properties: props,
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Recover (should replay full WAL)
        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        assert_eq!(recovered_current.node_count(), 3);

        Ok(())
    }

    /// Test checkpoint with compression enabled.
    #[test]
    fn test_checkpoint_with_compression() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            data_dir: temp_dir.path().to_path_buf(),
            enable_compression: true,
            compression_level: 3,
            ..Default::default()
        };
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        // Create nodes with larger properties to benefit from compression
        for i in 1..=100 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Node{} with some longer text for compression", i))
                .insert("description", "This is a longer description that should compress well when repeated across many nodes")
                .build();
            let node_id = NodeId::new(i)?;
            let label = GLOBAL_INTERNER
                .intern("Document")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;
            let version_id = VersionId::new(i)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;
        }

        let historical = HistoricalStorage::new();
        let stats = manager.create_checkpoint(LSN(100), &current, &historical)?;

        assert_eq!(stats.node_count, 100);
        assert!(stats.bytes_written > 0);

        Ok(())
    }

    /// Test has_persisted_state check.
    #[test]
    fn test_has_persisted_state() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig::with_data_dir(temp_dir.path());
        let mut manager = CheckpointManager::new(config)?;

        // Initially no persisted state
        assert!(!manager.has_persisted_state());

        // Create checkpoint
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(1), &current, &historical)?;

        // Now has persisted state
        assert!(manager.has_persisted_state());

        Ok(())
    }

    /// Test checkpoint preserves node properties correctly.
    #[test]
    fn test_checkpoint_preserves_properties() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Phase 1: Create checkpoint with various property types
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();

            let props = PropertyMapBuilder::new()
                .insert("string_prop", "hello")
                .insert("int_prop", 42i64)
                .insert("float_prop", 3.15f64)
                .insert("bool_prop", true)
                .build();

            let node_id = NodeId::new(1)?;
            let label = GLOBAL_INTERNER
                .intern("TestNode")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;
            let version_id = VersionId::new(1)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;

            let historical = HistoricalStorage::new();
            manager.create_checkpoint(LSN(1), &current, &historical)?;
        }

        // Phase 2: Recover and verify properties
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

            let node = recovered_current.get_node(NodeId::new(1)?)?;

            assert_eq!(
                node.get_property("string_prop").unwrap().as_str().unwrap(),
                "hello"
            );
            assert_eq!(node.get_property("int_prop").unwrap().as_int().unwrap(), 42);
            assert!(
                (node.get_property("float_prop").unwrap().as_float().unwrap() - 3.15).abs() < 0.001
            );
            assert!(node.get_property("bool_prop").unwrap().as_bool().unwrap());
        }

        Ok(())
    }
}
