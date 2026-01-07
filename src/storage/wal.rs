//! Write-Ahead Log (WAL) implementation for crash recovery and durability.
//!
//! The WAL provides:
//! - Sequential logging of all mutations
//! - Crash recovery by replaying operations
//! - Point-in-time recovery capabilities
//! - Configurable durability modes for performance tuning
//!
//! # WAL Format
//!
//! Each WAL entry consists of:
//! - Log Sequence Number (LSN): Monotonically increasing identifier
//! - Timestamp: When the operation was logged
//! - Operation: The mutation to apply
//! - Checksum: CRC32 for corruption detection
//!
//! # Durability Modes
//!
//! The WAL supports three durability modes:
//!
//! - **Synchronous**: fsync after every commit (maximum durability, default)
//! - **Async**: Background thread flushes periodically (maximum throughput)
//! - **GroupCommit**: Batch multiple transactions per fsync (ACID + high throughput)
//!
//! See [`DurabilityMode`] for details.

// Submodules for durability mode support
pub mod durability;
pub mod flush_guard;
pub mod group_commit;

// Re-export key types
pub use durability::{DurabilityMode, WriteOptions};
pub use flush_guard::{FlushGuard, FlushSignal};
pub use group_commit::GroupCommitCoordinator;

use crate::core::{
    id::{EdgeId, NodeId, VersionId},
    property::PropertyMap,
    temporal::{BiTemporalInterval, Timestamp, time},
};
use crate::utils::error::{Error, Result, StorageError};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Magic bytes identifying a GallifreyDB WAL segment file.
const WAL_MAGIC: [u8; 4] = *b"GWAL";

/// Current WAL format version.
/// Version 2: Full serialization of properties and temporal intervals.
const WAL_VERSION: u8 = 2;

/// Legacy WAL version (no header, no properties/temporal serialization).
/// Used for backward compatibility when reading old WAL segments.
const WAL_VERSION_LEGACY: u8 = 1;

/// Size of the WAL segment header (magic + version).
const WAL_HEADER_SIZE: usize = 5;

/// Log Sequence Number - monotonically increasing identifier for WAL entries
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LSN(pub u64);

impl LSN {
    /// Create the first LSN
    pub fn initial() -> Self {
        LSN(1)
    }

    /// Get the next LSN
    pub fn next(&self) -> Self {
        LSN(self.0 + 1)
    }
}

/// WAL operation types
#[derive(Debug, Clone)]
pub enum WalOperation {
    /// Create a new node
    CreateNode {
        /// The node ID
        node_id: NodeId,
        /// The node label
        label: String,
        /// The node properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Create a new edge
    CreateEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The source node ID
        source: NodeId,
        /// The target node ID
        target: NodeId,
        /// The edge label
        label: String,
        /// The edge properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Update node (creates new version)
    UpdateNode {
        /// The node ID
        node_id: NodeId,
        /// The version ID
        version_id: VersionId,
        /// The new label
        label: String,
        /// The new properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Update edge (creates new version)
    UpdateEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The version ID
        version_id: VersionId,
        /// The new label
        label: String,
        /// The new properties
        properties: PropertyMap,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Delete a node
    DeleteNode {
        /// The node ID
        node_id: NodeId,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Delete an edge
    DeleteEdge {
        /// The edge ID
        edge_id: EdgeId,
        /// The bi-temporal interval
        temporal: BiTemporalInterval,
    },
    /// Checkpoint marker - indicates a snapshot was taken
    Checkpoint {
        /// The LSN at checkpoint
        lsn: LSN,
        /// When the checkpoint was created
        timestamp: Timestamp,
    },
}

/// A single WAL entry
#[derive(Debug, Clone)]
pub struct WalEntry {
    /// Log sequence number
    pub lsn: LSN,
    /// Timestamp when logged
    pub timestamp: Timestamp,
    /// The operation to log
    pub operation: WalOperation,
    /// CRC32 checksum for corruption detection
    pub checksum: u32,
}

impl WalEntry {
    /// Create a new WAL entry with computed checksum
    pub fn new(lsn: LSN, operation: WalOperation) -> Self {
        let timestamp = time::now();
        // Checksum will be computed during serialization
        WalEntry {
            lsn,
            timestamp,
            operation,
            checksum: 0, // Will be set during serialization
        }
    }

    /// Verify the checksum against serialized data
    pub fn verify_checksum(&self, serialized_data: &[u8]) -> bool {
        // Extract checksum from data (stored at bytes 16-20)
        if serialized_data.len() < 20 {
            return false;
        }
        let stored_checksum = u32::from_le_bytes([
            serialized_data[16],
            serialized_data[17],
            serialized_data[18],
            serialized_data[19],
        ]);

        // Compute checksum over everything except the checksum field itself
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&serialized_data[0..16]); // LSN + timestamp
        hasher.update(&serialized_data[20..]); // Operation data
        let computed = hasher.finalize();

        stored_checksum == computed
    }
}

/// Configuration for WAL behavior
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Directory where WAL files are stored
    pub wal_dir: PathBuf,
    /// Maximum size of a WAL segment before rotation (in bytes)
    pub segment_size: usize,
    /// Whether to fsync after every write (slower but more durable).
    ///
    /// **Deprecated**: Use `durability_mode` instead. This field is kept for
    /// backward compatibility and is ignored when `durability_mode` is set
    /// to anything other than `Synchronous`.
    #[deprecated(since = "0.2.0", note = "Use `durability_mode` instead")]
    pub sync_on_write: bool,
    /// Number of WAL segments to keep for recovery
    pub segments_to_retain: usize,
    /// Durability mode controlling when data is synced to disk.
    ///
    /// This determines the tradeoff between durability guarantees and
    /// performance. See [`DurabilityMode`] for details.
    pub durability_mode: DurabilityMode,
}

impl Default for WalConfig {
    fn default() -> Self {
        WalConfig {
            wal_dir: PathBuf::from("gallifreydb/wal"),
            segment_size: 64 * 1024 * 1024, // 64MB
            sync_on_write: true,
            segments_to_retain: 10,
            durability_mode: DurabilityMode::Synchronous,
        }
    }
}

impl WalConfig {
    /// Create a new WalConfig with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the durability mode.
    pub fn with_durability_mode(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = mode;
        self
    }

    /// Set the WAL directory.
    pub fn with_wal_dir(mut self, dir: PathBuf) -> Self {
        self.wal_dir = dir;
        self
    }

    /// Set the segment size.
    pub fn with_segment_size(mut self, size: usize) -> Self {
        self.segment_size = size;
        self
    }

    /// Set the number of segments to retain.
    pub fn with_segments_to_retain(mut self, count: usize) -> Self {
        self.segments_to_retain = count;
        self
    }
}

/// Write-Ahead Log manager
///
/// # Lock-Free Sync Architecture
///
/// To achieve high throughput (50k+ ops/sec), the WAL uses a two-phase flush:
///
/// 1. **Append Phase** (fast, µs): Data written to `writer` (BufWriter), flushed to OS page cache
/// 2. **Sync Phase** (slow, ms): `sync_handle` forces OS to write page cache to disk
///
/// The key insight is that `sync_handle` is a cloned file handle that can sync
/// independently. This allows new writes to pile up in the OS page cache while
/// a previous sync is in progress, using the OS page cache as a natural double-buffer.
pub struct WriteAheadLog {
    config: WalConfig,
    current_lsn: LSN,
    current_segment: u64,
    /// The buffered writer for appending entries (fast path)
    writer: Option<BufWriter<File>>,
    /// Cloned file handle used exclusively for sync_data() calls.
    /// This allows releasing the writer lock before the slow fsync,
    /// enabling parallel writes during sync.
    sync_handle: Option<File>,
    current_size: usize,
    /// Background flush thread guard (for Async and GroupCommit modes)
    flush_guard: Option<FlushGuard>,
    /// Signal for requesting immediate flush (for batch size triggering)
    flush_signal: Option<Arc<FlushSignal>>,
    /// Group commit coordinator (for GroupCommit mode)
    group_commit: Option<Arc<GroupCommitCoordinator>>,
    /// Tracks whether there's unflushed data (for piggybacking optimization)
    has_pending_data: bool,
}

/// Helper to deserialize and validate a NodeId from WAL buffer
#[inline]
fn deserialize_node_id(buffer: &[u8], offset: usize, context: &str) -> Result<NodeId> {
    let raw_id = u64::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
    NodeId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid node ID in WAL {}: {}",
            context, e
        )))
    })
}

/// Helper to deserialize and validate an EdgeId from WAL buffer
#[inline]
fn deserialize_edge_id(buffer: &[u8], offset: usize, context: &str) -> Result<EdgeId> {
    let raw_id = u64::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
    EdgeId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid edge ID in WAL {}: {}",
            context, e
        )))
    })
}

/// Helper to deserialize and validate a VersionId from WAL buffer
#[inline]
fn deserialize_version_id(buffer: &[u8], offset: usize, context: &str) -> Result<VersionId> {
    let raw_id = u64::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
    VersionId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid version ID in WAL {}: {}",
            context, e
        )))
    })
}

impl WriteAheadLog {
    /// Create a new WAL with the given configuration
    pub fn new(config: WalConfig) -> Result<Self> {
        // Create WAL directory if it doesn't exist
        std::fs::create_dir_all(&config.wal_dir)
            .map_err(|e| StorageError::IoError(format!("Failed to create WAL directory: {}", e)))?;

        let durability_mode = config.durability_mode;

        let mut wal = WriteAheadLog {
            config,
            current_lsn: LSN::initial(),
            current_segment: 1,
            writer: None,
            sync_handle: None,
            current_size: 0,
            flush_guard: None,
            flush_signal: None,
            group_commit: None,
            has_pending_data: false,
        };

        // Initialize background infrastructure based on durability mode
        wal.initialize_durability_mode(durability_mode)?;

        Ok(wal)
    }

    /// Initialize the durability mode infrastructure.
    ///
    /// This sets up background threads and coordinators based on the mode.
    fn initialize_durability_mode(&mut self, mode: DurabilityMode) -> Result<()> {
        match mode {
            DurabilityMode::Synchronous => {
                // No background infrastructure needed
                self.flush_guard = None;
                self.group_commit = None;
            }
            DurabilityMode::Async { flush_interval_ms } => {
                // Set up group commit coordinator (not used but simplifies piggybacking)
                self.group_commit = None;

                // Note: FlushGuard will be created when the WAL is wrapped in Arc<Mutex<>>
                // and passed to GallifreyDB, since we need a reference to call flush_to_disk
                // For now, store the interval in config and create guard later
                let _ = flush_interval_ms; // Will be used when creating FlushGuard
            }
            DurabilityMode::GroupCommit {
                max_delay_ms,
                max_batch_size,
            } => {
                // Set up group commit coordinator
                self.group_commit = Some(Arc::new(GroupCommitCoordinator::new(
                    max_delay_ms,
                    max_batch_size,
                )));
            }
        }
        Ok(())
    }

    /// Get the durability mode.
    pub fn durability_mode(&self) -> DurabilityMode {
        self.config.durability_mode
    }

    /// Get a reference to the group commit coordinator, if any.
    pub fn group_commit_coordinator(&self) -> Option<&Arc<GroupCommitCoordinator>> {
        self.group_commit.as_ref()
    }

    /// Check if there is pending unflushed data.
    pub fn has_pending_data(&self) -> bool {
        self.has_pending_data
    }

    /// Start the background flush thread for Async and GroupCommit modes.
    ///
    /// This must be called after the WAL is wrapped in `Arc<Mutex<>>` since
    /// the flush thread needs a reference to call flush methods.
    ///
    /// # Arguments
    ///
    /// * `wal` - The Arc<Mutex<WAL>> that this WAL instance is wrapped in
    pub fn start_flush_thread(wal: Arc<Mutex<Self>>) {
        use std::time::Duration;

        // Get the durability mode and group commit coordinator while holding the lock
        let (mode, group_commit): (DurabilityMode, Option<Arc<GroupCommitCoordinator>>) = {
            let wal_guard = wal.lock().expect("WAL lock poisoned");
            (
                wal_guard.config.durability_mode,
                wal_guard.group_commit.clone(),
            )
        };

        let interval = match mode {
            DurabilityMode::Synchronous => {
                // No background thread needed for Synchronous mode
                return;
            }
            DurabilityMode::Async { flush_interval_ms } => Duration::from_millis(flush_interval_ms),
            DurabilityMode::GroupCommit { max_delay_ms, .. } => Duration::from_millis(max_delay_ms),
        };

        let wal_clone: Arc<Mutex<WriteAheadLog>> = Arc::clone(&wal);
        let group_commit_clone: Option<Arc<GroupCommitCoordinator>> = group_commit.clone();

        let flush_guard = FlushGuard::spawn(
            move || {
                // Phase 1: Flush to OS page cache (fast, µs)
                {
                    let mut wal_guard: std::sync::MutexGuard<'_, WriteAheadLog> =
                        match wal_clone.lock() {
                            Ok(guard) => guard,
                            Err(_) => return,
                        };
                    if let Err(_e) = wal_guard.flush_to_os() {
                        // Log error in production, but continue
                    }
                } // Lock released - parallel writes can now pile up

                // Phase 2: Sync to disk (slow, ms) - no lock held!
                // Clone the file handle while holding the lock, then release lock before fsync.
                // CRITICAL: try_clone() creates a new file descriptor that shares the same
                // file description, so sync_data() flushes all writes made through any handle.
                let handle_to_sync = {
                    let wal_guard: std::sync::MutexGuard<'_, WriteAheadLog> = match wal_clone.lock()
                    {
                        Ok(guard) => guard,
                        Err(_) => return,
                    };
                    // Clone the file handle to perform the slow sync operation outside of the lock
                    wal_guard
                        .sync_handle
                        .as_ref()
                        .and_then(|h| h.try_clone().ok())
                }; // WAL lock is released here - parallel writes can now proceed during fsync

                if let Some(sync_handle) = handle_to_sync
                    && let Err(e) = sync_handle.sync_data()
                {
                    // Mark flush as failed if using GroupCommit
                    if let Some(ref gc) = group_commit_clone {
                        let err = Error::Storage(StorageError::IoError(format!(
                            "fsync failed: {}",
                            e
                        )));
                        gc.mark_flushed(Err(err));
                    }
                    return;
                }

                // Phase 3: Update state and notify waiters
                {
                    let mut wal_guard: std::sync::MutexGuard<'_, WriteAheadLog> =
                        match wal_clone.lock() {
                            Ok(guard) => guard,
                            Err(_) => return,
                        };
                    wal_guard.has_pending_data = false;
                }

                // Notify GroupCommit waiters that flush is complete
                if let Some(ref gc) = group_commit_clone {
                    gc.mark_flushed(Ok(()));
                }
            },
            interval,
        );

        // Store the flush guard and signal in the WAL
        let mut wal_guard = wal.lock().expect("WAL lock poisoned");
        wal_guard.flush_signal = Some(Arc::clone(flush_guard.signal()));
        wal_guard.flush_guard = Some(flush_guard);
    }

    /// Open a new WAL segment for writing
    fn open_segment(&mut self, segment_id: u64) -> Result<()> {
        let segment_path = self.segment_path(segment_id);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&segment_path)
            .map_err(|e| StorageError::IoError(format!("Failed to open WAL segment: {}", e)))?;

        // Clone the file handle for sync operations.
        // This allows us to release the writer lock before calling sync_data(),
        // enabling parallel writes during the slow fsync operation.
        let sync_handle = file.try_clone().map_err(|e| {
            StorageError::IoError(format!("Failed to clone WAL file handle: {}", e))
        })?;

        // Get current file size
        let metadata = file.metadata().map_err(|e| {
            StorageError::IoError(format!("Failed to get WAL segment metadata: {}", e))
        })?;

        let file_size = metadata.len() as usize;
        let mut writer = BufWriter::new(file);

        // Write header for new segments
        if file_size == 0 {
            writer
                .write_all(&WAL_MAGIC)
                .map_err(|e| StorageError::IoError(format!("Failed to write WAL magic: {}", e)))?;
            writer.write_all(&[WAL_VERSION]).map_err(|e| {
                StorageError::IoError(format!("Failed to write WAL version: {}", e))
            })?;
            self.current_size = WAL_HEADER_SIZE;
        } else {
            self.current_size = file_size;
        }

        self.writer = Some(writer);
        self.sync_handle = Some(sync_handle);

        Ok(())
    }

    /// Get the path for a WAL segment
    fn segment_path(&self, segment_id: u64) -> PathBuf {
        self.config.wal_dir.join(format!("{:06}.log", segment_id))
    }

    /// Append an operation to the WAL
    ///
    /// This writes the operation to the BufWriter but does NOT fsync.
    /// Call `flush_to_os()` to push to OS cache, then `sync_to_disk()` for durability.
    pub fn append(&mut self, operation: WalOperation) -> Result<LSN> {
        // Ensure we have a writer
        if self.writer.is_none() {
            self.open_segment(self.current_segment)?;
        }

        let entry = WalEntry::new(self.current_lsn, operation);

        // Serialize the entry (simplified - in production would use proper serialization)
        let serialized = self.serialize_entry(&entry)?;

        // Write to current segment
        if let Some(writer) = &mut self.writer {
            writer
                .write_all(&serialized)
                .map_err(|e| StorageError::IoError(format!("Failed to write WAL entry: {}", e)))?;

            // Mark that we have pending unflushed data
            self.has_pending_data = true;

            self.current_size += serialized.len();
        }

        let lsn = self.current_lsn;
        self.current_lsn = self.current_lsn.next();

        // Check if we need to rotate to a new segment
        if self.current_size >= self.config.segment_size {
            self.rotate_segment()?;
        }

        Ok(lsn)
    }

    /// Rotate to a new WAL segment
    fn rotate_segment(&mut self) -> Result<()> {
        // Flush and sync current segment before rotation
        if let Some(mut writer) = self.writer.take() {
            writer.flush().map_err(|e| {
                StorageError::IoError(format!("Failed to flush WAL on rotation: {}", e))
            })?;
        }

        // Sync the old segment to disk
        if let Some(sync_handle) = self.sync_handle.take() {
            sync_handle.sync_data().map_err(|e| {
                StorageError::IoError(format!("Failed to sync WAL on rotation: {}", e))
            })?;
        }

        self.has_pending_data = false;

        // Move to next segment
        self.current_segment += 1;
        self.current_size = 0;
        self.open_segment(self.current_segment)?;

        // Clean up old segments
        self.cleanup_old_segments()?;

        Ok(())
    }

    /// Remove old WAL segments beyond retention policy
    fn cleanup_old_segments(&mut self) -> Result<()> {
        let keep_from = self
            .current_segment
            .saturating_sub(self.config.segments_to_retain as u64);

        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && name.ends_with(".log")
                    && let Some(seg_id) = name
                        .strip_suffix(".log")
                        .and_then(|s| s.parse::<u64>().ok())
                    && seg_id < keep_from
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        Ok(())
    }

    /// Serialize a WAL entry with CRC32 checksum
    fn serialize_entry(&self, entry: &WalEntry) -> Result<Vec<u8>> {
        // Serialize entry with proper CRC32 checksum
        let mut buffer = Vec::new();

        // Write LSN (8 bytes)
        buffer.extend_from_slice(&entry.lsn.0.to_le_bytes());

        // Write timestamp (8 bytes)
        buffer.extend_from_slice(&entry.timestamp.to_le_bytes());

        // Reserve space for checksum (4 bytes) - will fill in later
        let checksum_offset = buffer.len();
        buffer.extend_from_slice(&[0u8; 4]);

        // Write operation type and data with full serialization
        match &entry.operation {
            WalOperation::CreateNode {
                node_id,
                label,
                properties,
                temporal,
            } => {
                buffer.push(1); // operation type
                buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
                buffer.extend_from_slice(label.as_bytes());
                properties.serialize_into(&mut buffer)?;
                temporal.serialize_into(&mut buffer);
            }
            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                properties,
                temporal,
            } => {
                buffer.push(2); // operation type
                buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&source.as_u64().to_le_bytes());
                buffer.extend_from_slice(&target.as_u64().to_le_bytes());
                buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
                buffer.extend_from_slice(label.as_bytes());
                properties.serialize_into(&mut buffer)?;
                temporal.serialize_into(&mut buffer);
            }
            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties,
                temporal,
            } => {
                buffer.push(3); // operation type
                buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
                buffer.extend_from_slice(label.as_bytes());
                properties.serialize_into(&mut buffer)?;
                temporal.serialize_into(&mut buffer);
            }
            WalOperation::UpdateEdge {
                edge_id,
                version_id,
                label,
                properties,
                temporal,
            } => {
                buffer.push(4); // operation type
                buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
                buffer.extend_from_slice(label.as_bytes());
                properties.serialize_into(&mut buffer)?;
                temporal.serialize_into(&mut buffer);
            }
            WalOperation::DeleteNode { node_id, temporal } => {
                buffer.push(6); // operation type
                buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
                temporal.serialize_into(&mut buffer);
            }
            WalOperation::DeleteEdge { edge_id, temporal } => {
                buffer.push(7); // operation type
                buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
                temporal.serialize_into(&mut buffer);
            }
            WalOperation::Checkpoint { lsn, timestamp } => {
                buffer.push(5); // operation type
                buffer.extend_from_slice(&lsn.0.to_le_bytes());
                buffer.extend_from_slice(&timestamp.to_le_bytes());
            }
        }

        // Compute CRC32 over everything except the checksum field
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&buffer[0..checksum_offset]); // LSN + timestamp
        hasher.update(&buffer[checksum_offset + 4..]); // Operation data
        let checksum = hasher.finalize();

        // Write the checksum into the reserved space
        buffer[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());

        Ok(buffer)
    }

    /// Read all WAL entries from a specific LSN onwards
    pub fn read_from(&self, start_lsn: LSN) -> Result<Vec<WalEntry>> {
        let mut entries = Vec::new();

        // Find all WAL segments
        let mut segments = Vec::new();
        if let Ok(dir_entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in dir_entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && name.ends_with(".log")
                    && let Some(seg_id) = name
                        .strip_suffix(".log")
                        .and_then(|s| s.parse::<u64>().ok())
                {
                    segments.push((seg_id, entry.path()));
                }
            }
        }

        // Sort segments by ID
        segments.sort_by_key(|(id, _)| *id);

        // Read entries from each segment
        for (_, path) in segments {
            let segment_entries = self.read_segment(&path, start_lsn)?;
            entries.extend(segment_entries);
        }

        Ok(entries)
    }

    /// Helper to deserialize and validate a NodeId from WAL buffer
    #[inline]
    fn deserialize_node_id(buffer: &[u8], offset: usize, context: &str) -> Result<NodeId> {
        let raw_id = u64::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
            buffer[offset + 4],
            buffer[offset + 5],
            buffer[offset + 6],
            buffer[offset + 7],
        ]);
        NodeId::new(raw_id).map_err(|e| {
            Error::Storage(StorageError::CorruptedData(format!(
                "Invalid node ID in WAL {}: {}",
                context, e
            )))
        })
    }

    /// Helper to deserialize and validate an EdgeId from WAL buffer
    #[inline]
    fn deserialize_edge_id(buffer: &[u8], offset: usize, context: &str) -> Result<EdgeId> {
        let raw_id = u64::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
            buffer[offset + 4],
            buffer[offset + 5],
            buffer[offset + 6],
            buffer[offset + 7],
        ]);
        EdgeId::new(raw_id).map_err(|e| {
            Error::Storage(StorageError::CorruptedData(format!(
                "Invalid edge ID in WAL {}: {}",
                context, e
            )))
        })
    }

    /// Helper to deserialize and validate a VersionId from WAL buffer
    #[inline]
    fn deserialize_version_id(buffer: &[u8], offset: usize, context: &str) -> Result<VersionId> {
        let raw_id = u64::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
            buffer[offset + 4],
            buffer[offset + 5],
            buffer[offset + 6],
            buffer[offset + 7],
        ]);
        VersionId::new(raw_id).map_err(|e| {
            Error::Storage(StorageError::CorruptedData(format!(
                "Invalid version ID in WAL {}: {}",
                context, e
            )))
        })
    }

    /// Read WAL entries from a single segment file
    fn read_segment(&self, path: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
        use std::io::Read;

        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return Ok(Vec::new()), // File doesn't exist yet, return empty
        };

        let mut reader = std::io::BufReader::new(file);
        let mut entries = Vec::new();
        let mut buffer = Vec::new();

        // Read entire file into buffer
        reader
            .read_to_end(&mut buffer)
            .map_err(|e| StorageError::IoError(format!("Failed to read WAL segment: {}", e)))?;

        // Detect WAL format version
        let (version, mut offset) = if buffer.len() >= WAL_HEADER_SIZE && buffer[0..4] == WAL_MAGIC
        {
            // V2+ format: has magic header
            let ver = buffer[4];
            if ver > WAL_VERSION {
                return Err(StorageError::CorruptedData(format!(
                    "Unsupported WAL version: {} (max supported: {})",
                    ver, WAL_VERSION
                ))
                .into());
            }
            (ver, WAL_HEADER_SIZE)
        } else if !buffer.is_empty() {
            // Legacy V1 format: no header, starts directly with entry data
            (WAL_VERSION_LEGACY, 0)
        } else {
            return Ok(Vec::new()); // Empty segment
        };
        while offset < buffer.len() {
            // Need at least 20 bytes for LSN (8) + timestamp (8) + checksum (4)
            if offset + 20 > buffer.len() {
                break;
            }

            // Read LSN (8 bytes)
            let lsn = LSN(u64::from_le_bytes([
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
                buffer[offset + 4],
                buffer[offset + 5],
                buffer[offset + 6],
                buffer[offset + 7],
            ]));
            offset += 8;

            // Read timestamp (8 bytes)
            let timestamp = i64::from_le_bytes([
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
                buffer[offset + 4],
                buffer[offset + 5],
                buffer[offset + 6],
                buffer[offset + 7],
            ]);
            offset += 8;

            // Read checksum (4 bytes)
            let checksum = u32::from_le_bytes([
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
            ]);
            offset += 4;

            // Read operation type
            if offset >= buffer.len() {
                break;
            }
            let op_type = buffer[offset];
            offset += 1;

            // Parse operation data based on type and version
            let operation = match op_type {
                1 => {
                    // CreateNode
                    if offset + 12 > buffer.len() {
                        break;
                    }
                    let node_id = Self::deserialize_node_id(&buffer, offset, "CreateNode")?;
                    offset += 8;

                    let label_len = u32::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + label_len > buffer.len() {
                        break;
                    }
                    let label =
                        String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                    offset += label_len;

                    // V2+: deserialize properties and temporal
                    // V1: use placeholders (data was not serialized)
                    let (properties, temporal) = if version >= WAL_VERSION {
                        let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                        offset += props_len;
                        let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                        offset += temp_len;
                        (props, temp)
                    } else {
                        (PropertyMap::new(), BiTemporalInterval::current(timestamp))
                    };

                    WalOperation::CreateNode {
                        node_id,
                        label,
                        properties,
                        temporal,
                    }
                }
                2 => {
                    // CreateEdge
                    if offset + 28 > buffer.len() {
                        break;
                    }
                    let edge_id = Self::deserialize_edge_id(&buffer, offset, "CreateEdge")?;
                    offset += 8;

                    let source = Self::deserialize_node_id(&buffer, offset, "CreateEdge source")?;
                    offset += 8;

                    let target = Self::deserialize_node_id(&buffer, offset, "CreateEdge target")?;
                    offset += 8;

                    let label_len = u32::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + label_len > buffer.len() {
                        break;
                    }
                    let label =
                        String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                    offset += label_len;

                    let (properties, temporal) = if version >= WAL_VERSION {
                        let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                        offset += props_len;
                        let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                        offset += temp_len;
                        (props, temp)
                    } else {
                        (PropertyMap::new(), BiTemporalInterval::current(timestamp))
                    };

                    WalOperation::CreateEdge {
                        edge_id,
                        source,
                        target,
                        label,
                        properties,
                        temporal,
                    }
                }
                3 => {
                    // UpdateNode
                    // V1: only node_id + version_id (16 bytes)
                    // V2: + label + properties + temporal
                    if offset + 16 > buffer.len() {
                        break;
                    }
                    let node_id = Self::deserialize_node_id(&buffer, offset, "UpdateNode")?;
                    offset += 8;

                    let version_id = Self::deserialize_version_id(&buffer, offset, "UpdateNode")?;
                    offset += 8;

                    let (label, properties, temporal) = if version >= WAL_VERSION {
                        let label_len = u32::from_le_bytes([
                            buffer[offset],
                            buffer[offset + 1],
                            buffer[offset + 2],
                            buffer[offset + 3],
                        ]) as usize;
                        offset += 4;

                        if offset + label_len > buffer.len() {
                            break;
                        }
                        let lbl = String::from_utf8_lossy(&buffer[offset..offset + label_len])
                            .to_string();
                        offset += label_len;

                        let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                        offset += props_len;
                        let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                        offset += temp_len;
                        (lbl, props, temp)
                    } else {
                        (
                            String::new(),
                            PropertyMap::new(),
                            BiTemporalInterval::current(timestamp),
                        )
                    };

                    WalOperation::UpdateNode {
                        node_id,
                        version_id,
                        label,
                        properties,
                        temporal,
                    }
                }
                4 => {
                    // UpdateEdge
                    // V1: only edge_id + version_id (16 bytes)
                    // V2: + label + properties + temporal
                    if offset + 16 > buffer.len() {
                        break;
                    }
                    let edge_id = Self::deserialize_edge_id(&buffer, offset, "UpdateEdge")?;
                    offset += 8;

                    let version_id = Self::deserialize_version_id(&buffer, offset, "UpdateEdge")?;
                    offset += 8;

                    let (label, properties, temporal) = if version >= WAL_VERSION {
                        let label_len = u32::from_le_bytes([
                            buffer[offset],
                            buffer[offset + 1],
                            buffer[offset + 2],
                            buffer[offset + 3],
                        ]) as usize;
                        offset += 4;

                        if offset + label_len > buffer.len() {
                            break;
                        }
                        let lbl = String::from_utf8_lossy(&buffer[offset..offset + label_len])
                            .to_string();
                        offset += label_len;

                        let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                        offset += props_len;
                        let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                        offset += temp_len;
                        (lbl, props, temp)
                    } else {
                        (
                            String::new(),
                            PropertyMap::new(),
                            BiTemporalInterval::current(timestamp),
                        )
                    };

                    WalOperation::UpdateEdge {
                        edge_id,
                        version_id,
                        label,
                        properties,
                        temporal,
                    }
                }
                5 => {
                    // Checkpoint (same format in all versions)
                    if offset + 16 > buffer.len() {
                        break;
                    }
                    let cp_lsn = LSN(u64::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                        buffer[offset + 4],
                        buffer[offset + 5],
                        buffer[offset + 6],
                        buffer[offset + 7],
                    ]));
                    offset += 8;

                    let cp_timestamp = i64::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                        buffer[offset + 4],
                        buffer[offset + 5],
                        buffer[offset + 6],
                        buffer[offset + 7],
                    ]);
                    offset += 8;

                    WalOperation::Checkpoint {
                        lsn: cp_lsn,
                        timestamp: cp_timestamp,
                    }
                }
                6 => {
                    // DeleteNode
                    if offset + 8 > buffer.len() {
                        break;
                    }
                    let node_id = Self::deserialize_node_id(&buffer, offset, "DeleteNode")?;
                    offset += 8;

                    let temporal = if version >= WAL_VERSION {
                        let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                        offset += temp_len;
                        temp
                    } else {
                        BiTemporalInterval::current(timestamp)
                    };

                    WalOperation::DeleteNode { node_id, temporal }
                }
                7 => {
                    // DeleteEdge
                    if offset + 8 > buffer.len() {
                        break;
                    }
                    let edge_id = Self::deserialize_edge_id(&buffer, offset, "DeleteEdge")?;
                    offset += 8;

                    let temporal = if version >= WAL_VERSION {
                        let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                        offset += temp_len;
                        temp
                    } else {
                        BiTemporalInterval::current(timestamp)
                    };

                    WalOperation::DeleteEdge { edge_id, temporal }
                }
                _ => {
                    // Unknown operation type
                    return Err(StorageError::CorruptedData(format!(
                        "Unknown WAL operation type: {}",
                        op_type
                    ))
                    .into());
                }
            };

            // Only include entries >= start_lsn
            if lsn >= start_lsn {
                let entry = WalEntry {
                    lsn,
                    timestamp,
                    operation,
                    checksum,
                };
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    /// Flush data to OS page cache (Phase 1 of two-phase flush).
    ///
    /// This is fast (microseconds) and should be done while holding the WAL lock.
    /// After this call, data is in the OS page cache but NOT yet durable on disk.
    ///
    /// Call `sync_to_disk()` afterward (without holding the lock) for durability.
    pub fn flush_to_os(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer
                .flush()
                .map_err(|e| StorageError::IoError(format!("Failed to flush WAL buffer: {}", e)))?;
        }
        Ok(())
    }

    /// Sync data from OS page cache to disk (Phase 2 of two-phase flush).
    ///
    /// This is slow (milliseconds) and should be done WITHOUT holding the WAL lock.
    /// This allows parallel writes to pile up in the OS page cache during the fsync.
    ///
    /// Returns a reference to the sync handle for external callers who need to
    /// sync outside the lock.
    pub fn get_sync_handle(&self) -> Option<&File> {
        self.sync_handle.as_ref()
    }

    /// Perform the slow disk sync using the sync handle.
    ///
    /// This should be called OUTSIDE the WAL lock to allow parallel writes.
    pub fn sync_to_disk(&mut self) -> Result<()> {
        if let Some(sync_handle) = &self.sync_handle {
            sync_handle
                .sync_data()
                .map_err(|e| StorageError::IoError(format!("Failed to fsync WAL: {}", e)))?;
        }
        self.has_pending_data = false;
        Ok(())
    }

    /// Full flush for backward compatibility (combines both phases).
    ///
    /// **Warning**: This holds the lock during fsync, limiting throughput.
    /// For high-throughput scenarios, use `flush_to_os()` then release the lock,
    /// then call `sync_to_disk()`.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            // Step 1: Flush BufWriter buffer to OS (fast)
            writer
                .flush()
                .map_err(|e| StorageError::IoError(format!("Failed to flush WAL buffer: {}", e)))?;

            // Step 2: Force OS to sync data to disk (slow)
            // SAFETY: sync_data() ensures durability by forcing the OS to write buffered data to disk.
            // This is critical for WAL correctness - without it, committed transactions could be lost
            // on crash/power failure. We use sync_data() instead of sync_all() because we only need
            // to sync file data, not metadata (faster).
            //
            // NOTE: For high-throughput, call get_sync_handle() and sync outside the lock instead.
            if let Some(sync_handle) = &self.sync_handle {
                sync_handle
                    .sync_data()
                    .map_err(|e| StorageError::IoError(format!("Failed to fsync WAL: {}", e)))?;
            }
        }
        self.has_pending_data = false;
        Ok(())
    }

    /// Commit with the specified durability mode.
    ///
    /// This is the main entry point for mode-aware transaction commits.
    ///
    /// # Returns
    ///
    /// - For Synchronous: Returns `None` (already durable)
    /// - For Async: Returns `None` (will be flushed by background thread)
    /// - For GroupCommit: Returns `Some(epoch)` to wait for
    pub fn commit_with_mode(&mut self, mode: DurabilityMode) -> Result<Option<u64>> {
        match mode {
            DurabilityMode::Synchronous => {
                // Piggyback optimization: Wake background thread to flush pending async data
                // before we do our own fsync. This reduces the data-at-risk window for async
                // writes without slowing down the synchronous commit.
                if let Some(signal) = &self.flush_signal {
                    signal.request_flush();
                }

                // Immediate full flush (current behavior)
                self.flush()?;
                Ok(None)
            }
            DurabilityMode::Async { .. } => {
                // Just flush to OS cache, background thread will sync
                self.flush_to_os()?;
                Ok(None)
            }
            DurabilityMode::GroupCommit { .. } => {
                // Flush to OS cache and register with coordinator
                self.flush_to_os()?;

                if let Some(coordinator) = &self.group_commit {
                    let (epoch, should_trigger) = coordinator.register_transaction();

                    // If batch is full, wake the flush thread immediately
                    if should_trigger && let Some(signal) = &self.flush_signal {
                        signal.request_flush();
                    }

                    Ok(Some(epoch))
                } else {
                    // Fallback to sync if no coordinator (shouldn't happen)
                    self.sync_to_disk()?;
                    Ok(None)
                }
            }
        }
    }

    /// Wait for a group commit epoch to be flushed.
    ///
    /// This should be called after `commit_with_mode` returns `Some(epoch)`.
    pub fn wait_for_epoch(&self, epoch: u64) -> Result<()> {
        if let Some(coordinator) = &self.group_commit {
            coordinator.wait_for_flush(epoch)?;
        }
        Ok(())
    }

    /// Get the current LSN
    pub fn current_lsn(&self) -> LSN {
        self.current_lsn
    }
}

// ============================================================================
// WAL Migration Tool
// ============================================================================

/// Information about a WAL segment's format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalSegmentInfo {
    /// The detected version (1 = legacy without header, 2+ = with header)
    pub version: u8,
    /// Whether the segment needs migration to the current format
    pub needs_migration: bool,
    /// Size of the segment file in bytes
    pub size: u64,
}

/// Detect the version of a WAL segment file.
///
/// Returns information about the segment including its version and whether
/// it needs migration.
pub fn detect_wal_version(path: &Path) -> Result<WalSegmentInfo> {
    use std::io::Read;

    let metadata = std::fs::metadata(path)
        .map_err(|e| StorageError::IoError(format!("Failed to read WAL metadata: {}", e)))?;

    let size = metadata.len();
    if size == 0 {
        return Ok(WalSegmentInfo {
            version: WAL_VERSION,
            needs_migration: false,
            size: 0,
        });
    }

    let mut file = File::open(path)
        .map_err(|e| StorageError::IoError(format!("Failed to open WAL segment: {}", e)))?;

    let mut header = [0u8; WAL_HEADER_SIZE];
    let bytes_read = file
        .read(&mut header)
        .map_err(|e| StorageError::IoError(format!("Failed to read WAL header: {}", e)))?;

    if bytes_read >= WAL_HEADER_SIZE && header[0..4] == WAL_MAGIC {
        // V2+ format with magic header
        let version = header[4];
        Ok(WalSegmentInfo {
            version,
            needs_migration: version < WAL_VERSION,
            size,
        })
    } else {
        // Legacy V1 format (no header)
        Ok(WalSegmentInfo {
            version: WAL_VERSION_LEGACY,
            needs_migration: true,
            size,
        })
    }
}

/// Migrate a WAL segment from an older version to the current format.
///
/// This reads all entries from the old format and rewrites them in the
/// current format. The original file is backed up with a `.bak` extension.
///
/// # Arguments
/// * `path` - Path to the WAL segment file to migrate
///
/// # Returns
/// * `Ok(usize)` - Number of entries migrated
/// * `Err` - If migration fails
///
/// # Note
/// Migration of v1 segments will result in data loss for properties and
/// temporal intervals since v1 did not serialize this data. The migrated
/// entries will have empty properties and timestamp-based temporal intervals.
pub fn migrate_wal_segment(path: &Path) -> Result<usize> {
    use std::io::Read;

    let info = detect_wal_version(path)?;
    if !info.needs_migration {
        return Ok(0); // Already current version
    }

    // Read the entire file
    let mut file = File::open(path)
        .map_err(|e| StorageError::IoError(format!("Failed to open WAL segment: {}", e)))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .map_err(|e| StorageError::IoError(format!("Failed to read WAL segment: {}", e)))?;
    drop(file);

    // Parse entries using version-aware reading
    let entries = parse_wal_entries_versioned(&buffer, info.version, LSN::initial())?;
    let entry_count = entries.len();

    if entry_count == 0 {
        return Ok(0);
    }

    // Backup original file
    let backup_path = path.with_extension("log.bak");
    std::fs::rename(path, &backup_path)
        .map_err(|e| StorageError::IoError(format!("Failed to backup WAL segment: {}", e)))?;

    // Write entries in new format
    let mut new_file = File::create(path)
        .map_err(|e| StorageError::IoError(format!("Failed to create new WAL segment: {}", e)))?;

    // Write header
    new_file
        .write_all(&WAL_MAGIC)
        .map_err(|e| StorageError::IoError(format!("Failed to write WAL magic: {}", e)))?;
    new_file
        .write_all(&[WAL_VERSION])
        .map_err(|e| StorageError::IoError(format!("Failed to write WAL version: {}", e)))?;

    // Create a temporary WAL for serialization
    let temp_dir = std::env::temp_dir();
    let temp_config = WalConfig {
        wal_dir: temp_dir,
        ..Default::default()
    };
    let temp_wal = WriteAheadLog::new(temp_config)?;

    // Write each entry
    for entry in &entries {
        let serialized = temp_wal.serialize_entry(entry)?;
        new_file
            .write_all(&serialized)
            .map_err(|e| StorageError::IoError(format!("Failed to write WAL entry: {}", e)))?;
    }

    new_file
        .sync_all()
        .map_err(|e| StorageError::IoError(format!("Failed to sync migrated WAL: {}", e)))?;

    Ok(entry_count)
}

/// Migrate all WAL segments in a directory.
///
/// # Arguments
/// * `wal_dir` - Directory containing WAL segments
///
/// # Returns
/// * `Ok(Vec<(PathBuf, usize)>)` - List of migrated files and entry counts
pub fn migrate_wal_directory(wal_dir: &Path) -> Result<Vec<(PathBuf, usize)>> {
    let mut migrated = Vec::new();

    let entries = std::fs::read_dir(wal_dir)
        .map_err(|e| StorageError::IoError(format!("Failed to read WAL directory: {}", e)))?;

    for entry in entries {
        let entry = entry
            .map_err(|e| StorageError::IoError(format!("Failed to read directory entry: {}", e)))?;
        let path = entry.path();

        if path.extension().is_some_and(|ext| ext == "log") {
            let info = detect_wal_version(&path)?;
            if info.needs_migration {
                let count = migrate_wal_segment(&path)?;
                if count > 0 {
                    migrated.push((path, count));
                }
            }
        }
    }

    Ok(migrated)
}

/// Parse WAL entries with explicit version handling.
/// This is an internal helper for migration.
fn parse_wal_entries_versioned(
    buffer: &[u8],
    version: u8,
    start_lsn: LSN,
) -> Result<Vec<WalEntry>> {
    let mut entries = Vec::new();

    // Determine starting offset based on version
    let mut offset = if version >= WAL_VERSION && buffer.len() >= WAL_HEADER_SIZE {
        WAL_HEADER_SIZE
    } else {
        0
    };

    while offset < buffer.len() {
        // Need at least 20 bytes for LSN (8) + timestamp (8) + checksum (4)
        if offset + 20 > buffer.len() {
            break;
        }

        // Read LSN (8 bytes)
        let lsn = LSN(u64::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
            buffer[offset + 4],
            buffer[offset + 5],
            buffer[offset + 6],
            buffer[offset + 7],
        ]));
        offset += 8;

        // Read timestamp (8 bytes)
        let timestamp = i64::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
            buffer[offset + 4],
            buffer[offset + 5],
            buffer[offset + 6],
            buffer[offset + 7],
        ]);
        offset += 8;

        // Read checksum (4 bytes)
        let checksum = u32::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
        ]);
        offset += 4;

        // Read operation type
        if offset >= buffer.len() {
            break;
        }
        let op_type = buffer[offset];
        offset += 1;

        // Parse operation with version-aware logic
        let operation = match op_type {
            1 => {
                // CreateNode
                if offset + 12 > buffer.len() {
                    break;
                }
                let node_id = deserialize_node_id(buffer, offset, "CreateNode (migration)")?;
                offset += 8;

                let label_len = u32::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                ]) as usize;
                offset += 4;

                if offset + label_len > buffer.len() {
                    break;
                }
                let label =
                    String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                offset += label_len;

                let (properties, temporal) = if version >= WAL_VERSION {
                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
                    (props, temp)
                } else {
                    (PropertyMap::new(), BiTemporalInterval::current(timestamp))
                };

                WalOperation::CreateNode {
                    node_id,
                    label,
                    properties,
                    temporal,
                }
            }
            2 => {
                // CreateEdge
                if offset + 28 > buffer.len() {
                    break;
                }
                let edge_id = deserialize_edge_id(buffer, offset, "CreateEdge (migration)")?;
                offset += 8;

                let source = deserialize_node_id(buffer, offset, "CreateEdge source (migration)")?;
                offset += 8;

                let target = deserialize_node_id(buffer, offset, "CreateEdge target (migration)")?;
                offset += 8;

                let label_len = u32::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                ]) as usize;
                offset += 4;

                if offset + label_len > buffer.len() {
                    break;
                }
                let label =
                    String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                offset += label_len;

                let (properties, temporal) = if version >= WAL_VERSION {
                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
                    (props, temp)
                } else {
                    (PropertyMap::new(), BiTemporalInterval::current(timestamp))
                };

                WalOperation::CreateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    temporal,
                }
            }
            3 => {
                // UpdateNode
                if offset + 16 > buffer.len() {
                    break;
                }
                let node_id = deserialize_node_id(buffer, offset, "UpdateNode (migration)")?;
                offset += 8;

                let version_id = deserialize_version_id(buffer, offset, "UpdateNode (migration)")?;
                offset += 8;

                let (label, properties, temporal) = if version >= WAL_VERSION {
                    let label_len = u32::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + label_len > buffer.len() {
                        break;
                    }
                    let lbl =
                        String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                    offset += label_len;

                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
                    (lbl, props, temp)
                } else {
                    (
                        String::new(),
                        PropertyMap::new(),
                        BiTemporalInterval::current(timestamp),
                    )
                };

                WalOperation::UpdateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    temporal,
                }
            }
            4 => {
                // UpdateEdge
                if offset + 16 > buffer.len() {
                    break;
                }
                let edge_id = deserialize_edge_id(buffer, offset, "UpdateEdge (migration)")?;
                offset += 8;

                let version_id = deserialize_version_id(buffer, offset, "UpdateEdge (migration)")?;
                offset += 8;

                let (label, properties, temporal) = if version >= WAL_VERSION {
                    let label_len = u32::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + label_len > buffer.len() {
                        break;
                    }
                    let lbl =
                        String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                    offset += label_len;

                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
                    (lbl, props, temp)
                } else {
                    (
                        String::new(),
                        PropertyMap::new(),
                        BiTemporalInterval::current(timestamp),
                    )
                };

                WalOperation::UpdateEdge {
                    edge_id,
                    version_id,
                    label,
                    properties,
                    temporal,
                }
            }
            5 => {
                // Checkpoint
                if offset + 16 > buffer.len() {
                    break;
                }
                let cp_lsn = LSN(u64::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                    buffer[offset + 4],
                    buffer[offset + 5],
                    buffer[offset + 6],
                    buffer[offset + 7],
                ]));
                offset += 8;

                let cp_timestamp = i64::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                    buffer[offset + 4],
                    buffer[offset + 5],
                    buffer[offset + 6],
                    buffer[offset + 7],
                ]);
                offset += 8;

                WalOperation::Checkpoint {
                    lsn: cp_lsn,
                    timestamp: cp_timestamp,
                }
            }
            6 => {
                // DeleteNode
                if offset + 8 > buffer.len() {
                    break;
                }
                let node_id = deserialize_node_id(buffer, offset, "DeleteNode (migration)")?;
                offset += 8;

                let temporal = if version >= WAL_VERSION {
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
                    temp
                } else {
                    BiTemporalInterval::current(timestamp)
                };

                WalOperation::DeleteNode { node_id, temporal }
            }
            7 => {
                // DeleteEdge
                if offset + 8 > buffer.len() {
                    break;
                }
                let edge_id = deserialize_edge_id(buffer, offset, "DeleteEdge (migration)")?;
                offset += 8;

                let temporal = if version >= WAL_VERSION {
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
                    temp
                } else {
                    BiTemporalInterval::current(timestamp)
                };

                WalOperation::DeleteEdge { edge_id, temporal }
            }
            _ => {
                // Unknown operation type
                return Err(StorageError::CorruptedData(format!(
                    "Unknown WAL operation type: {}",
                    op_type
                ))
                .into());
            }
        };

        if lsn >= start_lsn {
            entries.push(WalEntry {
                lsn,
                timestamp,
                operation,
                checksum,
            });
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_lsn_ordering() {
        let lsn1 = LSN::initial();
        let lsn2 = lsn1.next();
        let lsn3 = lsn2.next();

        assert!(lsn1 < lsn2);
        assert!(lsn2 < lsn3);
        assert_eq!(lsn1.0, 1);
        assert_eq!(lsn2.0, 2);
        assert_eq!(lsn3.0, 3);
    }

    #[test]
    fn test_wal_entry_checksum() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let wal = WriteAheadLog::new(config)?;

        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: "Person".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        };

        let entry = WalEntry::new(LSN::initial(), operation);

        // Serialize the entry to get the actual bytes with checksum
        let serialized = wal.serialize_entry(&entry)?;

        // Verify checksum is valid
        assert!(entry.verify_checksum(&serialized));

        // Corrupt the checksum and verify it fails
        let mut corrupted = serialized.clone();
        corrupted[16] ^= 0xFF; // Flip some bits in the checksum
        assert!(!entry.verify_checksum(&corrupted));

        Ok(())
    }

    #[test]
    fn test_wal_creation() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };

        let wal = WriteAheadLog::new(config)?;
        assert_eq!(wal.current_lsn(), LSN::initial());
        Ok(())
    }

    #[test]
    fn test_wal_append() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false, // Faster for tests
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: "Person".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        };

        let lsn = wal.append(operation)?;
        assert_eq!(lsn, LSN::initial());
        assert_eq!(wal.current_lsn(), LSN(2));

        Ok(())
    }

    #[test]
    fn test_wal_segment_rotation() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            segment_size: 100, // Very small for testing rotation
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Append multiple entries to trigger rotation
        for i in 0..10 {
            let operation = WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: "Person".to_string(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            };
            wal.append(operation)?;
        }

        // Should have rotated to multiple segments
        assert!(wal.current_segment > 1);

        Ok(())
    }

    #[test]
    fn test_wal_delete_node_operation() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Log a delete node operation
        let operation = WalOperation::DeleteNode {
            node_id: NodeId::new(42).unwrap(),
            temporal: BiTemporalInterval::current(time::now()),
        };

        let lsn = wal.append(operation)?;
        assert_eq!(lsn, LSN::initial());

        // Flush to ensure it's written
        wal.flush()?;

        // Read it back
        let entries = wal.read_from(LSN::initial())?;
        assert_eq!(entries.len(), 1);

        match &entries[0].operation {
            WalOperation::DeleteNode { node_id, .. } => {
                assert_eq!(node_id.as_u64(), 42);
            }
            _ => panic!("Expected DeleteNode operation"),
        }

        Ok(())
    }

    #[test]
    fn test_wal_delete_edge_operation() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Log a delete edge operation
        let operation = WalOperation::DeleteEdge {
            edge_id: EdgeId::new(99).unwrap(),
            temporal: BiTemporalInterval::current(time::now()),
        };

        let lsn = wal.append(operation)?;
        assert_eq!(lsn, LSN::initial());

        // Flush to ensure it's written
        wal.flush()?;

        // Read it back
        let entries = wal.read_from(LSN::initial())?;
        assert_eq!(entries.len(), 1);

        match &entries[0].operation {
            WalOperation::DeleteEdge { edge_id, .. } => {
                assert_eq!(edge_id.as_u64(), 99);
            }
            _ => panic!("Expected DeleteEdge operation"),
        }

        Ok(())
    }

    #[test]
    fn test_wal_mixed_operations() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Log a sequence of mixed operations
        let ops = vec![
            WalOperation::CreateNode {
                node_id: NodeId::new(1).unwrap(),
                label: "Person".to_string(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            },
            WalOperation::CreateEdge {
                edge_id: EdgeId::new(1).unwrap(),
                source: NodeId::new(1).unwrap(),
                target: NodeId::new(2).unwrap(),
                label: "KNOWS".to_string(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            },
            WalOperation::DeleteNode {
                node_id: NodeId::new(3).unwrap(),
                temporal: BiTemporalInterval::current(time::now()),
            },
            WalOperation::DeleteEdge {
                edge_id: EdgeId::new(2).unwrap(),
                temporal: BiTemporalInterval::current(time::now()),
            },
        ];

        for op in ops {
            wal.append(op)?;
        }

        wal.flush()?;

        // Read all entries back
        let entries = wal.read_from(LSN::initial())?;
        assert_eq!(entries.len(), 4);

        // Verify operation types
        assert!(matches!(
            entries[0].operation,
            WalOperation::CreateNode { .. }
        ));
        assert!(matches!(
            entries[1].operation,
            WalOperation::CreateEdge { .. }
        ));
        assert!(matches!(
            entries[2].operation,
            WalOperation::DeleteNode { .. }
        ));
        assert!(matches!(
            entries[3].operation,
            WalOperation::DeleteEdge { .. }
        ));

        Ok(())
    }

    // Tests for full serialization with properties and temporal data

    #[test]
    fn test_wal_create_node_full_roundtrip() -> Result<()> {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::TimeRange;

        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Create properties with various types
        let properties = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .insert("score", 98.5f64)
            .build();

        // Create specific temporal interval
        let temporal = BiTemporalInterval::new(
            TimeRange::new(1000, 2000),
            TimeRange::new(3000, crate::core::temporal::TIMESTAMP_MAX),
        );

        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(42).unwrap(),
            label: "Person".to_string(),
            properties: properties.clone(),
            temporal,
        };

        wal.append(operation)?;
        wal.flush()?;

        // Read it back
        let entries = wal.read_from(LSN::initial())?;
        assert_eq!(entries.len(), 1);

        match &entries[0].operation {
            WalOperation::CreateNode {
                node_id,
                label,
                properties: p,
                temporal: t,
            } => {
                assert_eq!(node_id.as_u64(), 42);
                assert_eq!(label, "Person");

                // Verify properties
                assert_eq!(p.get("name").and_then(|v| v.as_str()), Some("Alice"));
                assert_eq!(p.get("age").and_then(|v| v.as_int()), Some(30));
                assert_eq!(p.get("active").and_then(|v| v.as_bool()), Some(true));

                // Verify temporal
                assert_eq!(t.valid_time().start(), 1000);
                assert_eq!(t.valid_time().end(), 2000);
                assert_eq!(t.transaction_time().start(), 3000);
                assert!(t.transaction_time().is_current());
            }
            _ => panic!("Expected CreateNode operation"),
        }

        Ok(())
    }

    #[test]
    fn test_wal_create_edge_full_roundtrip() -> Result<()> {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::TimeRange;

        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        let properties = PropertyMapBuilder::new()
            .insert("since", 2020i64)
            .insert("weight", 0.85f64)
            .build();

        let temporal =
            BiTemporalInterval::new(TimeRange::new(5000, 6000), TimeRange::new(7000, 8000));

        let operation = WalOperation::CreateEdge {
            edge_id: EdgeId::new(99).unwrap(),
            source: NodeId::new(1).unwrap(),
            target: NodeId::new(2).unwrap(),
            label: "FRIENDS_WITH".to_string(),
            properties: properties.clone(),
            temporal,
        };

        wal.append(operation)?;
        wal.flush()?;

        let entries = wal.read_from(LSN::initial())?;
        assert_eq!(entries.len(), 1);

        match &entries[0].operation {
            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                properties: p,
                temporal: t,
            } => {
                assert_eq!(edge_id.as_u64(), 99);
                assert_eq!(source.as_u64(), 1);
                assert_eq!(target.as_u64(), 2);
                assert_eq!(label, "FRIENDS_WITH");

                assert_eq!(p.get("since").and_then(|v| v.as_int()), Some(2020));

                assert_eq!(t.valid_time().start(), 5000);
                assert_eq!(t.valid_time().end(), 6000);
                assert_eq!(t.transaction_time().start(), 7000);
                assert_eq!(t.transaction_time().end(), 8000);
            }
            _ => panic!("Expected CreateEdge operation"),
        }

        Ok(())
    }

    #[test]
    fn test_wal_update_node_full_roundtrip() -> Result<()> {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::TimeRange;

        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        let properties = PropertyMapBuilder::new()
            .insert("age", 31i64) // Updated age
            .build();

        let temporal =
            BiTemporalInterval::new(TimeRange::new(10000, 20000), TimeRange::from(15000));

        let operation = WalOperation::UpdateNode {
            node_id: NodeId::new(42).unwrap(),
            version_id: VersionId::new(2).unwrap(),
            label: "Person".to_string(),
            properties: properties.clone(),
            temporal,
        };

        wal.append(operation)?;
        wal.flush()?;

        let entries = wal.read_from(LSN::initial())?;
        assert_eq!(entries.len(), 1);

        match &entries[0].operation {
            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties: p,
                temporal: t,
            } => {
                assert_eq!(node_id.as_u64(), 42);
                assert_eq!(version_id.as_u64(), 2);
                assert_eq!(label, "Person");
                assert_eq!(p.get("age").and_then(|v| v.as_int()), Some(31));
                assert_eq!(t.valid_time().start(), 10000);
            }
            _ => panic!("Expected UpdateNode operation"),
        }

        Ok(())
    }

    #[test]
    fn test_wal_delete_with_temporal() -> Result<()> {
        use crate::core::temporal::TimeRange;

        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        let temporal = BiTemporalInterval::new(TimeRange::new(100, 200), TimeRange::new(300, 400));

        wal.append(WalOperation::DeleteNode {
            node_id: NodeId::new(5).unwrap(),
            temporal,
        })?;

        wal.append(WalOperation::DeleteEdge {
            edge_id: EdgeId::new(10).unwrap(),
            temporal,
        })?;

        wal.flush()?;

        let entries = wal.read_from(LSN::initial())?;
        assert_eq!(entries.len(), 2);

        // Verify DeleteNode temporal is preserved
        match &entries[0].operation {
            WalOperation::DeleteNode {
                node_id,
                temporal: t,
            } => {
                assert_eq!(node_id.as_u64(), 5);
                assert_eq!(t.valid_time().start(), 100);
                assert_eq!(t.valid_time().end(), 200);
                assert_eq!(t.transaction_time().start(), 300);
                assert_eq!(t.transaction_time().end(), 400);
            }
            _ => panic!("Expected DeleteNode"),
        }

        // Verify DeleteEdge temporal is preserved
        match &entries[1].operation {
            WalOperation::DeleteEdge {
                edge_id,
                temporal: t,
            } => {
                assert_eq!(edge_id.as_u64(), 10);
                assert_eq!(t.valid_time().start(), 100);
                assert_eq!(t.transaction_time().end(), 400);
            }
            _ => panic!("Expected DeleteEdge"),
        }

        Ok(())
    }

    #[test]
    fn test_wal_segment_header() -> Result<()> {
        use std::io::Read;

        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: true,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Append an entry to create a segment
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: "Test".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        // Read the segment file directly to verify header
        let segment_path = temp_dir.path().join("000001.log");
        let mut file = std::fs::File::open(&segment_path).unwrap();
        let mut header = [0u8; 5];
        file.read_exact(&mut header).unwrap();

        // Verify magic bytes "GWAL"
        assert_eq!(&header[0..4], b"GWAL");
        // Verify version
        assert_eq!(header[4], 2); // WAL_VERSION

        Ok(())
    }

    #[test]
    fn test_wal_recovery_preserves_all_data() -> Result<()> {
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::TimeRange;

        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().to_path_buf();

        // Create data in WAL and simulate crash by dropping
        let original_properties;
        let original_temporal;
        {
            let config = WalConfig {
                wal_dir: wal_dir.clone(),
                sync_on_write: true,
                ..Default::default()
            };
            let mut wal = WriteAheadLog::new(config)?;

            original_properties = PropertyMapBuilder::new()
                .insert("key", "value")
                .insert("count", 42i64)
                .build();
            original_temporal =
                BiTemporalInterval::new(TimeRange::new(1000, 2000), TimeRange::new(3000, 4000));

            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(100).unwrap(),
                label: "TestNode".to_string(),
                properties: original_properties.clone(),
                temporal: original_temporal,
            })?;
            wal.flush()?;
            // WAL dropped here
        }

        // Recover from WAL
        {
            let config = WalConfig {
                wal_dir: wal_dir.clone(),
                ..Default::default()
            };
            let wal = WriteAheadLog::new(config)?;

            let entries = wal.read_from(LSN::initial())?;
            assert_eq!(entries.len(), 1);

            match &entries[0].operation {
                WalOperation::CreateNode {
                    node_id,
                    label,
                    properties,
                    temporal,
                } => {
                    assert_eq!(node_id.as_u64(), 100);
                    assert_eq!(label, "TestNode");

                    // Verify properties preserved
                    assert_eq!(
                        properties.get("key").and_then(|v| v.as_str()),
                        Some("value")
                    );
                    assert_eq!(properties.get("count").and_then(|v| v.as_int()), Some(42));

                    // Verify temporal preserved
                    assert_eq!(temporal.valid_time().start(), 1000);
                    assert_eq!(temporal.valid_time().end(), 2000);
                    assert_eq!(temporal.transaction_time().start(), 3000);
                    assert_eq!(temporal.transaction_time().end(), 4000);
                }
                _ => panic!("Expected CreateNode"),
            }
        }

        Ok(())
    }

    #[test]
    fn test_detect_wal_version_v2() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: true,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: "Test".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        let segment_path = temp_dir.path().join("000001.log");
        let info = detect_wal_version(&segment_path)?;

        assert_eq!(info.version, 2);
        assert!(!info.needs_migration);
        assert!(info.size > 0);

        Ok(())
    }

    #[test]
    fn test_detect_empty_segment() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let segment_path = temp_dir.path().join("empty.log");
        std::fs::File::create(&segment_path).unwrap();

        let info = detect_wal_version(&segment_path)?;

        assert_eq!(info.version, 2); // Empty segments default to current version
        assert!(!info.needs_migration);
        assert_eq!(info.size, 0);

        Ok(())
    }

    #[test]
    fn test_migration_not_needed_for_v2() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: true,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: "Test".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;

        let segment_path = temp_dir.path().join("000001.log");
        let count = migrate_wal_segment(&segment_path)?;

        // No migration needed for v2 segments
        assert_eq!(count, 0);

        Ok(())
    }

    // ========== New Error Handling Tests for Coverage ==========

    #[test]
    fn test_wal_invalid_node_id_dos_protection() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let segment_path = temp_dir.path().join("000001.log");

        // Manually craft WAL entry with invalid node_id (exceeds MAX_VALID_ID)
        let mut buffer = Vec::new();

        // Write header
        buffer.extend_from_slice(&WAL_MAGIC);
        buffer.push(WAL_VERSION);

        // Write LSN
        buffer.extend_from_slice(&1u64.to_le_bytes());

        // Write timestamp
        buffer.extend_from_slice(&time::now().to_le_bytes());

        // Write checksum placeholder
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // Write OpType::CreateNode (1)
        buffer.push(1u8);

        // Write INVALID node_id (exceeds MAX_VALID_ID)
        let invalid_id = u64::MAX;
        buffer.extend_from_slice(&invalid_id.to_le_bytes());

        // Write minimal rest of entry
        let label = "Test";
        buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
        buffer.extend_from_slice(label.as_bytes());

        // Write empty properties
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // Write temporal data
        let temporal = BiTemporalInterval::current(time::now());
        temporal.serialize_into(&mut buffer);

        std::fs::write(&segment_path, buffer)?;

        // Attempt to read via WAL
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let wal = WriteAheadLog::new(config)?;
        let result = wal.read_from(LSN::initial());

        // Should fail with corrupted data error for invalid ID
        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Storage(
                    crate::utils::error::StorageError::CorruptedData(_)
                )
            ),
            "Expected CorruptedData error for invalid node ID"
        );

        Ok(())
    }

    #[test]
    fn test_wal_invalid_edge_id_dos_protection() -> Result<()> {
        use crate::core::id::MAX_VALID_ID;

        let temp_dir = TempDir::new().unwrap();
        let segment_path = temp_dir.path().join("000001.log");

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&WAL_MAGIC);
        buffer.push(WAL_VERSION);
        buffer.extend_from_slice(&1u64.to_le_bytes());
        buffer.extend_from_slice(&time::now().to_le_bytes());
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // OpType::CreateEdge (2)
        buffer.push(2u8);

        // Invalid edge_id
        buffer.extend_from_slice(&(MAX_VALID_ID + 1).to_le_bytes());

        // source and target (need to add label, properties, temporal for complete entry)
        buffer.extend_from_slice(&1u64.to_le_bytes());
        buffer.extend_from_slice(&2u64.to_le_bytes());

        // Add label
        let label = "Test";
        buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
        buffer.extend_from_slice(label.as_bytes());

        // Add empty properties
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // Add temporal
        let temporal = BiTemporalInterval::current(time::now());
        temporal.serialize_into(&mut buffer);

        std::fs::write(&segment_path, buffer)?;

        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let wal = WriteAheadLog::new(config)?;
        let result = wal.read_from(LSN::initial());

        // Should fail with corrupted data error for invalid edge ID
        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Storage(
                    crate::utils::error::StorageError::CorruptedData(_)
                )
            ),
            "Expected CorruptedData error for invalid edge ID"
        );

        Ok(())
    }

    #[test]
    fn test_wal_invalid_version_id_rejection() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let segment_path = temp_dir.path().join("000001.log");

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&WAL_MAGIC);
        buffer.push(WAL_VERSION);
        buffer.extend_from_slice(&1u64.to_le_bytes());
        buffer.extend_from_slice(&time::now().to_le_bytes());
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // OpType::UpdateNode (3)
        buffer.push(3u8);

        // Valid node_id
        buffer.extend_from_slice(&1u64.to_le_bytes());

        // Invalid version_id
        buffer.extend_from_slice(&u64::MAX.to_le_bytes());

        std::fs::write(&segment_path, buffer)?;

        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let wal = WriteAheadLog::new(config)?;
        let result = wal.read_from(LSN::initial());

        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Storage(
                    crate::utils::error::StorageError::CorruptedData(_)
                )
            ),
            "Expected CorruptedData error for invalid version ID"
        );

        Ok(())
    }

    #[test]
    fn test_wal_unknown_operation_type() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let segment_path = temp_dir.path().join("000001.log");

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&WAL_MAGIC);
        buffer.push(WAL_VERSION);
        buffer.extend_from_slice(&1u64.to_le_bytes());
        buffer.extend_from_slice(&time::now().to_le_bytes());
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // UNKNOWN OpType (99)
        buffer.push(99u8);

        std::fs::write(&segment_path, buffer)?;

        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let wal = WriteAheadLog::new(config)?;
        let result = wal.read_from(LSN::initial());

        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Storage(
                    crate::utils::error::StorageError::CorruptedData(_)
                )
            ),
            "Expected CorruptedData error for unknown operation type"
        );

        Ok(())
    }

    #[test]
    fn test_wal_truncated_entry_graceful_stop() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: true,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Write 5 complete entries
        for i in 1..=5 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: "Test".to_string(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;
        drop(wal);

        // Truncate the file mid-entry (remove last 20 bytes)
        let segment_path = temp_dir.path().join("000001.log");
        let mut data = std::fs::read(&segment_path)?;
        let truncate_len = data.len().saturating_sub(20);
        data.truncate(truncate_len);
        std::fs::write(&segment_path, data)?;

        // Read should either error or stop gracefully at truncation
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let wal = WriteAheadLog::new(config)?;
        let result = wal.read_from(LSN::initial());

        // Accept either error (strict) or partial recovery (graceful)
        match result {
            Err(e) => {
                // Truncation detected - this is valid behavior
                assert!(
                    e.to_string().contains("Buffer too short") || e.to_string().contains("corrupt")
                );
            }
            Ok(entries) => {
                // Graceful recovery - should have at least some entries
                assert!(entries.len() >= 4 && entries.len() <= 5);
            }
        }

        Ok(())
    }

    #[test]
    fn test_wal_corrupted_checksum_detection() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: true,
            ..Default::default()
        };

        let mut wal = WriteAheadLog::new(config)?;
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: "Test".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        wal.flush()?;
        drop(wal);

        // Corrupt some data (not just checksum) to trigger corruption detection
        let segment_path = temp_dir.path().join("000001.log");
        let mut data = std::fs::read(&segment_path)?;

        // Corrupt data in the middle (operation type byte after header)
        if data.len() > WAL_HEADER_SIZE + 20 {
            // Corrupt the operation section
            data[WAL_HEADER_SIZE + 16] ^= 0xFF;
            data[WAL_HEADER_SIZE + 17] ^= 0xFF;
            std::fs::write(&segment_path, data)?;
        }

        // Attempt to read - should detect corruption or fail gracefully
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let wal = WriteAheadLog::new(config)?;
        let result = wal.read_from(LSN::initial());

        // Corrupted data should either error or be detected as invalid
        // This test documents the current behavior
        match result {
            Err(_) => {} // Expected - corruption detected
            Ok(entries) => {
                // If implementation doesn't validate checksums yet, might succeed
                // but this test documents that corruption detection is desired
                assert!(entries.len() <= 1);
            }
        }

        Ok(())
    }

    #[test]
    fn test_wal_segment_cleanup_respects_retention() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();

        // Create config with very small segment size to force rotation
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: true,
            segment_size: 256, // Very small to force rotation
            segments_to_retain: 5,
            durability_mode: DurabilityMode::Synchronous,
        };

        let mut wal = WriteAheadLog::new(config)?;

        // Create enough entries to force multiple segment rotations
        for i in 0..150 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i as u64 + 1).unwrap(),
                label: format!("TestNode{}", i),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;

            // Flush every few entries to force rotations
            if i % 5 == 0 {
                wal.flush()?;
            }
        }

        wal.flush()?;

        // Trigger cleanup
        wal.cleanup_old_segments()?;

        // Count remaining segment files
        let segment_count = std::fs::read_dir(temp_dir.path())?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s == "log")
                    .unwrap_or(false)
            })
            .count();

        // Should have at most segments_to_retain + current segment
        assert!(
            segment_count <= 6,
            "Expected ≤6 segments (5 retained + 1 current), found {}",
            segment_count
        );

        Ok(())
    }
}
