//! Persistence and checkpoint management for durable storage.
//!
//! This module provides:
//! - Checkpoint creation for fast recovery
//! - Memory-mapped file storage for efficient access
//! - Database recovery from checkpoints and WAL
//!
//! # Checkpoint Strategy
//!
//! Checkpoints are periodic snapshots of the entire database state that:
//! - Enable fast recovery without replaying the entire WAL
//! - Are created based on time or WAL size thresholds
//! - Store complete current state + metadata for historical storage

use crate::api::transaction::types::TxId;
use crate::core::{
    GLOBAL_INTERNER,
    graph::{Edge, Node},
    id::{EdgeId, NodeId, VersionId},
    interning::InternedString,
    property::PropertyMap,
    temporal::{BiTemporalInterval, Timestamp, time},
};
use crate::index::vector::HnswConfig;
use crate::storage::{
    current::CurrentStorage,
    historical::HistoricalStorage,
    version::VersionMetadata,
    wal::{LSN, concurrent_system::ConcurrentWalSystem},
};
use crate::utils::error::{Result, StorageError};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Transaction ID used for recovered operations during WAL replay.
///
/// During recovery, we replay operations from the WAL but don't have access to the
/// original transaction IDs. Using TxId(0) marks these as "recovered" operations.
///
/// **Limitation**: Original transaction provenance is not preserved across recovery.
/// This means temporal queries cannot distinguish which operations were part of the
/// same transaction after a crash/restart.
///
/// **Future Enhancement**: To preserve transaction IDs, the WAL format would need to
/// be extended to store TxId with each operation (see Issue #86).
const RECOVERY_TX_ID: u64 = 0;

/// Configuration for checkpoint behavior
#[derive(Debug, Clone)]
pub struct CheckpointConfig {
    /// Directory where checkpoints are stored
    pub checkpoint_dir: PathBuf,
    /// Minimum time between checkpoints
    pub checkpoint_interval: Duration,
    /// Minimum WAL entries before checkpoint
    pub min_wal_entries: u64,
    /// Maximum number of checkpoints to retain
    pub checkpoints_to_retain: usize,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        CheckpointConfig {
            checkpoint_dir: PathBuf::from("gallifreydb/checkpoints"),
            checkpoint_interval: Duration::from_secs(300), // 5 minutes
            min_wal_entries: 1000,
            checkpoints_to_retain: 5,
        }
    }
}

/// Vector index configuration data for checkpoint persistence
#[derive(Debug, Clone)]
pub struct VectorIndexCheckpointData {
    /// Whether vector indexing is enabled
    pub enabled: bool,
    /// Property name that is indexed (empty string if not enabled)
    pub property_name: String,
    /// HNSW index configuration
    pub config: HnswConfig,
}

impl VectorIndexCheckpointData {
    /// Create checkpoint data for disabled vector index
    pub fn disabled() -> Self {
        VectorIndexCheckpointData {
            enabled: false,
            property_name: String::new(),
            config: HnswConfig::default(),
        }
    }

    /// Create checkpoint data for enabled vector index
    pub fn enabled(property_name: String, config: HnswConfig) -> Self {
        VectorIndexCheckpointData {
            enabled: true,
            property_name,
            config,
        }
    }

    /// Serialize to writer in binary format
    ///
    /// Format:
    /// - enabled: 1 byte (0 or 1)
    /// - property_name_len: 4 bytes (u32 LE)
    /// - property_name: variable bytes (UTF-8)
    /// - config: 41 bytes (HnswConfig serialization)
    pub fn serialize_into<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(&[if self.enabled { 1 } else { 0 }])?;

        let name_bytes = self.property_name.as_bytes();
        writer.write_all(&(name_bytes.len() as u32).to_le_bytes())?;
        writer.write_all(name_bytes)?;

        self.config.serialize_into(writer)?;

        Ok(())
    }

    /// Deserialize from reader
    pub fn deserialize_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut buf_u8 = [0u8; 1];
        let mut buf_u32 = [0u8; 4];

        // Read enabled flag
        reader.read_exact(&mut buf_u8)?;
        let enabled = buf_u8[0] != 0;

        // Read property name length
        reader.read_exact(&mut buf_u32)?;
        let name_len = u32::from_le_bytes(buf_u32) as usize;

        // Read property name
        let mut name_bytes = vec![0u8; name_len];
        reader.read_exact(&mut name_bytes)?;
        let property_name = String::from_utf8(name_bytes).map_err(|e| {
            StorageError::CorruptedData(format!("Invalid UTF-8 in property name: {}", e))
        })?;

        // Read config
        let config = HnswConfig::deserialize_from(reader)?;

        Ok(VectorIndexCheckpointData {
            enabled,
            property_name,
            config,
        })
    }
}

/// Metadata about a checkpoint
#[derive(Debug, Clone)]
pub struct CheckpointMetadata {
    /// LSN at which this checkpoint was taken
    pub lsn: LSN,
    /// Timestamp when checkpoint was created
    pub timestamp: Timestamp,
    /// Number of nodes in the checkpoint
    pub node_count: usize,
    /// Number of edges in the checkpoint
    pub edge_count: usize,
    /// Number of historical versions
    pub version_count: usize,
    /// Vector index configuration (None for V1 checkpoints)
    pub vector_index_config: Option<VectorIndexCheckpointData>,
}

/// A database checkpoint containing full state
///
/// Note: Checkpoints store only metadata in the current implementation.
/// In a production system, this would serialize the full database state.
#[derive(Debug)]
pub struct Checkpoint {
    /// Checkpoint metadata
    pub metadata: CheckpointMetadata,
}

impl Checkpoint {
    /// Create a new checkpoint from current state
    pub fn new(lsn: LSN, current: &CurrentStorage, historical: &HistoricalStorage) -> Self {
        let stats = current.stats();
        let hist_stats = historical.stats();
        let vector_config = current.get_vector_index_config();

        let metadata = CheckpointMetadata {
            lsn,
            timestamp: time::now(),
            node_count: stats.node_count,
            edge_count: stats.edge_count,
            version_count: hist_stats.total_node_versions + hist_stats.total_edge_versions,
            vector_index_config: Some(vector_config),
        };

        // In production, would serialize current and historical state
        Checkpoint { metadata }
    }

    /// Save checkpoint to disk
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent_dir = path.parent().ok_or_else(|| {
            StorageError::IoError(format!(
                "Invalid checkpoint path: no parent directory for {:?}",
                path
            ))
        })?;

        std::fs::create_dir_all(parent_dir).map_err(|e| {
            StorageError::IoError(format!("Failed to create checkpoint directory: {}", e))
        })?;

        let file = File::create(path).map_err(|e| {
            StorageError::IoError(format!("Failed to create checkpoint file: {}", e))
        })?;

        let mut writer = BufWriter::new(file);

        // Write checkpoint metadata (simplified format)
        self.write_metadata(&mut writer)?;

        // In production, would serialize the full current and historical storage
        // For now, just write basic metadata
        writer
            .flush()
            .map_err(|e| StorageError::IoError(format!("Failed to flush checkpoint: {}", e)))?;

        Ok(())
    }

    /// Write checkpoint metadata
    fn write_metadata<W: Write>(&self, writer: &mut W) -> Result<()> {
        // Write magic bytes for verification
        writer.write_all(b"GFRY").map_err(|e| {
            StorageError::IoError(format!("Failed to write checkpoint magic: {}", e))
        })?;

        // Write version 2 (adds vector index config)
        writer.write_all(&2u32.to_le_bytes()).map_err(|e| {
            StorageError::IoError(format!("Failed to write checkpoint version: {}", e))
        })?;

        // Write metadata
        writer
            .write_all(&self.metadata.lsn.0.to_le_bytes())
            .map_err(|e| StorageError::IoError(format!("Failed to write LSN: {}", e)))?;

        // Phase 2: Serialize HybridTimestamp (12 bytes)
        let mut ts_buffer = Vec::with_capacity(12);
        self.metadata.timestamp.serialize_into(&mut ts_buffer);
        writer
            .write_all(&ts_buffer)
            .map_err(|e| StorageError::IoError(format!("Failed to write timestamp: {}", e)))?;

        writer
            .write_all(&(self.metadata.node_count as u64).to_le_bytes())
            .map_err(|e| StorageError::IoError(format!("Failed to write node count: {}", e)))?;

        writer
            .write_all(&(self.metadata.edge_count as u64).to_le_bytes())
            .map_err(|e| StorageError::IoError(format!("Failed to write edge count: {}", e)))?;

        writer
            .write_all(&(self.metadata.version_count as u64).to_le_bytes())
            .map_err(|e| StorageError::IoError(format!("Failed to write version count: {}", e)))?;

        // Write vector index config (V2 format)
        if let Some(ref vector_config) = self.metadata.vector_index_config {
            vector_config.serialize_into(writer).map_err(|e| {
                StorageError::IoError(format!("Failed to write vector index config: {}", e))
            })?;
        } else {
            // Write disabled config if None (for backward compatibility)
            VectorIndexCheckpointData::disabled()
                .serialize_into(writer)
                .map_err(|e| {
                    StorageError::IoError(format!("Failed to write vector index config: {}", e))
                })?;
        }

        Ok(())
    }

    /// Load checkpoint from disk
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .map_err(|e| StorageError::IoError(format!("Failed to open checkpoint file: {}", e)))?;

        let mut reader = BufReader::new(file);

        // Read and verify magic bytes
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic).map_err(|e| {
            StorageError::IoError(format!("Failed to read checkpoint magic: {}", e))
        })?;

        if &magic != b"GFRY" {
            return Err(
                StorageError::CorruptedData("Invalid checkpoint magic bytes".to_string()).into(),
            );
        }

        // Read version
        let mut version_bytes = [0u8; 4];
        reader.read_exact(&mut version_bytes).map_err(|e| {
            StorageError::IoError(format!("Failed to read checkpoint version: {}", e))
        })?;
        let version = u32::from_le_bytes(version_bytes);

        if version != 1 && version != 2 {
            return Err(StorageError::CorruptedData(format!(
                "Unsupported checkpoint version: {}",
                version
            ))
            .into());
        }

        // Read metadata (version-aware)
        let metadata = Self::read_metadata(&mut reader, version)?;

        // In production, would deserialize full storage state
        // For now, just return metadata
        Ok(Checkpoint { metadata })
    }

    /// Read checkpoint metadata (version-aware)
    fn read_metadata<R: Read>(reader: &mut R, version: u32) -> Result<CheckpointMetadata> {
        let mut lsn_bytes = [0u8; 8];
        reader
            .read_exact(&mut lsn_bytes)
            .map_err(|e| StorageError::IoError(format!("Failed to read LSN: {}", e)))?;
        let lsn = LSN(u64::from_le_bytes(lsn_bytes));

        // Phase 2: Read HybridTimestamp (12 bytes: 8 wallclock + 4 logical)
        let mut timestamp_bytes = [0u8; 12];
        reader
            .read_exact(&mut timestamp_bytes)
            .map_err(|e| StorageError::IoError(format!("Failed to read timestamp: {}", e)))?;
        let (timestamp, _) = crate::core::hlc::HybridTimestamp::deserialize(&timestamp_bytes)
            .map_err(|e| {
                StorageError::CorruptedData(format!("Failed to deserialize timestamp: {}", e))
            })?;

        let mut node_count_bytes = [0u8; 8];
        reader
            .read_exact(&mut node_count_bytes)
            .map_err(|e| StorageError::IoError(format!("Failed to read node count: {}", e)))?;
        let node_count = u64::from_le_bytes(node_count_bytes) as usize;

        let mut edge_count_bytes = [0u8; 8];
        reader
            .read_exact(&mut edge_count_bytes)
            .map_err(|e| StorageError::IoError(format!("Failed to read edge count: {}", e)))?;
        let edge_count = u64::from_le_bytes(edge_count_bytes) as usize;

        let mut version_count_bytes = [0u8; 8];
        reader
            .read_exact(&mut version_count_bytes)
            .map_err(|e| StorageError::IoError(format!("Failed to read version count: {}", e)))?;
        let version_count = u64::from_le_bytes(version_count_bytes) as usize;

        // Read vector index config if V2, otherwise None
        let vector_index_config = if version >= 2 {
            Some(
                VectorIndexCheckpointData::deserialize_from(reader).map_err(|e| {
                    StorageError::IoError(format!("Failed to read vector index config: {}", e))
                })?,
            )
        } else {
            None
        };

        Ok(CheckpointMetadata {
            lsn,
            timestamp,
            node_count,
            edge_count,
            version_count,
            vector_index_config,
        })
    }
}

/// Manages persistence, checkpointing, and recovery
pub struct PersistenceManager {
    config: CheckpointConfig,
    last_checkpoint_time: SystemTime,
    last_checkpoint_lsn: LSN,
}

impl PersistenceManager {
    /// Create a new persistence manager
    pub fn new(config: CheckpointConfig) -> Result<Self> {
        // Create checkpoint directory if it doesn't exist
        std::fs::create_dir_all(&config.checkpoint_dir).map_err(|e| {
            StorageError::IoError(format!("Failed to create checkpoint directory: {}", e))
        })?;

        Ok(PersistenceManager {
            config,
            last_checkpoint_time: UNIX_EPOCH,
            last_checkpoint_lsn: LSN::initial(),
        })
    }

    /// Check if a checkpoint should be created
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
        if lsn_diff >= self.config.min_wal_entries {
            return true;
        }

        false
    }

    /// List all checkpoint files in the checkpoint directory, sorted by filename.
    ///
    /// This helper method extracts the common directory scanning logic used by
    /// `find_latest_checkpoint()` and (future) `cleanup_old_checkpoints()`.
    /// It reads the checkpoint directory, filters for valid checkpoint files
    /// (matching "checkpoint_*.dat" pattern), and returns them sorted by filename.
    ///
    /// # Returns
    ///
    /// A vector of checkpoint file paths, sorted in ascending order by filename.
    /// Returns an empty vector if the directory doesn't exist or contains no
    /// checkpoint files.
    ///
    /// # Errors
    ///
    /// Returns `Err` for I/O errors other than "directory not found" (e.g.,
    /// permission denied). Missing directories are handled gracefully by
    /// returning an empty vector.
    pub(crate) fn list_checkpoints(&self) -> Result<Vec<PathBuf>> {
        let mut checkpoints = Vec::new();

        // Read directory - handle missing directory gracefully, propagate other errors
        match std::fs::read_dir(&self.config.checkpoint_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| {
                        StorageError::IoError(format!(
                            "Failed to read directory entry in checkpoint dir: {}",
                            e
                        ))
                    })?;

                    // Filter for checkpoint files matching pattern: checkpoint_*.dat
                    if let Some(name) = entry.file_name().to_str()
                        && name.starts_with("checkpoint_")
                        && name.ends_with(".dat")
                    {
                        checkpoints.push(entry.path());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Directory doesn't exist - return empty vector (graceful handling)
                return Ok(checkpoints);
            }
            Err(e) => {
                // Other I/O errors (e.g., permission denied) - propagate
                return Err(StorageError::IoError(format!(
                    "Failed to read checkpoint directory: {}",
                    e
                ))
                .into());
            }
        }

        // Sort by filename (LSN is encoded in the filename)
        checkpoints.sort();

        Ok(checkpoints)
    }

    /// Find the most recent checkpoint
    pub fn find_latest_checkpoint(&self) -> Result<Option<Checkpoint>> {
        self.list_checkpoints()?
            .last()
            .map(|path| Checkpoint::load(path.as_ref()))
            .transpose()
    }

    /// Recover database state from checkpoint and WAL
    pub fn recover(
        &mut self,
        wal: &ConcurrentWalSystem,
    ) -> Result<(CurrentStorage, HistoricalStorage, LSN)> {
        // Try to load latest checkpoint
        let checkpoint = self.find_latest_checkpoint()?;

        // In production, would deserialize storage from checkpoint
        // For now, always start with empty storage
        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();

        // Restore vector index configuration BEFORE WAL replay (Issue #292)
        // This ensures that vectors are automatically indexed during node creation.
        // If vector index restoration fails, recovery fails to maintain data integrity.
        if let Some(cp) = &checkpoint
            && let Some(vector_config) = &cp.metadata.vector_index_config
            && vector_config.enabled
        {
            current
                .enable_vector_index(&vector_config.property_name, vector_config.config.clone())?;
        }

        let start_lsn = if let Some(cp) = checkpoint {
            cp.metadata.lsn.next()
        } else {
            LSN::initial()
        };

        // Track max IDs during replay for ID generator initialization (Issue #291)
        let mut max_node_id: u64 = 0;
        let mut max_edge_id: u64 = 0;
        let mut max_version_id: u64 = 0;

        // Track next version_id for sequential version assignment
        // NOTE: We track both max_version_id and next_version_id because:
        // - max_version_id: Tracks the highest version ID seen (for ID generator init)
        // - next_version_id: Tracks the next ID to assign for delete tombstones
        // We can't optimize this to compute next_version_id at the end because delete
        // operations need to generate tombstone version IDs during replay.
        let mut next_version_id: u64 = 1;

        // Replay WAL entries since checkpoint
        let wal_entries = wal.read_from(start_lsn)?;

        for entry in wal_entries {
            use crate::storage::wal::WalOperation;

            match entry.operation {
                WalOperation::CreateNode {
                    node_id,
                    label,
                    properties,
                    temporal,
                } => {
                    // Track max node ID for ID generator initialization (Issue #291)
                    max_node_id = max_node_id.max(node_id.as_u64());

                    // Replay the CreateNode operation
                    self.replay_create_node(
                        &current,
                        &mut historical,
                        node_id,
                        label,
                        properties,
                        temporal,
                        &mut next_version_id,
                    )?;

                    // Track max version ID
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
                    // Track max edge ID for ID generator initialization (Issue #291)
                    max_edge_id = max_edge_id.max(edge_id.as_u64());

                    // Replay the CreateEdge operation
                    self.replay_create_edge(
                        &current,
                        &mut historical,
                        edge_id,
                        source,
                        target,
                        label,
                        properties,
                        temporal,
                        &mut next_version_id,
                    )?;

                    // Track max version ID
                    max_version_id = max_version_id.max(next_version_id - 1);
                }
                WalOperation::UpdateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    temporal,
                } => {
                    // Track max version ID
                    max_version_id = max_version_id.max(version_id.as_u64());

                    // CRITICAL: Update next_version_id to avoid conflicts with subsequent operations
                    // If this update uses version_id = N, the next version should be N+1
                    // This applies to all future operations (updates, deletes, creates)
                    next_version_id = next_version_id.max(version_id.as_u64() + 1);

                    // Replay the UpdateNode operation
                    self.replay_update_node(
                        &current,
                        &mut historical,
                        node_id,
                        version_id,
                        label,
                        properties,
                        temporal,
                    )?;
                }
                WalOperation::UpdateEdge {
                    edge_id,
                    version_id,
                    label,
                    properties,
                    temporal,
                } => {
                    // Track max version ID
                    max_version_id = max_version_id.max(version_id.as_u64());

                    // CRITICAL: Update next_version_id to avoid conflicts with subsequent operations
                    // If this update uses version_id = N, the next version should be N+1
                    // This applies to all future operations (updates, deletes, creates)
                    next_version_id = next_version_id.max(version_id.as_u64() + 1);

                    // Replay the UpdateEdge operation
                    // Note: source and target are not in WalOperation::UpdateEdge
                    // We need to get them from current storage
                    let current_edge = current.get_edge(edge_id)?;
                    self.replay_update_edge(
                        &current,
                        &mut historical,
                        edge_id,
                        version_id,
                        current_edge.source,
                        current_edge.target,
                        label,
                        properties,
                        temporal,
                    )?;
                }
                WalOperation::DeleteNode { node_id, temporal } => {
                    // Replay the DeleteNode operation
                    // CRITICAL: This closes the previous version's transaction_time
                    // BEFORE creating the tombstone for correct bi-temporal semantics
                    self.replay_delete_node(
                        &current,
                        &mut historical,
                        node_id,
                        temporal,
                        &mut next_version_id,
                    )?;

                    // Track max version ID
                    max_version_id = max_version_id.max(next_version_id - 1);
                }
                WalOperation::DeleteEdge { edge_id, temporal } => {
                    // Replay the DeleteEdge operation
                    // CRITICAL: This closes the previous version's transaction_time
                    // BEFORE creating the tombstone for correct bi-temporal semantics
                    self.replay_delete_edge(
                        &current,
                        &mut historical,
                        edge_id,
                        temporal,
                        &mut next_version_id,
                    )?;

                    // Track max version ID
                    max_version_id = max_version_id.max(next_version_id - 1);
                }
                WalOperation::Checkpoint { lsn, timestamp: _ } => {
                    // Update recovery tracking - checkpoint markers indicate progress
                    self.last_checkpoint_lsn = lsn;
                }
            }
        }

        let final_lsn = wal.current_lsn();

        // Initialize ID generators with max_id + 1 to prevent ID conflicts (Issue #291)
        // If max_id is 0 (empty WAL), generators start from 1 (default behavior)
        // Otherwise, generators continue from max_id + 1
        current.init_node_id_generator(max_node_id + 1);
        current.init_edge_id_generator(max_edge_id + 1);
        current.init_version_id_generator(max_version_id + 1);

        Ok((current, historical, final_lsn))
    }

    /// Replay CreateNode operation during recovery
    #[allow(clippy::too_many_arguments)]
    fn replay_create_node(
        &self,
        current: &CurrentStorage,
        historical: &mut HistoricalStorage,
        node_id: NodeId,
        label: InternedString,
        properties: PropertyMap,
        temporal: BiTemporalInterval,
        next_version_id: &mut u64,
    ) -> Result<()> {
        // Validate that the InternedString ID exists in the interner
        // This protects against corrupted WAL files or missing checkpoint data
        GLOBAL_INTERNER.resolve(label).ok_or_else(|| {
            StorageError::CorruptedData(format!(
                "InternedString ID {} for node_id={} not found in interner. \
                 This indicates corrupted WAL or missing checkpoint data.",
                label.as_u32(),
                node_id
            ))
        })?;

        // ID is valid, use it directly (no allocation!)
        let interned_label = label;

        // Extract commit timestamp from temporal interval
        let commit_timestamp = temporal.transaction_time().start();

        // Create version metadata with recovery transaction ID (0)
        let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

        // Generate unique version_id for this version (sequential across all entities)
        let version_id = VersionId::new(*next_version_id)?;
        *next_version_id += 1;

        // Create the node with all metadata
        let node = Node::with_metadata(
            node_id,
            interned_label,
            properties.clone(),
            version_id,
            metadata,
        );

        // Insert into current storage (handles vector indexing automatically)
        current.insert_node_direct(node, commit_timestamp)?;

        // Add to historical storage for temporal queries
        historical.add_node_version(node_id, version_id, temporal, interned_label, properties)?;

        Ok(())
    }

    /// Replay CreateEdge operation during recovery
    #[allow(clippy::too_many_arguments)]
    fn replay_create_edge(
        &self,
        current: &CurrentStorage,
        historical: &mut HistoricalStorage,
        edge_id: EdgeId,
        source: NodeId,
        target: NodeId,
        label: InternedString,
        properties: PropertyMap,
        temporal: BiTemporalInterval,
        next_version_id: &mut u64,
    ) -> Result<()> {
        // Validate that the InternedString ID exists in the interner
        GLOBAL_INTERNER.resolve(label).ok_or_else(|| {
            StorageError::CorruptedData(format!(
                "InternedString ID {} for edge_id={} not found in interner. \
                 This indicates corrupted WAL or missing checkpoint data.",
                label.as_u32(),
                edge_id
            ))
        })?;

        // ID is valid, use it directly (no allocation!)
        let interned_label = label;

        // Extract commit timestamp from temporal interval
        let commit_timestamp = temporal.transaction_time().start();

        // Create version metadata with recovery transaction ID (0)
        let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

        // Generate unique version_id for this version (sequential across all entities)
        let version_id = VersionId::new(*next_version_id)?;
        *next_version_id += 1;

        // Create the edge with all metadata
        let edge = Edge::with_metadata(
            edge_id,
            interned_label,
            source,
            target,
            properties.clone(),
            version_id,
            metadata,
        );

        // Insert into current storage
        current.insert_edge_direct(edge)?;

        // Add to historical storage for temporal queries
        historical.add_edge_version(
            edge_id,
            version_id,
            temporal,
            interned_label,
            source,
            target,
            properties,
        )?;

        Ok(())
    }

    /// Replay UpdateNode operation during recovery
    #[allow(clippy::too_many_arguments)]
    fn replay_update_node(
        &self,
        current: &CurrentStorage,
        historical: &mut HistoricalStorage,
        node_id: NodeId,
        version_id: VersionId,
        label: InternedString,
        properties: PropertyMap,
        temporal: BiTemporalInterval,
    ) -> Result<()> {
        // Validate that the InternedString ID exists in the interner
        GLOBAL_INTERNER.resolve(label).ok_or_else(|| {
            StorageError::CorruptedData(format!(
                "InternedString ID {} for node_id={} version_id={} not found in interner. \
                 This indicates corrupted WAL or missing checkpoint data.",
                label.as_u32(),
                node_id,
                version_id
            ))
        })?;

        // ID is valid, use it directly (no allocation!)
        let interned_label = label;

        // Extract commit timestamp from temporal interval
        let commit_timestamp = temporal.transaction_time().start();

        // Create version metadata with recovery transaction ID (0)
        let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

        // Create the updated node with all metadata
        let node = Node::with_metadata(
            node_id,
            interned_label,
            properties.clone(),
            version_id,
            metadata,
        );

        // Update in current storage (handles vector indexing automatically)
        current.update_node_direct(node, commit_timestamp)?;

        // Close previous version's transaction_time in historical storage
        // This is CRITICAL for correct bi-temporal semantics
        if let Some(prev_version_id) = historical.get_current_node_version(node_id) {
            historical.close_node_version_transaction_time(prev_version_id, commit_timestamp)?;
        }

        // Add new version to historical storage for temporal queries
        historical.add_node_version(node_id, version_id, temporal, interned_label, properties)?;

        Ok(())
    }

    /// Replay UpdateEdge operation during recovery
    #[allow(clippy::too_many_arguments)]
    fn replay_update_edge(
        &self,
        current: &CurrentStorage,
        historical: &mut HistoricalStorage,
        edge_id: EdgeId,
        version_id: VersionId,
        source: NodeId,
        target: NodeId,
        label: InternedString,
        properties: PropertyMap,
        temporal: BiTemporalInterval,
    ) -> Result<()> {
        // Validate that the InternedString ID exists in the interner
        GLOBAL_INTERNER.resolve(label).ok_or_else(|| {
            StorageError::CorruptedData(format!(
                "InternedString ID {} for edge_id={} version_id={} not found in interner. \
                 This indicates corrupted WAL or missing checkpoint data.",
                label.as_u32(),
                edge_id,
                version_id
            ))
        })?;

        // ID is valid, use it directly (no allocation!)
        let interned_label = label;

        // Extract commit timestamp from temporal interval
        let commit_timestamp = temporal.transaction_time().start();

        // Create version metadata with recovery transaction ID (0)
        let metadata = VersionMetadata::new(TxId::new(RECOVERY_TX_ID), commit_timestamp);

        // Create the updated edge with all metadata
        let edge = Edge::with_metadata(
            edge_id,
            interned_label,
            source,
            target,
            properties.clone(),
            version_id,
            metadata,
        );

        // Update in current storage
        current.update_edge_direct(edge)?;

        // Close previous version's transaction_time in historical storage
        // This is CRITICAL for correct bi-temporal semantics
        if let Some(prev_version_id) = historical.get_current_edge_version(edge_id) {
            historical.close_edge_version_transaction_time(prev_version_id, commit_timestamp)?;
        }

        // Add new version to historical storage for temporal queries
        historical.add_edge_version(
            edge_id,
            version_id,
            temporal,
            interned_label,
            source,
            target,
            properties,
        )?;

        Ok(())
    }

    /// Replay DeleteNode operation during recovery
    fn replay_delete_node(
        &self,
        current: &CurrentStorage,
        historical: &mut HistoricalStorage,
        node_id: NodeId,
        temporal: BiTemporalInterval,
        next_version_id: &mut u64,
    ) -> Result<()> {
        // Get the node before deleting (to capture label and properties for tombstone)
        let node = current.get_node(node_id)?;

        // Extract commit timestamp from temporal interval
        let commit_timestamp = temporal.transaction_time().start();

        // Close the current version's transaction_time in historical storage
        // This is CRITICAL for correct bi-temporal semantics
        if let Some(current_version_id) = historical.get_current_node_version(node_id) {
            historical.close_node_version_transaction_time(current_version_id, commit_timestamp)?;
        }

        // Generate version ID for the tombstone
        let tombstone_version_id = VersionId::new(*next_version_id)?;
        *next_version_id += 1;

        // Create tombstone temporal interval
        // The tombstone marks when the deletion occurred. Its transaction_time
        // starts at commit_timestamp and remains open. Its valid_time is closed
        // immediately since the entity no longer exists.
        let tombstone_temporal = temporal.close_valid_time(commit_timestamp);

        // Add tombstone version to historical storage with the node's label and properties
        historical.add_node_version(
            node_id,
            tombstone_version_id,
            tombstone_temporal,
            node.label,
            node.properties.clone(),
        )?;

        // Delete from current storage
        current.delete_node_direct(node_id, commit_timestamp)?;

        Ok(())
    }

    /// Replay DeleteEdge operation during recovery
    fn replay_delete_edge(
        &self,
        current: &CurrentStorage,
        historical: &mut HistoricalStorage,
        edge_id: EdgeId,
        temporal: BiTemporalInterval,
        next_version_id: &mut u64,
    ) -> Result<()> {
        // Get the edge before deleting (to capture label, source, target, properties for tombstone)
        let edge = current.get_edge(edge_id)?;

        // Extract commit timestamp from temporal interval
        let commit_timestamp = temporal.transaction_time().start();

        // Close the current version's transaction_time in historical storage
        // This is CRITICAL for correct bi-temporal semantics
        if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
            historical.close_edge_version_transaction_time(current_version_id, commit_timestamp)?;
        }

        // Generate version ID for the tombstone
        let tombstone_version_id = VersionId::new(*next_version_id)?;
        *next_version_id += 1;

        // Create tombstone temporal interval
        // The tombstone marks when the deletion occurred. Its transaction_time
        // starts at commit_timestamp and remains open. Its valid_time is closed
        // immediately since the entity no longer exists.
        let tombstone_temporal = temporal.close_valid_time(commit_timestamp);

        // Add tombstone version to historical storage with the edge's data
        historical.add_edge_version(
            edge_id,
            tombstone_version_id,
            tombstone_temporal,
            edge.label,
            edge.source,
            edge.target,
            edge.properties.clone(),
        )?;

        // Delete from current storage
        current.delete_edge_direct(edge_id)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GLOBAL_INTERNER;
    use crate::core::graph::Node;
    use crate::storage::version::VersionMetadata;
    use crate::{PropertyMapBuilder, api::transaction::types::TxId};
    use tempfile::TempDir;

    #[test]
    fn test_checkpoint_metadata() {
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        let checkpoint = Checkpoint::new(LSN(100), &current, &historical);

        assert_eq!(checkpoint.metadata.lsn, LSN(100));
        assert_eq!(checkpoint.metadata.node_count, 0);
        assert_eq!(checkpoint.metadata.edge_count, 0);
    }

    #[test]
    fn test_checkpoint_save_load() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("test_checkpoint.dat");

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        let checkpoint = Checkpoint::new(LSN(42), &current, &historical);
        checkpoint.save(&checkpoint_path)?;

        let loaded = Checkpoint::load(&checkpoint_path)?;
        assert_eq!(loaded.metadata.lsn, LSN(42));

        Ok(())
    }

    #[test]
    fn test_persistence_manager_creation() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };

        let _manager = PersistenceManager::new(config)?;
        assert!(temp_dir.path().exists());

        Ok(())
    }

    #[test]
    fn test_should_checkpoint_time() {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().to_path_buf(),
            checkpoint_interval: Duration::from_secs(1),
            ..Default::default()
        };

        let mut manager = PersistenceManager::new(config).unwrap();

        // Initially should checkpoint
        assert!(manager.should_checkpoint(LSN(1000)));

        // After updating last checkpoint time, should not checkpoint immediately
        manager.last_checkpoint_time = SystemTime::now();
        manager.last_checkpoint_lsn = LSN(100);

        assert!(!manager.should_checkpoint(LSN(150)));
    }

    #[test]
    fn test_should_checkpoint_lsn() {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().to_path_buf(),
            checkpoint_interval: Duration::from_secs(3600), // Very long
            min_wal_entries: 100,
            ..Default::default()
        };

        let mut manager = PersistenceManager::new(config).unwrap();
        manager.last_checkpoint_time = SystemTime::now();
        manager.last_checkpoint_lsn = LSN(1);

        // Should checkpoint when LSN threshold reached
        assert!(manager.should_checkpoint(LSN(150)));
        assert!(!manager.should_checkpoint(LSN(50)));
    }

    // V2 Checkpoint Serialization Tests

    #[test]
    fn test_hnsw_config_serialization_roundtrip() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use std::io::Cursor;

        let config = HnswConfig::new(384, DistanceMetric::Cosine)
            .with_m(32)
            .with_ef_construction(200)
            .with_ef_search(100)
            .with_capacity(5000);

        // Serialize
        let mut buffer = Vec::new();
        config.serialize_into(&mut buffer)?;

        // Deserialize
        let mut cursor = Cursor::new(buffer);
        let loaded = HnswConfig::deserialize_from(&mut cursor)?;

        assert_eq!(loaded.dimensions, 384);
        assert_eq!(loaded.metric, DistanceMetric::Cosine);
        assert_eq!(loaded.m, 32);
        assert_eq!(loaded.ef_construction, 200);
        assert_eq!(loaded.ef_search, 100);
        assert_eq!(loaded.capacity, 5000);

        Ok(())
    }

    #[test]
    fn test_vector_index_checkpoint_data_disabled() -> Result<()> {
        use std::io::Cursor;

        let data = VectorIndexCheckpointData::disabled();

        // Serialize
        let mut buffer = Vec::new();
        data.serialize_into(&mut buffer)?;

        // Deserialize
        let mut cursor = Cursor::new(buffer);
        let loaded = VectorIndexCheckpointData::deserialize_from(&mut cursor)?;

        assert!(!loaded.enabled);
        assert_eq!(loaded.property_name, "");

        Ok(())
    }

    #[test]
    fn test_vector_index_checkpoint_data_enabled() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use std::io::Cursor;

        let config = HnswConfig::new(768, DistanceMetric::Euclidean);
        let data = VectorIndexCheckpointData::enabled("embedding".to_string(), config.clone());

        // Serialize
        let mut buffer = Vec::new();
        data.serialize_into(&mut buffer)?;

        // Deserialize
        let mut cursor = Cursor::new(buffer);
        let loaded = VectorIndexCheckpointData::deserialize_from(&mut cursor)?;

        assert!(loaded.enabled);
        assert_eq!(loaded.property_name, "embedding");
        assert_eq!(loaded.config.dimensions, 768);
        assert_eq!(loaded.config.metric, DistanceMetric::Euclidean);

        Ok(())
    }

    #[test]
    fn test_checkpoint_v2_save_load_with_vector_config() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("test_v2_checkpoint.dat");

        // Create storage with vector index enabled
        let current = CurrentStorage::new();
        let config = HnswConfig::new(384, DistanceMetric::Cosine);
        current.enable_vector_index("embedding", config)?;

        let historical = HistoricalStorage::new();

        // Create and save checkpoint
        let checkpoint = Checkpoint::new(LSN(123), &current, &historical);
        checkpoint.save(&checkpoint_path)?;

        // Load checkpoint
        let loaded = Checkpoint::load(&checkpoint_path)?;

        // Verify metadata
        assert_eq!(loaded.metadata.lsn, LSN(123));

        // Verify vector config was preserved
        assert!(loaded.metadata.vector_index_config.is_some());
        let vector_config = loaded.metadata.vector_index_config.unwrap();
        assert!(vector_config.enabled);
        assert_eq!(vector_config.property_name, "embedding");
        assert_eq!(vector_config.config.dimensions, 384);
        assert_eq!(vector_config.config.metric, DistanceMetric::Cosine);

        Ok(())
    }

    #[test]
    fn test_checkpoint_v2_save_load_without_vector_config() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("test_v2_no_vector.dat");

        // Create storage without vector index
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Create and save checkpoint
        let checkpoint = Checkpoint::new(LSN(456), &current, &historical);
        checkpoint.save(&checkpoint_path)?;

        // Load checkpoint
        let loaded = Checkpoint::load(&checkpoint_path)?;

        // Verify metadata
        assert_eq!(loaded.metadata.lsn, LSN(456));

        // Verify vector config exists but is disabled
        assert!(loaded.metadata.vector_index_config.is_some());
        let vector_config = loaded.metadata.vector_index_config.unwrap();
        assert!(!vector_config.enabled);

        Ok(())
    }

    #[test]
    fn test_distance_metric_encoding() -> Result<()> {
        use crate::index::vector::DistanceMetric;

        // Test all variants
        assert_eq!(DistanceMetric::Cosine.to_u8(), 0);
        assert_eq!(DistanceMetric::Euclidean.to_u8(), 1);
        assert_eq!(DistanceMetric::DotProduct.to_u8(), 2);

        // Test roundtrip
        assert_eq!(DistanceMetric::from_u8(0)?, DistanceMetric::Cosine);
        assert_eq!(DistanceMetric::from_u8(1)?, DistanceMetric::Euclidean);
        assert_eq!(DistanceMetric::from_u8(2)?, DistanceMetric::DotProduct);

        // Test invalid value
        assert!(DistanceMetric::from_u8(99).is_err());

        Ok(())
    }

    #[test]
    fn test_checkpoint_v2_binary_format_size() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("test_size.dat");

        let current = CurrentStorage::new();
        let config = HnswConfig::new(384, DistanceMetric::Cosine);
        current.enable_vector_index("test_property", config)?;

        let historical = HistoricalStorage::new();
        let checkpoint = Checkpoint::new(LSN(1), &current, &historical);
        checkpoint.save(&checkpoint_path)?;

        // Verify file exists and has expected structure
        let metadata = std::fs::metadata(&checkpoint_path)?;

        // Expected size:
        // - Magic (4) + Version (4) = 8
        // - LSN (8) + Timestamp (12) + NodeCount (8) + EdgeCount (8) + VersionCount (8) = 44
        //   (Phase 2: Timestamp is now HybridTimestamp: 8-byte wallclock + 4-byte logical = 12 bytes)
        // - Vector config: enabled (1) + name_len (4) + "test_property" (13) + HnswConfig (41) = 59
        // Total = 111 bytes (was 107 before Phase 2)
        assert_eq!(metadata.len(), 111);

        Ok(())
    }

    // ========== New Error Handling Tests for Coverage ==========

    #[test]
    fn test_checkpoint_load_invalid_magic_bytes() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("corrupted.dat");

        // Create valid checkpoint first
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();
        let checkpoint = Checkpoint::new(LSN(1), &current, &historical);
        checkpoint.save(&checkpoint_path)?;

        // Corrupt magic bytes
        let mut data = std::fs::read(&checkpoint_path)?;
        data[0..4].copy_from_slice(b"XXXX"); // Invalid magic
        std::fs::write(&checkpoint_path, data)?;

        // Attempt to load - should fail with CorruptedData error
        let result = Checkpoint::load(&checkpoint_path);

        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Storage(
                    crate::utils::error::StorageError::CorruptedData(_)
                )
            ),
            "Expected CorruptedData error for invalid magic bytes"
        );

        Ok(())
    }

    #[test]
    fn test_checkpoint_load_unsupported_future_version() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("future_version.dat");

        // Create valid checkpoint
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();
        let checkpoint = Checkpoint::new(LSN(1), &current, &historical);
        checkpoint.save(&checkpoint_path)?;

        // Modify version to future version (99)
        let mut data = std::fs::read(&checkpoint_path)?;
        data[4..8].copy_from_slice(&99u32.to_le_bytes());
        std::fs::write(&checkpoint_path, data)?;

        // Attempt to load - should fail with CorruptedData error for unsupported version
        let result = Checkpoint::load(&checkpoint_path);

        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Storage(
                    crate::utils::error::StorageError::CorruptedData(_)
                )
            ),
            "Expected CorruptedData error for unsupported version"
        );

        Ok(())
    }

    #[test]
    fn test_checkpoint_v1_backward_compatibility() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("v1_checkpoint.dat");

        // Manually create V1 format checkpoint (version 1, no vector config)
        let mut buffer = Vec::new();

        // Magic bytes
        buffer.extend_from_slice(b"GFRY");

        // Version 1
        buffer.extend_from_slice(&1u32.to_le_bytes());

        // LSN
        buffer.extend_from_slice(&42u64.to_le_bytes());

        // Timestamp
        buffer.extend_from_slice(&time::now().serialize());

        // Counts
        buffer.extend_from_slice(&0u64.to_le_bytes()); // node_count
        buffer.extend_from_slice(&0u64.to_le_bytes()); // edge_count
        buffer.extend_from_slice(&0u64.to_le_bytes()); // version_count

        // V1 doesn't have vector config - ends here

        std::fs::write(&checkpoint_path, buffer)?;

        // Load V1 checkpoint
        let loaded = Checkpoint::load(&checkpoint_path)?;

        assert_eq!(loaded.metadata.lsn, LSN(42));
        assert!(loaded.metadata.vector_index_config.is_none());

        Ok(())
    }

    #[test]
    fn test_checkpoint_invalid_utf8_property_name() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_path = temp_dir.path().join("invalid_utf8.dat");

        // Create valid V2 checkpoint with vector config
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"GFRY");
        buffer.extend_from_slice(&2u32.to_le_bytes());
        buffer.extend_from_slice(&1u64.to_le_bytes());
        buffer.extend_from_slice(&time::now().serialize());
        buffer.extend_from_slice(&0u64.to_le_bytes());
        buffer.extend_from_slice(&0u64.to_le_bytes());
        buffer.extend_from_slice(&0u64.to_le_bytes());

        // Vector config with invalid UTF-8 in property name
        buffer.push(1); // enabled
        buffer.extend_from_slice(&4u32.to_le_bytes()); // name length = 4
        buffer.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xFC]); // Invalid UTF-8 bytes

        std::fs::write(&checkpoint_path, buffer)?;

        // Attempt to load
        let result = Checkpoint::load(&checkpoint_path);

        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("UTF-8") || err_str.contains("corrupt"));

        Ok(())
    }

    #[test]
    fn test_recovery_from_empty_checkpoint_dir() -> Result<()> {
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;

        let temp_dir = TempDir::new().unwrap();

        // Create empty WAL
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path().join("wal"));
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create persistence manager with empty checkpoint directory
        let config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().join("checkpoints"),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;

        // Recover should return empty storage and initial LSN
        let result = manager.recover(&wal);

        // Should either succeed with empty storage or return appropriate error
        match result {
            Ok((current, _historical, lsn)) => {
                // Successful recovery from empty state
                assert_eq!(current.node_count(), 0);
                assert_eq!(lsn, LSN::initial());
            }
            Err(e) => {
                // Recovery not fully implemented - document expected behavior
                let err_str = e.to_string();
                assert!(err_str.contains("not implemented") || err_str.contains("stub"));
            }
        }

        Ok(())
    }

    #[test]
    fn test_recovery_replay_wal_from_checkpoint_lsn() -> Result<()> {
        use crate::core::id::NodeId;
        use crate::core::property::PropertyMap;
        use crate::core::temporal::BiTemporalInterval;
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");

        // Create WAL with some entries
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Append entries to WAL (LSN 1-5)
        for i in 1..=5 {
            wal.append(crate::storage::wal::WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: GLOBAL_INTERNER.intern("Test").unwrap(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Create checkpoint at LSN 3
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();
        let checkpoint_path = temp_dir.path().join("checkpoint.dat");
        let checkpoint = Checkpoint::new(LSN(3), &current, &historical);
        checkpoint.save(&checkpoint_path)?;

        // Recovery should start replay from LSN 4 (after checkpoint)
        let config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;

        let result = manager.recover(&wal);

        // Recovery might not be fully implemented yet
        match result {
            Ok((_current, _historical, lsn)) => {
                // If recovery works, LSN should be after checkpoint
                assert!(lsn >= LSN(3));
            }
            Err(e) => {
                // Recovery not fully implemented - document expected behavior
                let err_str = e.to_string();
                assert!(err_str.contains("not implemented") || err_str.contains("stub"));
            }
        }

        Ok(())
    }

    // ========================================================================
    // Vector Index Recovery Tests (Issue #292)
    // ========================================================================

    /// Test that vector index configuration is restored from checkpoint.
    ///
    /// This test verifies that when a database with an enabled vector index
    /// crashes and recovers, the vector index configuration is properly restored.
    #[test]
    fn test_recovery_restores_vector_index_config() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let checkpoint_dir = temp_dir.path().join("checkpoints");

        // Phase 1: Setup database with vector index
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Enable vector index with specific configuration
        let vector_config = HnswConfig::new(384, DistanceMetric::Cosine)
            .with_m(16)
            .with_ef_construction(200);
        current.enable_vector_index("embedding", vector_config.clone())?;

        // Create checkpoint with vector index enabled
        let checkpoint = Checkpoint::new(LSN(1), &current, &historical);
        std::fs::create_dir_all(&checkpoint_dir)?;
        let checkpoint_path = checkpoint_dir.join("checkpoint_000001.dat");
        checkpoint.save(&checkpoint_path)?;

        // Phase 2: Simulate crash and recovery
        drop(current);
        drop(historical);

        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Verify vector index is enabled after recovery
        assert!(
            recovered_current.is_vector_index_enabled(),
            "Vector index should be enabled after recovery"
        );

        // Verify configuration matches original
        let recovered_config = recovered_current.get_hnsw_config_for("embedding");
        assert!(
            recovered_config.is_some(),
            "Vector index config should exist"
        );
        let recovered_config = recovered_config.unwrap();
        assert_eq!(recovered_config.dimensions, 384);
        assert_eq!(recovered_config.metric, DistanceMetric::Cosine);
        assert_eq!(recovered_config.m, 16);
        assert_eq!(recovered_config.ef_construction, 200);

        Ok(())
    }

    /// Test crash-and-recover with 10 nodes containing vector embeddings.
    ///
    /// This is the main acceptance test from Issue #292. It verifies that:
    /// 1. Vector indexes restore when enabled in checkpoints
    /// 2. All vectors get re-indexed during recovery
    /// 3. Similarity queries function properly post-recovery
    #[test]
    fn test_recovery_with_vector_embeddings() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use crate::storage::wal::WalOperation;
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let checkpoint_dir = temp_dir.path().join("checkpoints");

        // Phase 1: Create database with vector index and 10 embedded nodes
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Enable vector index for embeddings (4D for simplicity)
        let vector_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        current.enable_vector_index("embedding", vector_config)?;

        // Create 10 nodes with embeddings and write to WAL
        let embeddings = vec![
            vec![1.0f32, 0.0, 0.0, 0.0],
            vec![0.9f32, 0.1, 0.0, 0.0],
            vec![0.8f32, 0.2, 0.0, 0.0],
            vec![0.0f32, 1.0, 0.0, 0.0],
            vec![0.0f32, 0.9, 0.1, 0.0],
            vec![0.0f32, 0.0, 1.0, 0.0],
            vec![0.0f32, 0.0, 0.9, 0.1],
            vec![0.0f32, 0.0, 0.0, 1.0],
            vec![0.5f32, 0.5, 0.0, 0.0],
            vec![0.0f32, 0.5, 0.5, 0.0],
        ];

        let mut node_ids = Vec::new();
        for (i, embedding) in embeddings.iter().enumerate() {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Node{}", i))
                .insert_vector("embedding", embedding)
                .build();

            let node_id = NodeId::new(i as u64 + 1)?;
            let temporal = BiTemporalInterval::current(time::now());

            // Write to WAL
            wal.append(WalOperation::CreateNode {
                node_id,
                label: GLOBAL_INTERNER.intern("Document").unwrap(),
                properties: props.clone(),
                temporal,
            })?;

            // Create in storage for checkpoint
            let interned_label = GLOBAL_INTERNER.intern("Document")?;
            let version_id = VersionId::new(i as u64 + 1)?;
            let commit_timestamp = temporal.transaction_time().start();
            let metadata = VersionMetadata::new(TxId::new(0), commit_timestamp);
            let node =
                Node::with_metadata(node_id, interned_label, props.clone(), version_id, metadata);
            current.insert_node_direct(node, commit_timestamp)?;

            node_ids.push(node_id);
        }

        wal.flush()?;

        // Create checkpoint at LSN 0 to force WAL replay of all node operations.
        // Nodes are already in memory/WAL, but checkpoint LSN=0 ensures recovery
        // will replay all entries starting from the beginning.
        let checkpoint = Checkpoint::new(LSN(0), &current, &historical);
        std::fs::create_dir_all(&checkpoint_dir)?;
        let checkpoint_path = checkpoint_dir.join("checkpoint_000000.dat");
        checkpoint.save(&checkpoint_path)?;

        // Phase 2: Simulate crash - drop all storage
        drop(current);
        drop(historical);

        // Phase 3: Recovery
        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Phase 4: Verify recovery
        // 4.1: All 10 nodes should exist
        assert_eq!(
            recovered_current.node_count(),
            10,
            "All 10 nodes should be recovered"
        );

        // 4.2: Vector index should be enabled
        assert!(
            recovered_current.is_vector_index_enabled(),
            "Vector index should be enabled after recovery"
        );

        // 4.3: Similarity queries should work
        // Query with first embedding - should find similar nodes
        let query_embedding = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = recovered_current.search_vectors_in("embedding", &query_embedding, 3)?;

        assert!(
            results.len() >= 2,
            "Should find at least 2 similar nodes (self + similar)"
        );

        // The first two nodes (indices 0, 1, 2) should be most similar to query
        let result_ids: Vec<NodeId> = results.iter().map(|(id, _)| *id).collect();
        assert!(
            result_ids.contains(&node_ids[0]) || result_ids.contains(&node_ids[1]),
            "Should find nodes with similar embeddings"
        );

        // 4.4: Verify individual node embeddings are preserved
        for (i, node_id) in node_ids.iter().enumerate() {
            let node = recovered_current.get_node(*node_id)?;
            let recovered_embedding = node
                .get_property("embedding")
                .and_then(|v| v.as_vector())
                .expect("Node should have embedding property");

            assert_eq!(
                recovered_embedding,
                &embeddings[i][..],
                "Embedding for Node{} should match original",
                i
            );
        }

        Ok(())
    }

    /// Test that disabled vector indexes don't cause errors during recovery.
    ///
    /// This test verifies graceful handling when no vector index exists
    /// in the checkpoint (acceptance criteria from Issue #292).
    #[test]
    fn test_recovery_without_vector_index() -> Result<()> {
        use crate::storage::wal::WalOperation;
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let checkpoint_dir = temp_dir.path().join("checkpoints");

        // Phase 1: Create database WITHOUT vector index
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Note: No vector index enabled

        // Create a few nodes without embeddings
        for i in 0..3 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Node{}", i))
                .build();

            let node_id = NodeId::new(i + 1)?;
            let temporal = BiTemporalInterval::current(time::now());

            wal.append(WalOperation::CreateNode {
                node_id,
                label: GLOBAL_INTERNER.intern("Document").unwrap(),
                properties: props.clone(),
                temporal,
            })?;

            let interned_label = GLOBAL_INTERNER.intern("Document")?;
            let version_id = VersionId::new(i + 1)?;
            let commit_timestamp = temporal.transaction_time().start();
            let metadata = VersionMetadata::new(TxId::new(0), commit_timestamp);
            let node = Node::with_metadata(node_id, interned_label, props, version_id, metadata);
            current.insert_node_direct(node, commit_timestamp)?;
        }

        wal.flush()?;

        // Create checkpoint at LSN 0 (without vector index) to ensure all node
        // creations are replayed from WAL during recovery.
        let checkpoint = Checkpoint::new(LSN(0), &current, &historical);
        std::fs::create_dir_all(&checkpoint_dir)?;
        let checkpoint_path = checkpoint_dir.join("checkpoint_000000.dat");
        checkpoint.save(&checkpoint_path)?;

        // Phase 2: Simulate crash
        drop(current);
        drop(historical);

        // Phase 3: Recovery should succeed without errors
        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Verify recovery succeeded
        assert_eq!(recovered_current.node_count(), 3);
        assert!(!recovered_current.is_vector_index_enabled());

        Ok(())
    }

    /// Test that vector index is rebuilt by iterating through recovered nodes.
    ///
    /// This test specifically verifies that the HNSW index is populated
    /// with all vectors during recovery, not just the configuration.
    #[test]
    fn test_recovery_rebuilds_vector_index() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use crate::storage::wal::WalOperation;
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let checkpoint_dir = temp_dir.path().join("checkpoints");

        // Phase 1: Create nodes with embeddings
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        let vector_config = HnswConfig::new(3, DistanceMetric::Euclidean).with_capacity(50);
        current.enable_vector_index("vec", vector_config)?;

        // Create 5 nodes with embeddings
        let embeddings = [
            vec![1.0f32, 0.0, 0.0],
            vec![0.0f32, 1.0, 0.0],
            vec![0.0f32, 0.0, 1.0],
            vec![0.5f32, 0.5, 0.0],
            vec![0.0f32, 0.5, 0.5],
        ];

        for (i, embedding) in embeddings.iter().enumerate() {
            let props = PropertyMapBuilder::new()
                .insert_vector("vec", embedding)
                .build();

            let node_id = NodeId::new(i as u64 + 1)?;
            let temporal = BiTemporalInterval::current(time::now());

            wal.append(WalOperation::CreateNode {
                node_id,
                label: GLOBAL_INTERNER.intern("Doc").unwrap(),
                properties: props.clone(),
                temporal,
            })?;

            let interned_label = GLOBAL_INTERNER.intern("Doc")?;
            let version_id = VersionId::new(i as u64 + 1)?;
            let commit_timestamp = temporal.transaction_time().start();
            let metadata = VersionMetadata::new(TxId::new(0), commit_timestamp);
            let node = Node::with_metadata(node_id, interned_label, props, version_id, metadata);
            current.insert_node_direct(node, commit_timestamp)?;
        }

        wal.flush()?;

        // Create checkpoint at LSN 0 to ensure all node creations are replayed from WAL.
        let checkpoint = Checkpoint::new(LSN(0), &current, &historical);
        std::fs::create_dir_all(&checkpoint_dir)?;
        checkpoint.save(&checkpoint_dir.join("checkpoint_000000.dat"))?;

        drop(current);
        drop(historical);

        // Phase 2: Recovery
        let config = CheckpointConfig {
            checkpoint_dir,
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Phase 3: Verify index is functional
        // Search should return results, meaning index was rebuilt
        let query = vec![1.0f32, 0.0, 0.0];
        let results = recovered_current.search_vectors_in("vec", &query, 3)?;

        assert!(
            !results.is_empty(),
            "Search should return results, proving index was rebuilt"
        );

        Ok(())
    }

    /// Test that recovery fails gracefully when vector index restoration fails.
    ///
    /// This test verifies the error handling strategy: if vector index restoration
    /// fails during recovery, the entire recovery operation should fail to maintain
    /// data integrity (rather than continuing with inconsistent state).
    #[test]
    fn test_recovery_fails_on_invalid_vector_index_config() -> Result<()> {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        let checkpoint_dir = temp_dir.path().join("checkpoints");

        // Phase 1: Create a checkpoint with INVALID vector config
        // We'll manually corrupt the checkpoint file to have dimensions=0 (invalid)
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Create valid checkpoint first
        let valid_config = HnswConfig::new(384, DistanceMetric::Cosine);
        current.enable_vector_index("embedding", valid_config)?;

        let checkpoint = Checkpoint::new(LSN(1), &current, &historical);
        std::fs::create_dir_all(&checkpoint_dir)?;
        let checkpoint_path = checkpoint_dir.join("checkpoint_000001.dat");
        checkpoint.save(&checkpoint_path)?;

        // Phase 2: Corrupt the checkpoint to have invalid dimensions (0)
        let mut data = std::fs::read(&checkpoint_path)?;

        // Find and corrupt the dimensions field in the binary format
        // Vector config format: enabled(1) + name_len(4) + name + HnswConfig
        // HnswConfig starts with dimensions (4 bytes)
        // We need to find where "embedding" string ends and set next 4 bytes to 0

        // Expected structure after magic/version/lsn/timestamp/counts:
        // - Magic (4) + Version (4) = 8
        // - LSN (8) + Timestamp (12) + NodeCount (8) + EdgeCount (8) + VersionCount (8) = 44
        // - enabled (1) + name_len (4) = 5
        // Total header before name = 8 + 44 + 5 = 57

        let name_start = 57;
        let name_len = u32::from_le_bytes([data[53], data[54], data[55], data[56]]) as usize;
        let dimensions_offset = name_start + name_len;

        // Corrupt dimensions to 0 (invalid)
        data[dimensions_offset..dimensions_offset + 4].copy_from_slice(&0u32.to_le_bytes());

        // Write corrupted checkpoint
        std::fs::write(&checkpoint_path, data)?;

        drop(current);
        drop(historical);

        // Phase 3: Attempt recovery - should FAIL due to invalid vector config
        let config = CheckpointConfig {
            checkpoint_dir,
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let result = manager.recover(&wal);

        // Verify recovery fails (maintains data integrity)
        match result {
            Ok(_) => panic!("Recovery should fail when vector index config is invalid"),
            Err(err) => {
                // Verify the error is related to vector configuration
                let err_str = err.to_string();
                assert!(
                    err_str.contains("dimension")
                        || err_str.contains("vector")
                        || err_str.contains("invalid"),
                    "Error should be related to invalid vector config, got: {}",
                    err_str
                );
            }
        }

        Ok(())
    }

    // ========================================================================
    // List Checkpoints Helper Tests (Issue #213)
    // ========================================================================

    /// Test that list_checkpoints() returns empty vector when directory is empty
    #[test]
    fn test_list_checkpoints_empty_directory() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };

        let manager = PersistenceManager::new(config)?;
        let checkpoints = manager.list_checkpoints()?;

        assert!(
            checkpoints.is_empty(),
            "Empty directory should return no checkpoints"
        );
        Ok(())
    }

    /// Test that list_checkpoints() filters checkpoint files correctly
    #[test]
    fn test_list_checkpoints_filters_correctly() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_dir = temp_dir.path();

        // Create some checkpoint files
        std::fs::write(checkpoint_dir.join("checkpoint_000001.dat"), b"test")?;
        std::fs::write(checkpoint_dir.join("checkpoint_000002.dat"), b"test")?;

        // Create some non-checkpoint files that should be ignored
        std::fs::write(checkpoint_dir.join("other_file.txt"), b"test")?;
        std::fs::write(checkpoint_dir.join("checkpoint.bak"), b"test")?;
        std::fs::write(checkpoint_dir.join("data.dat"), b"test")?;

        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.to_path_buf(),
            ..Default::default()
        };

        let manager = PersistenceManager::new(config)?;
        let checkpoints = manager.list_checkpoints()?;

        assert_eq!(
            checkpoints.len(),
            2,
            "Should find exactly 2 checkpoint files"
        );

        // Verify only checkpoint files are included
        for path in &checkpoints {
            let filename = path.file_name().unwrap().to_str().unwrap();
            assert!(filename.starts_with("checkpoint_"));
            assert!(filename.ends_with(".dat"));
        }

        Ok(())
    }

    /// Test that list_checkpoints() returns files sorted by filename
    #[test]
    fn test_list_checkpoints_sorted() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let checkpoint_dir = temp_dir.path();

        // Create checkpoint files in random order
        std::fs::write(checkpoint_dir.join("checkpoint_000003.dat"), b"test")?;
        std::fs::write(checkpoint_dir.join("checkpoint_000001.dat"), b"test")?;
        std::fs::write(checkpoint_dir.join("checkpoint_000005.dat"), b"test")?;
        std::fs::write(checkpoint_dir.join("checkpoint_000002.dat"), b"test")?;

        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.to_path_buf(),
            ..Default::default()
        };

        let manager = PersistenceManager::new(config)?;
        let checkpoints = manager.list_checkpoints()?;

        assert_eq!(checkpoints.len(), 4);

        // Verify files are sorted
        let filenames: Vec<String> = checkpoints
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();

        assert_eq!(filenames[0], "checkpoint_000001.dat");
        assert_eq!(filenames[1], "checkpoint_000002.dat");
        assert_eq!(filenames[2], "checkpoint_000003.dat");
        assert_eq!(filenames[3], "checkpoint_000005.dat");

        Ok(())
    }

    /// Test that list_checkpoints() handles directory read errors gracefully
    #[test]
    fn test_list_checkpoints_nonexistent_directory() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let nonexistent_dir = temp_dir.path().join("does_not_exist");

        let config = CheckpointConfig {
            checkpoint_dir: nonexistent_dir,
            ..Default::default()
        };

        // Note: PersistenceManager::new creates the directory, so we need to
        // delete it after creation to test the error case
        let manager = PersistenceManager::new(config.clone())?;
        std::fs::remove_dir(&config.checkpoint_dir)?;

        let checkpoints = manager.list_checkpoints()?;

        // Should return empty vector when directory doesn't exist
        assert!(
            checkpoints.is_empty(),
            "Should return empty vector when directory doesn't exist"
        );

        Ok(())
    }
}
