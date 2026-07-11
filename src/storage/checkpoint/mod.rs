//! Checkpoint system with full state snapshot via index persistence.
//!
//! Zstd compression levels range from [`MIN_ZSTD_LEVEL`] (fastest) to
//! [`MAX_ZSTD_LEVEL`] (best ratio). Default is [`DEFAULT_ZSTD_LEVEL`].
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
    IndexPersistenceError, IndexPersistenceManager, MANIFEST_VERSION, TEMPORAL_MAGIC,
    formats::{
        GraphIndexData, GraphIndexManifestEntry, IndexManifest, StringInternerManifestEntry,
        TemporalIndexData, TemporalIndexManifestEntry,
    },
    graph::restore_property_map,
};
use crate::storage::redb_cold_storage::RedbColdStorage;
use crate::storage::wal::LSN;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;

/// Minimum valid zstd compression level.
pub const MIN_ZSTD_LEVEL: i32 = 1;
/// Maximum valid zstd compression level.
pub const MAX_ZSTD_LEVEL: i32 = 22;
/// Default zstd compression level (balances speed and ratio).
pub const DEFAULT_ZSTD_LEVEL: i32 = 3;

/// Convert IndexPersistenceError to our Result type.
fn persistence_err(e: IndexPersistenceError) -> crate::core::error::Error {
    StorageError::CheckpointError {
        reason: e.to_string(),
    }
    .into()
}

/// Reconstruct the full property state of the version PRECEDING `version`,
/// using only versions captured in the checkpoint snapshot (Issue #3387).
///
/// Used to materialize sparse vector deltas at checkpoint-extract time:
/// walks `prev_version` links back to the nearest anchor, then re-applies
/// the intervening deltas oldest-first. Fails (rather than persisting a
/// silently-lossy entry) if the chain leaves the snapshot (e.g. the base
/// was cold-migrated) or carries no anchor.
fn reconstruct_node_base_properties(
    by_id: &std::collections::HashMap<VersionId, Arc<crate::core::version::NodeVersion>>,
    version: &crate::core::version::NodeVersion,
) -> Result<crate::core::property::PropertyMap> {
    let chain_err = |reason: String| StorageError::CheckpointError { reason };

    let mut chain: Vec<&Arc<crate::core::version::NodeVersion>> = Vec::new();
    let mut cur = version.prev_version;
    while let Some(vid) = cur {
        if chain.len() >= crate::storage::historical::MAX_RECONSTRUCTION_DEPTH {
            return Err(chain_err(format!(
                "Version chain for node {} exceeds max reconstruction depth \
                 while materializing sparse vector deltas",
                version.node_id
            ))
            .into());
        }
        let v = by_id.get(&vid).ok_or_else(|| {
            chain_err(format!(
                "Cannot materialize sparse vector delta for node version {}: \
                 base version {} is not in the checkpoint snapshot (cold-migrated?)",
                version.id, vid
            ))
        })?;
        chain.push(v);
        if v.is_anchor() {
            break;
        }
        cur = v.prev_version;
    }

    let anchor_props = match chain.last().map(|v| &v.data) {
        Some(VersionData::Anchor { properties, .. }) => properties.clone(),
        _ => {
            return Err(chain_err(format!(
                "Cannot materialize sparse vector delta for node version {}: \
                 no anchor found in its version chain",
                version.id
            ))
            .into());
        }
    };

    // Re-apply the deltas between the anchor and `version` oldest-first.
    let mut props = anchor_props;
    for v in chain.iter().rev().skip(1) {
        if let VersionData::Delta { delta } = &v.data {
            props = delta.apply(&props);
        }
    }
    Ok(props)
}

/// Edge mirror of [`reconstruct_node_base_properties`].
fn reconstruct_edge_base_properties(
    by_id: &std::collections::HashMap<VersionId, Arc<crate::core::version::EdgeVersion>>,
    version: &crate::core::version::EdgeVersion,
) -> Result<crate::core::property::PropertyMap> {
    let chain_err = |reason: String| StorageError::CheckpointError { reason };

    let mut chain: Vec<&Arc<crate::core::version::EdgeVersion>> = Vec::new();
    let mut cur = version.prev_version;
    while let Some(vid) = cur {
        if chain.len() >= crate::storage::historical::MAX_RECONSTRUCTION_DEPTH {
            return Err(chain_err(format!(
                "Version chain for edge {} exceeds max reconstruction depth \
                 while materializing sparse vector deltas",
                version.edge_id
            ))
            .into());
        }
        let v = by_id.get(&vid).ok_or_else(|| {
            chain_err(format!(
                "Cannot materialize sparse vector delta for edge version {}: \
                 base version {} is not in the checkpoint snapshot (cold-migrated?)",
                version.id, vid
            ))
        })?;
        chain.push(v);
        if v.is_anchor() {
            break;
        }
        cur = v.prev_version;
    }

    let anchor_props = match chain.last().map(|v| &v.data) {
        Some(VersionData::Anchor { properties, .. }) => properties.clone(),
        _ => {
            return Err(chain_err(format!(
                "Cannot materialize sparse vector delta for edge version {}: \
                 no anchor found in its version chain",
                version.id
            ))
            .into());
        }
    };

    let mut props = anchor_props;
    for v in chain.iter().rev().skip(1) {
        if let VersionData::Delta { delta } = &v.data {
            props = delta.apply(&props);
        }
    }
    Ok(props)
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
            compression_level: DEFAULT_ZSTD_LEVEL,
        }
    }
}

impl CheckpointConfig {
    /// Create configuration with a specific data directory.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::checkpoint::CheckpointConfig;
    /// let config = CheckpointConfig::with_data_dir("my_data/mydb");
    /// assert_eq!(config.data_dir.to_str().unwrap(), "my_data/mydb");
    /// ```
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
    /// # The Spark
    /// Without checkpoints, recovering a database from a crash would require replaying
    /// the entire Write-Ahead Log (WAL) from the beginning of time. This manager
    /// coordinates the periodic flushing of in-memory indexes (like [`crate::index::vector::sharded::ShardedVectorIndex`])
    /// to disk. This ensures that the recovery process only needs to replay recent
    /// operations.
    ///
    /// # The Details
    /// Instantiating this manager validates the configuration and initializes the
    /// underlying [`crate::storage::index_persistence::IndexPersistenceManager`].
    /// It verifies that the `data_dir` is accessible and ensures the compression
    /// settings are valid for zstd.
    ///
    /// # Errors
    /// Returns a [`StorageError::CheckpointError`] if:
    /// - The data directory cannot be created.
    /// - `enable_compression` is true but `compression_level` is not in the valid zstd range (1-22).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::checkpoint::{CheckpointManager, CheckpointConfig};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = CheckpointConfig::default();
    /// let manager = CheckpointManager::new(config)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(config: CheckpointConfig) -> Result<Self> {
        // Validate compression level (zstd supports 1-22)
        if config.enable_compression
            && !(MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL).contains(&config.compression_level)
        {
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
    /// # The Spark
    /// Creating a checkpoint is an expensive I/O operation. If we checkpoint too often,
    /// we degrade system performance. If we checkpoint too rarely, recovery times
    /// become unacceptably long. This function evaluates heuristics to decide the
    /// optimal moment to pause and flush the state.
    ///
    /// # The Details
    /// This uses a hybrid threshold approach. It returns `true` if *either*:
    /// 1. The time elapsed since the last checkpoint exceeds the configured `checkpoint_interval`.
    /// 2. The number of new entries appended to the WAL (calculated via the `current_lsn`
    ///    minus the last checkpoint's [`LSN`]) exceeds `min_wal_entries`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::checkpoint::{CheckpointManager, CheckpointConfig};
    /// # use aletheiadb::storage::wal::LSN;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = CheckpointConfig::default();
    /// let manager = CheckpointManager::new(config)?;
    ///
    /// // Initially, should checkpoint because the manager hasn't recorded any checkpoints yet,
    /// // and the elapsed time effectively exceeds the interval.
    /// assert_eq!(manager.should_checkpoint(LSN(10)), true);
    /// # Ok(())
    /// # }
    /// ```
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
    /// # Locking contract (Issue #3425)
    ///
    /// `historical` is taken as a bare `&HistoricalStorage`, but a consistent
    /// (and data-race-free) snapshot requires the **caller to hold the
    /// `historical` `RwLock` read guard** for the whole duration of this call —
    /// exactly as [`crate::db::AletheiaDB::backup`] does. Holding that read guard
    /// is what makes the snapshot mutually exclusive with the commit path's
    /// `historical.write()` guard, which (as of #3425) is now held across
    /// `commit_timestamp` finalization. Without it, a snapshot can race the
    /// finalize step and capture a committed node/edge whose `commit_timestamp`
    /// is still `None`. Callers that already hold the guard by construction (e.g.
    /// via `historical.read()`) satisfy this contract; do not call this method
    /// with an unlocked `historical`.
    ///
    /// # Arguments
    ///
    /// * `lsn` - Current LSN for consistency tracking
    /// * `current` - Current storage to persist
    /// * `historical` - Historical storage to persist (caller must hold its read guard)
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

        // Replay WAL entries after checkpoint LSN.
        //
        // ⚠️ Transaction-framing invariant (Issue #3413): this EXCLUSIVE
        // `.next()` convention starts replay one LSN PAST `checkpoint_lsn`. If
        // `checkpoint_lsn` ever pointed at the last op of a committed
        // `[BeginTx .. CommitTx]` band, `.next()` would begin replay mid-band
        // (at the `CommitTx`, with its `BeginTx` skipped) — which
        // `resolve_transaction_frames` only tolerates as a benign no-op because
        // its buffered ops are also below the boundary. This whole `recover*`
        // path is currently test-only and unwired (production replays from an
        // INCLUSIVE `manifest.lsn`, always a band start; see `db::config` and
        // the resolver's load-bearing-invariant doc). Before wiring any of these
        // to production, ensure the start LSN lands on a transaction-frame
        // boundary, not mid-band.
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
    /// Delegates to the canonical free function in the index-persistence layer
    /// so the extraction logic lives in one place.
    fn extract_graph_data_from_snapshot(
        &self,
        snapshot: &crate::storage::snapshot::CurrentStorageSnapshot,
    ) -> Result<GraphIndexData> {
        crate::storage::index_persistence::graph::extract_graph_data_from_snapshot(snapshot)
            .map_err(persistence_err)
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
        use crate::storage::index_persistence::formats::{
            EdgeAnchorEntry, NodeAnchorEntry, PersistedVersionType,
        };
        use crate::storage::index_persistence::temporal::{
            convert_edge_version, convert_node_version, materialize_version_data_for_persistence,
            needs_sparse_vector_materialization,
        };

        let mut node_versions = Vec::with_capacity(snapshot.node_version_count());
        let mut node_anchors = Vec::with_capacity(snapshot.node_version_count());
        let mut edge_versions = Vec::with_capacity(snapshot.edge_version_count());
        let mut edge_anchors = Vec::with_capacity(snapshot.edge_version_count());

        // Sparse vector deltas cannot be persisted as-is (Issue #3387
        // availability fix): materialize them against the base state
        // reconstructed WITHIN the snapshot, in the persisted copy only.
        // The common case (no sparse deltas) pays only this detection scan.
        let node_by_id: std::collections::HashMap<
            VersionId,
            Arc<crate::core::version::NodeVersion>,
        > = if snapshot
            .iter_node_versions()
            .any(|v| needs_sparse_vector_materialization(&v.data))
        {
            snapshot.iter_node_versions().map(|v| (v.id, v)).collect()
        } else {
            std::collections::HashMap::new()
        };
        let edge_by_id: std::collections::HashMap<
            VersionId,
            Arc<crate::core::version::EdgeVersion>,
        > = if snapshot
            .iter_edge_versions()
            .any(|v| needs_sparse_vector_materialization(&v.data))
        {
            snapshot.iter_edge_versions().map(|v| (v.id, v)).collect()
        } else {
            std::collections::HashMap::new()
        };

        // Extract node versions from snapshot (isolated from concurrent
        // writes). Entry conversion is delegated to the canonical
        // `convert_node_version` so the checkpoint path persists exactly the
        // same shape as the index-persistence save path, including the
        // Issue #3387 tx-time closures and version chain links.
        for version_arc in snapshot.iter_node_versions() {
            let version = &*version_arc;
            let entry = if needs_sparse_vector_materialization(&version.data) {
                let base = reconstruct_node_base_properties(&node_by_id, version)?;
                let mut persisted = version.clone();
                persisted.data = materialize_version_data_for_persistence(&version.data, &base)
                    .map_err(persistence_err)?;
                convert_node_version(&persisted).map_err(persistence_err)?
            } else {
                convert_node_version(version).map_err(persistence_err)?
            };
            if matches!(entry.version_type, PersistedVersionType::Anchor) {
                node_anchors.push(NodeAnchorEntry {
                    node_id: entry.node_id,
                    anchor_tx_time: entry.tx_time,
                    full_state: entry.properties.clone(),
                    vector_snapshot_id: entry.vector_snapshot_id,
                });
            }
            node_versions.push(entry);
        }

        // Extract edge versions from snapshot (isolated from concurrent writes)
        for version_arc in snapshot.iter_edge_versions() {
            let version = &*version_arc;
            let entry = if needs_sparse_vector_materialization(&version.data) {
                let base = reconstruct_edge_base_properties(&edge_by_id, version)?;
                let mut persisted = version.clone();
                persisted.data = materialize_version_data_for_persistence(&version.data, &base)
                    .map_err(persistence_err)?;
                convert_edge_version(&persisted).map_err(persistence_err)?
            } else {
                convert_edge_version(version).map_err(persistence_err)?
            };
            if matches!(entry.version_type, PersistedVersionType::Anchor) {
                edge_anchors.push(EdgeAnchorEntry {
                    edge_id: entry.edge_id,
                    anchor_tx_time: entry.tx_time,
                    full_state: entry.properties.clone(),
                });
            }
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

            max_version_id = temporal_data
                .node_versions
                .iter()
                .map(|e| e.version_id)
                .chain(temporal_data.edge_versions.iter().map(|e| e.version_id))
                .max()
                .unwrap_or(0);

            // Restore via the canonical index-persistence path (Issue #3387):
            // it restores the persisted tx-time closures and version chain
            // links and finalizes version heads via `rebuild_version_chains`,
            // so a restore-only recovery (no WAL replay) serves the same full
            // bi-temporal reads as before the checkpoint.
            crate::storage::index_persistence::temporal::restore_into_historical_storage(
                &temporal_data,
                &mut historical,
            )
            .map_err(persistence_err)?;
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
