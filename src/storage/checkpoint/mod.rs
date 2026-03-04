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
//! use aletheiadb::storage::checkpoint::{CheckpointManager, CheckpointConfig};
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
use crate::core::error::{Result, StorageError};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::version::VersionData;
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
use crate::storage::redb_cold_storage::RedbColdStorage;
use crate::storage::wal::LSN;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;

/// Convert IndexPersistenceError to our Result type.
fn persistence_err(e: IndexPersistenceError) -> crate::core::error::Error {
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
        let (current_snapshot, historical_snapshot) = {
            // Synchronize with concurrent writes to ensure consistency
            let _lock = current.snapshot_lock.write();
            let c = current.create_snapshot(lsn);
            let h = historical.create_snapshot(lsn);
            (c, h)
        };

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
        cold_storage: Option<&Arc<RedbColdStorage>>,
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
            outgoing_node_ids: Vec::new(),
            outgoing_offsets: Vec::new(),
            outgoing_neighbors: Vec::new(),
            incoming_node_ids: Vec::new(),
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
                    let removed_keys: Vec<u32> = delta
                        .removed
                        .iter()
                        .map(|k: &crate::core::interning::InternedString| k.as_u32())
                        .collect();

                    (
                        PersistedVersionType::Delta {
                            base_anchor_tx: version.temporal.transaction_time().start().wallclock(),
                            base_anchor_tx_logical: version
                                .temporal
                                .transaction_time()
                                .start()
                                .logical(),
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
                valid_from_logical: valid_time.start().logical(),
                valid_to: if valid_time.is_current() {
                    None
                } else {
                    Some(valid_time.end().wallclock())
                },
                valid_to_logical: if valid_time.is_current() {
                    None
                } else {
                    Some(valid_time.end().logical())
                },
                tx_time: version.temporal.transaction_time().start().wallclock(),
                tx_time_logical: version.temporal.transaction_time().start().logical(),
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
                    let removed_keys: Vec<u32> = delta
                        .removed
                        .iter()
                        .map(|k: &crate::core::interning::InternedString| k.as_u32())
                        .collect();

                    (
                        PersistedVersionType::Delta {
                            base_anchor_tx: version.temporal.transaction_time().start().wallclock(),
                            base_anchor_tx_logical: version
                                .temporal
                                .transaction_time()
                                .start()
                                .logical(),
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
                valid_from_logical: valid_time.start().logical(),
                valid_to: if valid_time.is_current() {
                    None
                } else {
                    Some(valid_time.end().wallclock())
                },
                valid_to_logical: if valid_time.is_current() {
                    None
                } else {
                    Some(valid_time.end().logical())
                },
                tx_time: version.temporal.transaction_time().start().wallclock(),
                tx_time_logical: version.temporal.transaction_time().start().logical(),
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
        use crate::core::version::PropertyDelta;

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

                let version = crate::core::version::NodeVersion {
                    id: version_id,
                    node_id,
                    temporal,
                    label,
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

                let version = crate::core::version::EdgeVersion {
                    id: version_id,
                    edge_id,
                    source,
                    target,
                    temporal,
                    label,
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
        // Capture initial version ID before replay
        let initial_version_id = current.get_version_id_generator_current();

        let (final_lsn, max_node_id, max_edge_id, next_version_id) =
            crate::storage::recovery::replay_wal_into_storage(
                wal,
                &current,
                &mut historical,
                start_lsn,
                initial_version_id,
            )?;

        // Update ID generators to account for replayed entities
        if let Some(max_node_id) = max_node_id {
            current.init_node_id_generator(max_node_id + 1);
        }
        if let Some(max_edge_id) = max_edge_id {
            current.init_edge_id_generator(max_edge_id + 1);
        }
        // Ensure version ID generator is updated
        current.ensure_version_id_generator_at_least(next_version_id);

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
mod tests;
