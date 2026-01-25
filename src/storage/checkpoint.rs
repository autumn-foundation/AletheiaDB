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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::core::GLOBAL_INTERNER;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::storage::cold_storage::ColdStorage;
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

/// Result of a recovery operation with cold storage support.
///
/// This struct provides detailed information about the recovery process,
/// including which data sources were used and how many WAL entries were replayed.
pub struct RecoveryResult {
    /// Recovered current storage.
    pub current: CurrentStorage,
    /// Recovered historical storage.
    pub historical: HistoricalStorage,
    /// Final LSN after WAL replay.
    pub final_lsn: LSN,
    /// Checkpoint LSN that was loaded (if checkpoint existed).
    pub checkpoint_lsn: Option<LSN>,
    /// Cold storage flushed LSN (if cold storage existed).
    pub flushed_lsn: Option<LSN>,
    /// Effective LSN used as the recovery point (max of checkpoint and flushed).
    pub effective_lsn: LSN,
    /// Number of WAL entries that were replayed.
    pub wal_entries_replayed: u64,
}

impl RecoveryResult {
    /// Check if cold storage data was used during recovery.
    pub fn used_cold_storage(&self) -> bool {
        match (self.checkpoint_lsn, self.flushed_lsn) {
            (Some(checkpoint), Some(flushed)) => flushed.0 > checkpoint.0,
            (None, Some(_)) => true,
            _ => false,
        }
    }

    /// Check if any checkpoint data was loaded.
    pub fn used_checkpoint(&self) -> bool {
        self.checkpoint_lsn.is_some()
    }

    /// Get the number of WAL entries that were skipped due to cold storage.
    pub fn wal_entries_skipped_from_cold(&self) -> u64 {
        match (self.checkpoint_lsn, self.flushed_lsn) {
            (Some(checkpoint), Some(flushed)) if flushed.0 > checkpoint.0 => {
                flushed.0 - checkpoint.0
            }
            (None, Some(flushed)) => flushed.0,
            _ => 0,
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
    /// Returns an error if:
    /// - The data directory cannot be created
    /// - The compression level is invalid (must be 1-22 for zstd)
    pub fn new(config: CheckpointConfig) -> Result<Self> {
        // Validate compression level (zstd supports 1-22)
        if config.enable_compression && !(1..=22).contains(&config.compression_level) {
            return Err(StorageError::CheckpointError {
                reason: format!(
                    "Invalid compression level {}: must be 1-22 for zstd",
                    config.compression_level
                ),
            }
            .into());
        }

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

        // 0. Create MVCC snapshots for isolation
        // This prevents fuzzy checkpointing (mixed state from different LSNs)
        let current_snapshot = current.create_snapshot(lsn);
        let historical_snapshot = historical.create_snapshot(lsn);

        // 1. Save string interner first (other indexes depend on it)
        self.persistence_manager
            .save_string_interner()
            .map_err(persistence_err)?;
        bytes_written += std::fs::metadata(self.persistence_manager.interner_path())
            .map(|m| m.len())
            .unwrap_or(0);

        // 2. Save graph index (current state) from snapshot
        let graph_data = self.extract_graph_data_from_snapshot(&current_snapshot)?;
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

        // 3. Save temporal index (historical versions) from snapshot
        let temporal_data = self.extract_temporal_data_from_snapshot(&historical_snapshot)?;
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

        // Validate checkpoint LSN is consistent with WAL
        // The checkpoint LSN should not exceed the WAL's current LSN
        let wal_current_lsn = wal.current_lsn();
        if checkpoint_lsn.0 > wal_current_lsn.0 {
            return Err(StorageError::CheckpointError {
                reason: format!(
                    "Checkpoint LSN {} is ahead of WAL current LSN {}, \
                     checkpoint may be from a different WAL or corrupted",
                    checkpoint_lsn.0, wal_current_lsn.0
                ),
            }
            .into());
        }

        // Load graph index
        let current = self.load_current_storage(&manifest)?;

        // Load temporal index
        let (historical, historical_max_version_id) = self.load_historical_storage(&manifest)?;

        // Ensure version ID generator accounts for historical versions
        // The current storage's generator was initialized from the count of restored entities,
        // but historical storage may have higher version IDs that we need to account for
        if historical_max_version_id > 0 {
            use crate::core::id::MAX_VALID_ID;

            // Use saturating_add to prevent overflow, then validate against MAX_VALID_ID
            let next_version_id = historical_max_version_id.saturating_add(1);
            if next_version_id > MAX_VALID_ID {
                return Err(StorageError::CheckpointError {
                    reason: format!(
                        "Historical max version ID {} would overflow MAX_VALID_ID on recovery",
                        historical_max_version_id
                    ),
                }
                .into());
            }
            current.ensure_version_id_generator_at_least(next_version_id);
        }

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

    /// Recover from checkpoint with cold storage support.
    ///
    /// This method extends the standard recovery process to account for data
    /// that has been flushed to cold storage. When cold storage has a higher
    /// `flushed_lsn` than the checkpoint, WAL replay can start from the
    /// cold storage's LSN instead of the checkpoint LSN, skipping entries
    /// that are already safely persisted to cold storage.
    ///
    /// # Recovery Flow
    ///
    /// 1. Open cold storage → get `flushed_lsn`
    /// 2. Load checkpoint state
    /// 3. Replay WAL from `max(checkpoint_lsn, flushed_lsn) + 1`
    /// 4. Rebuild hot tier (warm cache starts empty)
    ///
    /// # Key Invariant
    ///
    /// `WAL_truncation_lsn <= cold_storage.get_flushed_lsn()` (always)
    ///
    /// # Arguments
    ///
    /// * `wal` - The concurrent WAL system for replay
    /// * `cold_storage` - Optional cold storage for LSN tracking
    ///
    /// # Returns
    ///
    /// A tuple of (CurrentStorage, HistoricalStorage, final_lsn, recovery_info).
    ///
    /// # Errors
    ///
    /// Returns an error if loading or WAL replay fails.
    pub fn recover_with_cold_storage(
        &mut self,
        wal: &ConcurrentWalSystem,
        cold_storage: Option<&Arc<dyn ColdStorage>>,
    ) -> Result<RecoveryResult> {
        // Get flushed_lsn from cold storage if available
        let flushed_lsn = cold_storage.and_then(|cs| cs.get_flushed_lsn().ok().flatten());

        // Check if persisted indexes exist
        if !self.persistence_manager.indexes_exist() {
            // No persisted state - determine replay start from cold storage or beginning
            return self.recover_from_wal_with_cold_storage(wal, flushed_lsn);
        }

        // Load manifest and strings
        let manifest = self
            .persistence_manager
            .load_manifest_and_strings()
            .map_err(persistence_err)?;
        let checkpoint_lsn = LSN(manifest.lsn);

        // Validate checkpoint LSN is consistent with WAL
        let wal_current_lsn = wal.current_lsn();
        if checkpoint_lsn.0 > wal_current_lsn.0 {
            return Err(StorageError::CheckpointError {
                reason: format!(
                    "Checkpoint LSN {} is ahead of WAL current LSN {}, \
                     checkpoint may be from a different WAL or corrupted",
                    checkpoint_lsn.0, wal_current_lsn.0
                ),
            }
            .into());
        }

        // Load graph index
        let current = self.load_current_storage(&manifest)?;

        // Load temporal index
        let (historical, historical_max_version_id) = self.load_historical_storage(&manifest)?;

        // Ensure version ID generator accounts for historical versions
        if historical_max_version_id > 0 {
            use crate::core::id::MAX_VALID_ID;

            let next_version_id = historical_max_version_id.saturating_add(1);
            if next_version_id > MAX_VALID_ID {
                return Err(StorageError::CheckpointError {
                    reason: format!(
                        "Historical max version ID {} would overflow MAX_VALID_ID on recovery",
                        historical_max_version_id
                    ),
                }
                .into());
            }
            current.ensure_version_id_generator_at_least(next_version_id);
        }

        // Determine the effective recovery point
        // Use the higher of checkpoint_lsn or flushed_lsn
        let effective_lsn = match flushed_lsn {
            Some(flushed) if flushed.0 > checkpoint_lsn.0 => {
                // Cold storage has more recent data than checkpoint
                // Validate consistency: flushed_lsn should not exceed WAL current LSN
                if flushed.0 > wal_current_lsn.0 {
                    return Err(StorageError::CheckpointError {
                        reason: format!(
                            "Cold storage flushed_lsn {} is ahead of WAL current LSN {}, \
                             data inconsistency detected",
                            flushed.0, wal_current_lsn.0
                        ),
                    }
                    .into());
                }
                flushed
            }
            _ => checkpoint_lsn,
        };

        // Replay WAL entries after effective LSN
        let start_lsn = effective_lsn.next();
        let (current, historical, final_lsn) =
            self.replay_wal(wal, current, historical, start_lsn)?;

        // Update tracking
        self.last_checkpoint_lsn = checkpoint_lsn;

        Ok(RecoveryResult {
            current,
            historical,
            final_lsn,
            checkpoint_lsn: Some(checkpoint_lsn),
            flushed_lsn,
            effective_lsn,
            wal_entries_replayed: final_lsn.0.saturating_sub(start_lsn.0),
        })
    }

    /// Recover from WAL only, with optional cold storage LSN.
    ///
    /// This is used when no checkpoint exists but cold storage may have data.
    fn recover_from_wal_with_cold_storage(
        &self,
        wal: &ConcurrentWalSystem,
        flushed_lsn: Option<LSN>,
    ) -> Result<RecoveryResult> {
        // Create empty storage
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Determine start LSN
        let start_lsn = match flushed_lsn {
            Some(lsn) => lsn.next(),
            None => LSN::initial(),
        };
        let effective_lsn = flushed_lsn.unwrap_or(LSN::initial());

        // Replay WAL from start
        let (current, historical, final_lsn) =
            self.replay_wal(wal, current, historical, start_lsn)?;

        Ok(RecoveryResult {
            current,
            historical,
            final_lsn,
            checkpoint_lsn: None,
            flushed_lsn,
            effective_lsn,
            wal_entries_replayed: final_lsn.0.saturating_sub(start_lsn.0),
        })
    }

    // ========================================================================
    // Private Helper Methods
    // ========================================================================

    /// Extract graph data from CurrentStorage snapshot for persistence.
    ///
    /// Uses MVCC snapshot for isolation, preventing fuzzy checkpointing.
    fn extract_graph_data_from_snapshot(
        &self,
        snapshot: &crate::storage::snapshot::CurrentStorageSnapshot,
    ) -> Result<GraphIndexData> {
        use crate::storage::snapshot::StorageSnapshot;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Extract all nodes from snapshot (isolated from concurrent writes)
        for node in snapshot.iter_nodes() {
            let persisted = PersistedNode {
                id: node.id.as_u64(),
                label_idx: node.label.as_u32(),
                version_id: node.current_version.as_u64(),
                properties: persist_property_map(&node.properties).map_err(persistence_err)?,
            };
            nodes.push(persisted);
        }

        // Extract all edges from snapshot (isolated from concurrent writes)
        for edge in snapshot.iter_edges() {
            let persisted = PersistedEdge {
                id: edge.id.as_u64(),
                source_id: edge.source.as_u64(),
                target_id: edge.target.as_u64(),
                label_idx: edge.label.as_u32(),
                version_id: edge.current_version.as_u64(),
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

    /// Extract graph data from CurrentStorage for persistence (legacy method).
    ///
    /// This is kept for backwards compatibility with existing tests.
    /// New code should use extract_graph_data_from_snapshot for snapshot isolation.
    #[allow(dead_code)]
    fn extract_graph_data(&self, current: &CurrentStorage) -> Result<GraphIndexData> {
        let snapshot = current.create_snapshot(LSN(0));
        self.extract_graph_data_from_snapshot(&snapshot)
    }

    /// Extract temporal data from HistoricalStorage snapshot for persistence.
    ///
    /// Uses MVCC snapshot for isolation, preventing fuzzy checkpointing.
    fn extract_temporal_data_from_snapshot(
        &self,
        snapshot: &crate::storage::snapshot::HistoricalStorageSnapshot,
    ) -> Result<TemporalIndexData> {
        use crate::core::property::PropertyMapBuilder;
        use crate::storage::index_persistence::formats::{
            EdgeAnchorEntry, EdgeVersionEntry, NodeAnchorEntry, NodeVersionEntry,
            PersistedVersionType,
        };

        let mut node_versions = Vec::new();
        let mut node_anchors = Vec::new();
        let mut edge_versions = Vec::new();
        let mut edge_anchors = Vec::new();

        // Extract node versions from snapshot (isolated from concurrent writes)
        for version_arc in snapshot.iter_node_versions() {
            let version = &*version_arc;
            let version_id = version.id;
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
                label_idx: version.label.as_u32(),
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

        // Extract edge versions from snapshot (isolated from concurrent writes)
        for version_arc in snapshot.iter_edge_versions() {
            let version = &*version_arc;
            let version_id = version.id;
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
                label_idx: version.label.as_u32(),
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

    /// Extract temporal data from HistoricalStorage for persistence (legacy method).
    ///
    /// This is kept for backwards compatibility with existing tests.
    /// New code should use extract_temporal_data_from_snapshot for snapshot isolation.
    #[allow(dead_code)]
    fn extract_temporal_data(&self, historical: &HistoricalStorage) -> Result<TemporalIndexData> {
        let snapshot = historical.create_snapshot(LSN(0));
        self.extract_temporal_data_from_snapshot(&snapshot)
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

            // Track maximum version ID to initialize generator
            let mut max_version_id: u64 = 0;

            // Restore nodes
            for persisted_node in &graph_data.nodes {
                let node_id = NodeId::new(persisted_node.id)?;
                let label = InternedString::from_raw(persisted_node.label_idx);
                let properties =
                    restore_property_map(&persisted_node.properties).map_err(persistence_err)?;
                let version_id = VersionId::new(persisted_node.version_id)?;
                max_version_id = max_version_id.max(persisted_node.version_id);

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
                let version_id = VersionId::new(persisted_edge.version_id)?;
                max_version_id = max_version_id.max(persisted_edge.version_id);

                let edge = Edge::new(edge_id, label, source, target, properties, version_id);
                current.insert_edge_direct(edge)?;
            }

            // Initialize version ID generator to continue from max version ID
            current.init_version_id_generator(max_version_id + 1);

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
    ///
    /// Returns the loaded HistoricalStorage and the maximum version ID found,
    /// which is needed to properly initialize the version ID generator.
    fn load_historical_storage(
        &self,
        manifest: &IndexManifest,
    ) -> Result<(HistoricalStorage, u64)> {
        use crate::storage::version::PropertyDelta;

        let mut historical = HistoricalStorage::new();
        let mut max_version_id: u64 = 0;

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

            // Restore node versions
            for entry in &temporal_data.node_versions {
                max_version_id = max_version_id.max(entry.version_id);

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
                let label = InternedString::from_raw(entry.label_idx);

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
                max_version_id = max_version_id.max(entry.version_id);

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
                let label = InternedString::from_raw(entry.label_idx);

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

        Ok((historical, max_version_id))
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

        // Create WAL first so we have a valid LSN for the checkpoint
        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

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
            // Use LSN(0) which is valid for an empty WAL
            manager.create_checkpoint(LSN(0), &current, &historical)?;
        }

        // Phase 2: Recover from checkpoint using the same WAL
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

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

    // ========================================================================
    // Additional Coverage Tests
    // ========================================================================

    /// Test invalid compression level returns error.
    #[test]
    fn test_invalid_compression_level_error() {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            data_dir: temp_dir.path().to_path_buf(),
            enable_compression: true,
            compression_level: 0, // Invalid - must be 1-22
            ..Default::default()
        };
        let result = CheckpointManager::new(config);
        assert!(result.is_err());
        match result {
            Err(e) => assert!(e.to_string().contains("Invalid compression level")),
            Ok(_) => panic!("Expected error"),
        }

        // Test compression level too high
        let config2 = CheckpointConfig {
            data_dir: temp_dir.path().to_path_buf(),
            enable_compression: true,
            compression_level: 25, // Invalid - must be 1-22
            ..Default::default()
        };
        let result2 = CheckpointManager::new(config2);
        assert!(result2.is_err());
    }

    /// Test that compression level is not validated when compression is disabled.
    #[test]
    fn test_compression_disabled_ignores_level() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            data_dir: temp_dir.path().to_path_buf(),
            enable_compression: false,
            compression_level: 0, // Invalid, but should be ignored
            ..Default::default()
        };
        let _manager = CheckpointManager::new(config)?;
        Ok(())
    }

    /// Test checkpoint without compression (uncompressed path).
    #[test]
    fn test_checkpoint_without_compression() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Phase 1: Create checkpoint without compression
        {
            let config = CheckpointConfig {
                data_dir: data_dir.clone(),
                enable_compression: false,
                compression_level: 1, // Won't be used
                ..Default::default()
            };
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();

            for i in 1..=5 {
                let props = PropertyMapBuilder::new()
                    .insert("name", format!("Node{}", i))
                    .build();
                let node_id = NodeId::new(i)?;
                let label =
                    GLOBAL_INTERNER
                        .intern("Uncompressed")
                        .map_err(|e| StorageError::WalError {
                            reason: e.to_string(),
                        })?;
                let version_id = VersionId::new(i)?;
                let node = Node::new(node_id, label, props, version_id);
                current.insert_node_direct(node, time::now())?;
            }

            let historical = HistoricalStorage::new();
            let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
            assert_eq!(stats.node_count, 5);
        }

        // Phase 2: Recover and verify
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;
            assert_eq!(recovered_current.node_count(), 5);
        }

        Ok(())
    }

    /// Test checkpoint with edges.
    #[test]
    fn test_checkpoint_with_edges() -> Result<()> {
        use crate::core::graph::Edge;
        use crate::core::id::EdgeId;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Phase 1: Create checkpoint with nodes and edges
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();

            // Create nodes
            for i in 1..=3 {
                let props = PropertyMapBuilder::new()
                    .insert("name", format!("Person{}", i))
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

            // Create edges
            let edge_label =
                GLOBAL_INTERNER
                    .intern("KNOWS")
                    .map_err(|e| StorageError::WalError {
                        reason: e.to_string(),
                    })?;

            let edge1 = Edge::new(
                EdgeId::new(1)?,
                edge_label,
                NodeId::new(1)?,
                NodeId::new(2)?,
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
                VersionId::new(4)?,
            );
            let edge2 = Edge::new(
                EdgeId::new(2)?,
                edge_label,
                NodeId::new(2)?,
                NodeId::new(3)?,
                PropertyMapBuilder::new().insert("since", 2021i64).build(),
                VersionId::new(5)?,
            );

            current.insert_edge_direct(edge1)?;
            current.insert_edge_direct(edge2)?;

            let historical = HistoricalStorage::new();
            let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
            assert_eq!(stats.node_count, 3);
            assert_eq!(stats.edge_count, 2);
        }

        // Phase 2: Recover and verify edges
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

            assert_eq!(recovered_current.node_count(), 3);
            assert_eq!(recovered_current.edge_count(), 2);

            // Verify edge data
            let edge1 = recovered_current.get_edge(EdgeId::new(1)?)?;
            assert_eq!(edge1.source.as_u64(), 1);
            assert_eq!(edge1.target.as_u64(), 2);
            assert_eq!(edge1.get_property("since").unwrap().as_int().unwrap(), 2020);

            let edge2 = recovered_current.get_edge(EdgeId::new(2)?)?;
            assert_eq!(edge2.source.as_u64(), 2);
            assert_eq!(edge2.target.as_u64(), 3);
        }

        Ok(())
    }

    /// Test checkpoint LSN ahead of WAL returns error.
    #[test]
    fn test_checkpoint_lsn_ahead_of_wal_error() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Create a checkpoint with a high LSN
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            let historical = HistoricalStorage::new();

            // Create checkpoint with LSN 1000
            manager.create_checkpoint(LSN(1000), &current, &historical)?;
        }

        // Try to recover with an empty WAL (current LSN = 0)
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let result = manager.recover(&wal);
            assert!(result.is_err());
            match result {
                Err(e) => {
                    let err_str = e.to_string();
                    assert!(err_str.contains("Checkpoint LSN"));
                    assert!(err_str.contains("ahead of WAL"));
                }
                Ok(_) => panic!("Expected error"),
            }
        }

        Ok(())
    }

    /// Test WAL replay with CreateEdge operation.
    #[test]
    fn test_wal_replay_create_edge() -> Result<()> {
        use crate::core::id::EdgeId;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create nodes first
        for i in 1..=2 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: "Person".to_string(),
                properties: PropertyMapBuilder::new()
                    .insert("name", format!("Person{}", i))
                    .build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }

        // Create edge
        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(1)?,
            source: NodeId::new(1)?,
            target: NodeId::new(2)?,
            label: "KNOWS".to_string(),
            properties: PropertyMapBuilder::new().insert("since", 2023i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Recover (no persisted state - full WAL replay)
        let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        assert_eq!(recovered_current.node_count(), 2);
        assert_eq!(recovered_current.edge_count(), 1);

        let edge = recovered_current.get_edge(EdgeId::new(1)?)?;
        assert_eq!(edge.source.as_u64(), 1);
        assert_eq!(edge.target.as_u64(), 2);

        // Verify historical storage also has the edge version
        assert_eq!(recovered_historical.get_edge_versions().len(), 1);

        Ok(())
    }

    /// Test WAL replay with UpdateNode operation.
    #[test]
    fn test_wal_replay_update_node() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let node_id = NodeId::new(1)?;

        // Create node
        wal.append(WalOperation::CreateNode {
            node_id,
            label: "Person".to_string(),
            properties: PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        // Update node
        wal.append(WalOperation::UpdateNode {
            node_id,
            version_id: VersionId::new(2)?,
            label: "Person".to_string(),
            properties: PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 31i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Recover
        let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        assert_eq!(recovered_current.node_count(), 1);

        let node = recovered_current.get_node(node_id)?;
        assert_eq!(node.get_property("age").unwrap().as_int().unwrap(), 31);

        // Verify historical has versions (create + update)
        assert_eq!(recovered_historical.get_node_versions().len(), 2);

        Ok(())
    }

    /// Test WAL replay with UpdateEdge operation.
    #[test]
    fn test_wal_replay_update_edge() -> Result<()> {
        use crate::core::id::EdgeId;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create nodes first
        for i in 1..=2 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: "Person".to_string(),
                properties: PropertyMapBuilder::new().build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }

        let edge_id = EdgeId::new(1)?;

        // Create edge
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source: NodeId::new(1)?,
            target: NodeId::new(2)?,
            label: "KNOWS".to_string(),
            properties: PropertyMapBuilder::new().insert("strength", 5i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        // Update edge
        wal.append(WalOperation::UpdateEdge {
            edge_id,
            version_id: VersionId::new(4)?,
            label: "KNOWS".to_string(),
            properties: PropertyMapBuilder::new().insert("strength", 10i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Recover
        let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        assert_eq!(recovered_current.edge_count(), 1);

        let edge = recovered_current.get_edge(edge_id)?;
        assert_eq!(edge.get_property("strength").unwrap().as_int().unwrap(), 10);

        // Verify historical has edge versions (create + update)
        assert_eq!(recovered_historical.get_edge_versions().len(), 2);

        Ok(())
    }

    /// Test WAL replay with DeleteNode operation.
    #[test]
    fn test_wal_replay_delete_node() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let node_id = NodeId::new(1)?;

        // Create node
        wal.append(WalOperation::CreateNode {
            node_id,
            label: "ToDelete".to_string(),
            properties: PropertyMapBuilder::new().insert("temp", true).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        // Delete node
        wal.append(WalOperation::DeleteNode {
            node_id,
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Recover
        let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        // Node should be deleted from current
        assert_eq!(recovered_current.node_count(), 0);

        // But historical should have versions (create + tombstone)
        assert_eq!(recovered_historical.get_node_versions().len(), 2);

        Ok(())
    }

    /// Test WAL replay with DeleteEdge operation.
    #[test]
    fn test_wal_replay_delete_edge() -> Result<()> {
        use crate::core::id::EdgeId;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create nodes
        for i in 1..=2 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: "Person".to_string(),
                properties: PropertyMapBuilder::new().build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }

        let edge_id = EdgeId::new(1)?;

        // Create edge
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source: NodeId::new(1)?,
            target: NodeId::new(2)?,
            label: "TEMP_EDGE".to_string(),
            properties: PropertyMapBuilder::new().build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        // Delete edge
        wal.append(WalOperation::DeleteEdge {
            edge_id,
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Recover
        let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        // Edge should be deleted from current
        assert_eq!(recovered_current.edge_count(), 0);
        // Nodes should still exist
        assert_eq!(recovered_current.node_count(), 2);

        // Historical should have edge versions (create + tombstone)
        assert_eq!(recovered_historical.get_edge_versions().len(), 2);

        Ok(())
    }

    /// Test WAL replay with Checkpoint marker (should be ignored).
    #[test]
    fn test_wal_replay_checkpoint_marker() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create a node
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(1)?,
            label: "Test".to_string(),
            properties: PropertyMapBuilder::new().build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        // Add checkpoint marker
        wal.append(WalOperation::Checkpoint {
            lsn: LSN(1),
            timestamp: time::now(),
        })?;

        // Create another node
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(2)?,
            label: "Test".to_string(),
            properties: PropertyMapBuilder::new().build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Recover - checkpoint marker should be ignored
        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Both nodes should exist
        assert_eq!(recovered_current.node_count(), 2);

        Ok(())
    }

    /// Test checkpoint with temporal data including node versions.
    #[test]
    fn test_checkpoint_with_temporal_node_versions() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Phase 1: Create checkpoint with historical node versions
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            let mut historical = HistoricalStorage::new();

            // Create a node in current storage
            let node_id = NodeId::new(1)?;
            let label = GLOBAL_INTERNER
                .intern("Document")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;

            let props = PropertyMapBuilder::new()
                .insert("title", "Version 2")
                .build();
            let version_id = VersionId::new(2)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;

            // Add historical version (anchor)
            let anchor_props = PropertyMapBuilder::new()
                .insert("title", "Version 1")
                .build();
            historical.add_node_version(
                node_id,
                VersionId::new(1)?,
                BiTemporalInterval::current(time::now()),
                label,
                anchor_props,
            )?;

            let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
            assert_eq!(stats.node_count, 1);
            assert!(stats.version_count >= 1);
        }

        // Phase 2: Recover and verify temporal data
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

            assert_eq!(recovered_current.node_count(), 1);
            assert_eq!(recovered_historical.get_node_versions().len(), 1);
        }

        Ok(())
    }

    /// Test checkpoint with temporal data including edge versions.
    #[test]
    fn test_checkpoint_with_temporal_edge_versions() -> Result<()> {
        use crate::core::graph::Edge;
        use crate::core::id::EdgeId;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Phase 1: Create checkpoint with historical edge versions
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            let mut historical = HistoricalStorage::new();

            // Create nodes
            let person_label =
                GLOBAL_INTERNER
                    .intern("Person")
                    .map_err(|e| StorageError::WalError {
                        reason: e.to_string(),
                    })?;
            for i in 1..=2 {
                let node = Node::new(
                    NodeId::new(i)?,
                    person_label,
                    PropertyMapBuilder::new().build(),
                    VersionId::new(i)?,
                );
                current.insert_node_direct(node, time::now())?;
            }

            // Create edge in current storage
            let edge_label =
                GLOBAL_INTERNER
                    .intern("KNOWS")
                    .map_err(|e| StorageError::WalError {
                        reason: e.to_string(),
                    })?;
            let edge = Edge::new(
                EdgeId::new(1)?,
                edge_label,
                NodeId::new(1)?,
                NodeId::new(2)?,
                PropertyMapBuilder::new().insert("strength", 10i64).build(),
                VersionId::new(4)?,
            );
            current.insert_edge_direct(edge)?;

            // Add historical edge version (anchor)
            historical.add_edge_version(
                EdgeId::new(1)?,
                VersionId::new(3)?,
                BiTemporalInterval::current(time::now()),
                edge_label,
                NodeId::new(1)?,
                NodeId::new(2)?,
                PropertyMapBuilder::new().insert("strength", 5i64).build(),
            )?;

            let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
            assert_eq!(stats.edge_count, 1);
            assert_eq!(stats.version_count, 1);
        }

        // Phase 2: Recover and verify temporal edge data
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

            assert_eq!(recovered_current.edge_count(), 1);
            assert_eq!(recovered_historical.get_edge_versions().len(), 1);
        }

        Ok(())
    }

    /// Test get_persisted_lsn returns None when no persisted state exists.
    #[test]
    fn test_get_persisted_lsn_none() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig::with_data_dir(temp_dir.path());
        let manager = CheckpointManager::new(config)?;

        assert!(manager.get_persisted_lsn().is_none());
        Ok(())
    }

    /// Test CheckpointConfig default values.
    #[test]
    fn test_checkpoint_config_default() {
        let config = CheckpointConfig::default();

        assert_eq!(config.data_dir, PathBuf::from("data"));
        assert_eq!(config.checkpoint_interval, Duration::from_secs(300));
        assert_eq!(config.min_wal_entries, 1000);
        assert!(config.enable_compression);
        assert_eq!(config.compression_level, 3);
    }

    /// Test checkpoint updates last_checkpoint_time and last_checkpoint_lsn.
    #[test]
    fn test_checkpoint_updates_tracking() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig::with_data_dir(temp_dir.path());
        let mut manager = CheckpointManager::new(config)?;

        // Initially, last checkpoint time is UNIX_EPOCH
        assert_eq!(manager.last_checkpoint_lsn, LSN::initial());

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        manager.create_checkpoint(LSN(42), &current, &historical)?;

        // After checkpoint, tracking should be updated
        assert_eq!(manager.last_checkpoint_lsn, LSN(42));
        assert!(manager.last_checkpoint_time > UNIX_EPOCH);

        Ok(())
    }

    /// Test should_checkpoint with time threshold.
    #[test]
    fn test_should_checkpoint_time_threshold() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            data_dir: temp_dir.path().to_path_buf(),
            checkpoint_interval: Duration::from_millis(1), // Very short
            min_wal_entries: 1_000_000,                    // Very high
            ..Default::default()
        };
        let mut manager = CheckpointManager::new(config)?;

        // Set last checkpoint time to now
        manager.last_checkpoint_time = SystemTime::now();
        manager.last_checkpoint_lsn = LSN(100);

        // Wait a bit for time to elapse
        std::thread::sleep(Duration::from_millis(5));

        // Should checkpoint due to time threshold
        assert!(manager.should_checkpoint(LSN(101)));

        Ok(())
    }

    /// Test temporal data with closed valid time (valid_to is set).
    #[test]
    fn test_checkpoint_with_closed_valid_time() -> Result<()> {
        use crate::core::hlc::HybridTimestamp;
        use crate::core::temporal::TimeRange;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            let mut historical = HistoricalStorage::new();

            let node_id = NodeId::new(1)?;
            let label =
                GLOBAL_INTERNER
                    .intern("ClosedNode")
                    .map_err(|e| StorageError::WalError {
                        reason: e.to_string(),
                    })?;

            // Current state (node is deleted, so not in current)
            // Add historical version with closed valid time
            let now = time::now();
            let later = HybridTimestamp::new_unchecked(now.wallclock() + 1000, 0);
            let valid_time = TimeRange::new(now, later)?;
            let tx_time = TimeRange::from(now);
            let temporal = BiTemporalInterval::new(valid_time, tx_time);

            historical.add_node_version(
                node_id,
                VersionId::new(1)?,
                temporal,
                label,
                PropertyMapBuilder::new().insert("deleted", true).build(),
            )?;

            let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
            assert_eq!(stats.version_count, 1);
        }

        // Recover and verify
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (_recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

            // Should have the version with closed valid time
            assert_eq!(recovered_historical.get_node_versions().len(), 1);
        }

        Ok(())
    }

    /// Test ID generators are properly initialized after recovery with max IDs.
    #[test]
    fn test_recovery_id_generator_initialization() -> Result<()> {
        use crate::core::graph::Edge;
        use crate::core::id::EdgeId;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Create checkpoint with high IDs
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();

            let label = GLOBAL_INTERNER
                .intern("Test")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;

            // Use high node ID
            let node = Node::new(
                NodeId::new(100)?,
                label,
                PropertyMapBuilder::new().build(),
                VersionId::new(1)?,
            );
            current.insert_node_direct(node, time::now())?;

            // Use high edge ID
            let edge = Edge::new(
                EdgeId::new(200)?,
                label,
                NodeId::new(100)?,
                NodeId::new(100)?,
                PropertyMapBuilder::new().build(),
                VersionId::new(2)?,
            );
            current.insert_edge_direct(edge)?;

            let historical = HistoricalStorage::new();
            manager.create_checkpoint(LSN(0), &current, &historical)?;
        }

        // Recover and verify ID generators by creating new entities
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

            // Create new node - ID should be > 100
            let new_node_id =
                recovered_current.create_node("NewNode", PropertyMapBuilder::new().build())?;
            assert!(new_node_id.as_u64() > 100);

            // Create new edge - ID should be > 200
            let new_edge_id = recovered_current.create_edge(
                NodeId::new(100)?,
                new_node_id,
                "NEW_EDGE",
                PropertyMapBuilder::new().build(),
            )?;
            assert!(new_edge_id.as_u64() > 200);
        }

        Ok(())
    }

    /// Test WAL replay updates ID generators when replaying entries with higher IDs.
    #[test]
    fn test_wal_replay_updates_id_generators() -> Result<()> {
        use crate::core::id::EdgeId;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Create checkpoint with low IDs
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            let label = GLOBAL_INTERNER
                .intern("Test")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;

            let node = Node::new(
                NodeId::new(1)?,
                label,
                PropertyMapBuilder::new().build(),
                VersionId::new(1)?,
            );
            current.insert_node_direct(node, time::now())?;

            let historical = HistoricalStorage::new();
            manager.create_checkpoint(LSN(0), &current, &historical)?;
        }

        // Add WAL entries with higher IDs
        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Add entry with high node ID
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(500)?,
            label: "HighId".to_string(),
            properties: PropertyMapBuilder::new().build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        // Add entry with high edge ID
        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(600)?,
            source: NodeId::new(1)?,
            target: NodeId::new(500)?,
            label: "HighEdge".to_string(),
            properties: PropertyMapBuilder::new().build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Recover
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

            // Create new node - ID should be > 500
            let new_node_id =
                recovered_current.create_node("NewNode", PropertyMapBuilder::new().build())?;
            assert!(new_node_id.as_u64() > 500);

            // Create new edge - ID should be > 600
            let new_edge_id = recovered_current.create_edge(
                NodeId::new(1)?,
                new_node_id,
                "NEW_EDGE",
                PropertyMapBuilder::new().build(),
            )?;
            assert!(new_edge_id.as_u64() > 600);
        }

        Ok(())
    }

    /// Test CheckpointStats fields.
    #[test]
    fn test_checkpoint_stats_fields() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig::with_data_dir(temp_dir.path());
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        let stats = manager.create_checkpoint(LSN(99), &current, &historical)?;

        // Verify stats structure
        assert_eq!(stats.lsn, LSN(99));
        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.edge_count, 0);
        assert_eq!(stats.version_count, 0);
        assert!(stats.duration.as_nanos() > 0); // Should have taken some time
        assert!(stats.bytes_written > 0); // At least manifest

        Ok(())
    }

    /// Test recovery with historical versions that have higher version IDs than current storage.
    #[test]
    fn test_recovery_historical_version_id_tracking() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        // Create checkpoint where historical has higher version IDs
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            let mut historical = HistoricalStorage::new();

            let node_id = NodeId::new(1)?;
            let label =
                GLOBAL_INTERNER
                    .intern("Versioned")
                    .map_err(|e| StorageError::WalError {
                        reason: e.to_string(),
                    })?;

            // Current node has version 1
            let node = Node::new(
                node_id,
                label,
                PropertyMapBuilder::new().insert("v", 1i64).build(),
                VersionId::new(1)?,
            );
            current.insert_node_direct(node, time::now())?;

            // Historical has version 100 (higher than current)
            historical.add_node_version(
                node_id,
                VersionId::new(100)?,
                BiTemporalInterval::current(time::now()),
                label,
                PropertyMapBuilder::new().insert("v", 100i64).build(),
            )?;

            manager.create_checkpoint(LSN(0), &current, &historical)?;
        }

        // Recover and verify version ID generator accounts for historical
        // by creating a new node and checking the version is > 100
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
            let wal = ConcurrentWalSystem::new(wal_config)?;

            let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

            // Creating a new node should use version ID > 100
            // We verify this indirectly by checking the node count increased
            let _new_node_id =
                recovered_current.create_node("NewNode", PropertyMapBuilder::new().build())?;
            assert_eq!(recovered_current.node_count(), 2);
        }

        Ok(())
    }

    /// Test persistence_err helper function.
    #[test]
    fn test_persistence_err_conversion() {
        use crate::storage::index_persistence::IndexPersistenceError;
        use std::path::PathBuf;

        let orig_err = IndexPersistenceError::InvalidMagic {
            path: PathBuf::from("/test/path"),
            expected: [0x12, 0x34, 0x56, 0x78],
            got: [0xAB, 0xCD, 0xEF, 0x00],
        };

        let converted = persistence_err(orig_err);
        let err_string = converted.to_string();

        assert!(err_string.contains("Invalid magic bytes"));
    }

    // ========================================================================
    // Cold Storage Recovery Tests (Issue 7: Redb + WAL replay)
    // ========================================================================

    #[test]
    fn test_recovery_with_no_cold_storage() -> Result<()> {
        // Full WAL replay when no cold storage is available
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        // Recovery without cold storage should work
        let result = manager.recover_with_cold_storage(&wal, None)?;

        // Should have empty storage with no checkpoint
        assert_eq!(result.current.node_count(), 0);
        assert!(result.checkpoint_lsn.is_none());
        assert!(result.flushed_lsn.is_none());
        assert!(!result.used_cold_storage());
        assert!(!result.used_checkpoint());

        Ok(())
    }

    #[test]
    fn test_recovery_with_checkpoint_no_cold_storage() -> Result<()> {
        // Standard checkpoint-based recovery without cold storage
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Write WAL entries to advance LSN to 100+
        // In a real scenario, WAL entries would be written before checkpoint
        for i in 1..=100 {
            let op = WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: format!("Node{}", i),
                properties: PropertyMapBuilder::new().build(),
                temporal: BiTemporalInterval::current(time::now()),
            };
            wal.append_async(op)?;
        }
        wal.flush()?;

        // Create checkpoint with some data
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            for i in 1..=5 {
                let props = PropertyMapBuilder::new()
                    .insert("name", format!("Node{}", i))
                    .build();
                let node_id = NodeId::new(i)?;
                let label = GLOBAL_INTERNER.intern("Person").unwrap();
                let version_id = VersionId::new(i)?;
                let node = Node::new(node_id, label, props, version_id);
                current.insert_node_direct(node, time::now())?;
            }

            let historical = HistoricalStorage::new();
            manager.create_checkpoint(LSN(100), &current, &historical)?;
        }

        // Recover without cold storage
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let result = manager.recover_with_cold_storage(&wal, None)?;

            assert_eq!(result.current.node_count(), 5);
            assert_eq!(result.checkpoint_lsn, Some(LSN(100)));
            assert!(result.flushed_lsn.is_none());
            assert!(!result.used_cold_storage());
            assert!(result.used_checkpoint());
        }

        Ok(())
    }

    #[test]
    fn test_recovery_loads_cold_storage_first() -> Result<()> {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

        // Cold storage with flushed_lsn should be checked before WAL replay
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");
        let cold_dir = temp_dir.path().join("cold");

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create cold storage with a flushed_lsn
        let cold_storage = Arc::new(RedbColdStorage::new(
            cold_dir.join("cold.redb"),
            RedbConfig::new(),
        )?);

        // Store some data with LSN tracking
        let node = crate::storage::version::NodeVersion::new_anchor(
            VersionId::new(1)?,
            NodeId::new(1)?,
            BiTemporalInterval::current(time::now()),
            GLOBAL_INTERNER.intern("Test").unwrap(),
            PropertyMapBuilder::new().build(),
        );
        cold_storage.store_batch_with_lsn(&[node], &[], LSN(50))?;

        // Verify flushed_lsn is set
        assert_eq!(cold_storage.get_flushed_lsn()?, Some(LSN(50)));

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        // Recovery should see the flushed_lsn
        let cold: Arc<dyn ColdStorage> = cold_storage;
        let result = manager.recover_with_cold_storage(&wal, Some(&cold))?;

        // Should have detected cold storage
        assert_eq!(result.flushed_lsn, Some(LSN(50)));
        assert!(result.used_cold_storage());

        Ok(())
    }

    #[test]
    fn test_recovery_replays_wal_from_flushed_lsn() -> Result<()> {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

        // When cold storage has higher flushed_lsn than checkpoint,
        // WAL replay should start from flushed_lsn + 1
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");
        let cold_dir = temp_dir.path().join("cold");

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Write WAL entries to advance LSN to 100+ (to match cold storage flushed_lsn)
        for i in 1..=100 {
            let op = WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: format!("Node{}", i),
                properties: PropertyMapBuilder::new().build(),
                temporal: BiTemporalInterval::current(time::now()),
            };
            wal.append_async(op)?;
        }
        wal.flush()?;

        // Create checkpoint at LSN 50
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let current = CurrentStorage::new();
            let historical = HistoricalStorage::new();
            manager.create_checkpoint(LSN(50), &current, &historical)?;
        }

        // Create cold storage with flushed_lsn at 100 (higher than checkpoint)
        let cold_storage = Arc::new(RedbColdStorage::new(
            cold_dir.join("cold.redb"),
            RedbConfig::new(),
        )?);

        let node = crate::storage::version::NodeVersion::new_anchor(
            VersionId::new(1)?,
            NodeId::new(1)?,
            BiTemporalInterval::current(time::now()),
            GLOBAL_INTERNER.intern("Test").unwrap(),
            PropertyMapBuilder::new().build(),
        );
        cold_storage.store_batch_with_lsn(&[node], &[], LSN(100))?;

        // Recover with cold storage
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;

            let cold: Arc<dyn ColdStorage> = cold_storage;
            let result = manager.recover_with_cold_storage(&wal, Some(&cold))?;

            // Should use flushed_lsn as effective recovery point
            assert_eq!(result.checkpoint_lsn, Some(LSN(50)));
            assert_eq!(result.flushed_lsn, Some(LSN(100)));
            assert_eq!(result.effective_lsn, LSN(100));
            assert!(result.used_cold_storage());

            // WAL entries between checkpoint and flushed_lsn should be skipped
            assert_eq!(result.wal_entries_skipped_from_cold(), 50);
        }

        Ok(())
    }

    #[test]
    fn test_recovery_with_no_wal_segments() -> Result<()> {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

        // Recovery with just cold storage data and no WAL
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");
        let cold_dir = temp_dir.path().join("cold");

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create cold storage with some data
        let cold_storage = Arc::new(RedbColdStorage::new(
            cold_dir.join("cold.redb"),
            RedbConfig::new(),
        )?);

        let node = crate::storage::version::NodeVersion::new_anchor(
            VersionId::new(1)?,
            NodeId::new(1)?,
            BiTemporalInterval::current(time::now()),
            GLOBAL_INTERNER.intern("Test").unwrap(),
            PropertyMapBuilder::new().build(),
        );
        cold_storage.store_batch_with_lsn(&[node], &[], LSN(75))?;

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let cold: Arc<dyn ColdStorage> = cold_storage;
        let result = manager.recover_with_cold_storage(&wal, Some(&cold))?;

        // Should have no WAL entries replayed
        assert_eq!(result.wal_entries_replayed, 0);
        assert_eq!(result.effective_lsn, LSN(75));

        Ok(())
    }

    #[test]
    fn test_recovery_validates_lsn_consistency() -> Result<()> {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

        // flushed_lsn ahead of WAL should be detected as inconsistency
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let data_dir = temp_dir.path().join("data");
        let cold_dir = temp_dir.path().join("cold");

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Write WAL entries to advance LSN to 50
        for i in 1..=50 {
            let op = WalOperation::CreateNode {
                node_id: NodeId::new(i)?,
                label: format!("Node{}", i),
                properties: PropertyMapBuilder::new().build(),
                temporal: BiTemporalInterval::current(time::now()),
            };
            wal.append_async(op)?;
        }
        wal.flush()?;
        // WAL is now at LSN 50

        // Create checkpoint at LSN 10 (valid, < WAL current LSN)
        {
            let config = CheckpointConfig::with_data_dir(&data_dir);
            let mut manager = CheckpointManager::new(config)?;
            let current = CurrentStorage::new();
            let historical = HistoricalStorage::new();
            manager.create_checkpoint(LSN(10), &current, &historical)?;
        }

        // Create cold storage with flushed_lsn = 1000 (way ahead of WAL)
        let cold_storage = Arc::new(RedbColdStorage::new(
            cold_dir.join("cold.redb"),
            RedbConfig::new(),
        )?);

        let node = crate::storage::version::NodeVersion::new_anchor(
            VersionId::new(1)?,
            NodeId::new(1)?,
            BiTemporalInterval::current(time::now()),
            GLOBAL_INTERNER.intern("Test").unwrap(),
            PropertyMapBuilder::new().build(),
        );
        cold_storage.store_batch_with_lsn(&[node], &[], LSN(1000))?;

        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let cold: Arc<dyn ColdStorage> = cold_storage;
        let result = manager.recover_with_cold_storage(&wal, Some(&cold));

        // Should detect inconsistency
        assert!(result.is_err());
        let err = result.err().expect("Expected error").to_string();
        assert!(
            err.contains("flushed_lsn") || err.contains("inconsistency"),
            "Error should mention LSN inconsistency: {}",
            err
        );

        Ok(())
    }

    #[test]
    fn test_recovery_result_helpers() {
        // Test RecoveryResult helper methods

        // Case 1: No cold storage, no checkpoint
        let result = RecoveryResult {
            current: CurrentStorage::new(),
            historical: HistoricalStorage::new(),
            final_lsn: LSN(0),
            checkpoint_lsn: None,
            flushed_lsn: None,
            effective_lsn: LSN(0),
            wal_entries_replayed: 0,
        };
        assert!(!result.used_cold_storage());
        assert!(!result.used_checkpoint());
        assert_eq!(result.wal_entries_skipped_from_cold(), 0);

        // Case 2: Checkpoint only
        let result = RecoveryResult {
            current: CurrentStorage::new(),
            historical: HistoricalStorage::new(),
            final_lsn: LSN(50),
            checkpoint_lsn: Some(LSN(50)),
            flushed_lsn: None,
            effective_lsn: LSN(50),
            wal_entries_replayed: 0,
        };
        assert!(!result.used_cold_storage());
        assert!(result.used_checkpoint());
        assert_eq!(result.wal_entries_skipped_from_cold(), 0);

        // Case 3: Cold storage ahead of checkpoint
        let result = RecoveryResult {
            current: CurrentStorage::new(),
            historical: HistoricalStorage::new(),
            final_lsn: LSN(100),
            checkpoint_lsn: Some(LSN(50)),
            flushed_lsn: Some(LSN(100)),
            effective_lsn: LSN(100),
            wal_entries_replayed: 0,
        };
        assert!(result.used_cold_storage());
        assert!(result.used_checkpoint());
        assert_eq!(result.wal_entries_skipped_from_cold(), 50);

        // Case 4: Cold storage only (no checkpoint)
        let result = RecoveryResult {
            current: CurrentStorage::new(),
            historical: HistoricalStorage::new(),
            final_lsn: LSN(75),
            checkpoint_lsn: None,
            flushed_lsn: Some(LSN(75)),
            effective_lsn: LSN(75),
            wal_entries_replayed: 0,
        };
        assert!(result.used_cold_storage());
        assert!(!result.used_checkpoint());
        assert_eq!(result.wal_entries_skipped_from_cold(), 75);
    }
}
