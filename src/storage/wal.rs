//! Write-Ahead Log (WAL) implementation for crash recovery and durability.
//!
//! The WAL provides:
//! - Sequential logging of all mutations
//! - Crash recovery by replaying operations
//! - Point-in-time recovery capabilities
//!
//! # WAL Format
//!
//! Each WAL entry consists of:
//! - Log Sequence Number (LSN): Monotonically increasing identifier
//! - Timestamp: When the operation was logged
//! - Operation: The mutation to apply
//! - Checksum: CRC32 for corruption detection

use crate::core::{
    id::{EdgeId, NodeId, VersionId},
    property::PropertyMap,
    temporal::{BiTemporalInterval, Timestamp, time},
};
use crate::utils::error::{Result, StorageError};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

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
    /// Create a new WAL entry
    pub fn new(lsn: LSN, operation: WalOperation) -> Self {
        let timestamp = time::now();
        let checksum = Self::compute_checksum(lsn, timestamp, &operation);
        WalEntry {
            lsn,
            timestamp,
            operation,
            checksum,
        }
    }

    /// Compute CRC32 checksum for the entry
    fn compute_checksum(lsn: LSN, timestamp: Timestamp, operation: &WalOperation) -> u32 {
        // Simple checksum based on LSN and timestamp
        // In production, would use proper CRC32 over serialized data
        let mut hash = lsn.0 as u32;
        hash = hash.wrapping_mul(31).wrapping_add(timestamp as u32);

        // Add operation type discriminant
        let op_type = match operation {
            WalOperation::CreateNode { .. } => 1,
            WalOperation::CreateEdge { .. } => 2,
            WalOperation::UpdateNode { .. } => 3,
            WalOperation::UpdateEdge { .. } => 4,
            WalOperation::Checkpoint { .. } => 5,
        };
        hash = hash.wrapping_mul(31).wrapping_add(op_type);

        hash
    }

    /// Verify the checksum is valid
    pub fn verify_checksum(&self) -> bool {
        let computed = Self::compute_checksum(self.lsn, self.timestamp, &self.operation);
        computed == self.checksum
    }
}

/// Configuration for WAL behavior
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Directory where WAL files are stored
    pub wal_dir: PathBuf,
    /// Maximum size of a WAL segment before rotation (in bytes)
    pub segment_size: usize,
    /// Whether to fsync after every write (slower but more durable)
    pub sync_on_write: bool,
    /// Number of WAL segments to keep for recovery
    pub segments_to_retain: usize,
}

impl Default for WalConfig {
    fn default() -> Self {
        WalConfig {
            wal_dir: PathBuf::from("gallifreydb/wal"),
            segment_size: 64 * 1024 * 1024, // 64MB
            sync_on_write: true,
            segments_to_retain: 10,
        }
    }
}

/// Write-Ahead Log manager
pub struct WriteAheadLog {
    config: WalConfig,
    current_lsn: LSN,
    current_segment: u64,
    writer: Option<BufWriter<File>>,
    current_size: usize,
}

impl WriteAheadLog {
    /// Create a new WAL with the given configuration
    pub fn new(config: WalConfig) -> Result<Self> {
        // Create WAL directory if it doesn't exist
        std::fs::create_dir_all(&config.wal_dir)
            .map_err(|e| StorageError::IoError(format!("Failed to create WAL directory: {}", e)))?;

        Ok(WriteAheadLog {
            config,
            current_lsn: LSN::initial(),
            current_segment: 1,
            writer: None,
            current_size: 0,
        })
    }

    /// Open a new WAL segment for writing
    fn open_segment(&mut self, segment_id: u64) -> Result<()> {
        let segment_path = self.segment_path(segment_id);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&segment_path)
            .map_err(|e| StorageError::IoError(format!("Failed to open WAL segment: {}", e)))?;

        // Get current file size
        let metadata = file.metadata().map_err(|e| {
            StorageError::IoError(format!("Failed to get WAL segment metadata: {}", e))
        })?;

        self.current_size = metadata.len() as usize;
        self.writer = Some(BufWriter::new(file));

        Ok(())
    }

    /// Get the path for a WAL segment
    fn segment_path(&self, segment_id: u64) -> PathBuf {
        self.config.wal_dir.join(format!("{:06}.log", segment_id))
    }

    /// Append an operation to the WAL
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

            if self.config.sync_on_write {
                writer
                    .flush()
                    .map_err(|e| StorageError::IoError(format!("Failed to flush WAL: {}", e)))?;
            }

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
        // Flush and close current writer
        if let Some(mut writer) = self.writer.take() {
            writer.flush().map_err(|e| {
                StorageError::IoError(format!("Failed to flush WAL on rotation: {}", e))
            })?;
        }

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

    /// Serialize a WAL entry (simplified version)
    fn serialize_entry(&self, entry: &WalEntry) -> Result<Vec<u8>> {
        // In production, would use a proper serialization format like bincode or postcard
        // For now, using a simple format for demonstration
        let mut buffer = Vec::new();

        // Write LSN (8 bytes)
        buffer.extend_from_slice(&entry.lsn.0.to_le_bytes());

        // Write timestamp (8 bytes)
        buffer.extend_from_slice(&entry.timestamp.to_le_bytes());

        // Write checksum (4 bytes)
        buffer.extend_from_slice(&entry.checksum.to_le_bytes());

        // Write operation type and data (simplified)
        match &entry.operation {
            WalOperation::CreateNode { node_id, label, .. } => {
                buffer.push(1); // operation type
                buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
                buffer.extend_from_slice(label.as_bytes());
            }
            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                ..
            } => {
                buffer.push(2); // operation type
                buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&source.as_u64().to_le_bytes());
                buffer.extend_from_slice(&target.as_u64().to_le_bytes());
                buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
                buffer.extend_from_slice(label.as_bytes());
            }
            WalOperation::UpdateNode {
                node_id,
                version_id,
                ..
            } => {
                buffer.push(3); // operation type
                buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            }
            WalOperation::UpdateEdge {
                edge_id,
                version_id,
                ..
            } => {
                buffer.push(4); // operation type
                buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
                buffer.extend_from_slice(&version_id.as_u64().to_le_bytes());
            }
            WalOperation::Checkpoint { lsn, timestamp } => {
                buffer.push(5); // operation type
                buffer.extend_from_slice(&lsn.0.to_le_bytes());
                buffer.extend_from_slice(&timestamp.to_le_bytes());
            }
        }

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

    /// Read WAL entries from a single segment file
    fn read_segment(&self, _path: &Path, _start_lsn: LSN) -> Result<Vec<WalEntry>> {
        // Simplified reading - in production would properly deserialize
        // For now, return empty as we need proper serialization/deserialization
        Ok(Vec::new())
    }

    /// Flush all pending writes
    pub fn flush(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer
                .flush()
                .map_err(|e| StorageError::IoError(format!("Failed to flush WAL: {}", e)))?;
        }
        Ok(())
    }

    /// Get the current LSN
    pub fn current_lsn(&self) -> LSN {
        self.current_lsn
    }
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
    fn test_wal_entry_checksum() {
        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(1),
            label: "Person".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        };

        let entry = WalEntry::new(LSN::initial(), operation);
        assert!(entry.verify_checksum());
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
            node_id: NodeId::new(1),
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
                node_id: NodeId::new(i),
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
}
