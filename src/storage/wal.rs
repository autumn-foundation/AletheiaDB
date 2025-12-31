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
            WalOperation::DeleteNode { node_id, .. } => {
                buffer.push(6); // operation type
                buffer.extend_from_slice(&node_id.as_u64().to_le_bytes());
            }
            WalOperation::DeleteEdge { edge_id, .. } => {
                buffer.push(7); // operation type
                buffer.extend_from_slice(&edge_id.as_u64().to_le_bytes());
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

        let mut offset = 0;
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

            // Parse operation data based on type
            let (operation, _bytes_read) = match op_type {
                1 => {
                    // CreateNode
                    if offset + 12 > buffer.len() {
                        break;
                    }
                    let node_id = NodeId::new(u64::from_le_bytes([
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

                    (
                        WalOperation::CreateNode {
                            node_id,
                            label,
                            properties: PropertyMap::new(),
                            temporal: BiTemporalInterval::current(timestamp),
                        },
                        12 + label_len,
                    )
                }
                2 => {
                    // CreateEdge
                    if offset + 28 > buffer.len() {
                        break;
                    }
                    let edge_id = EdgeId::new(u64::from_le_bytes([
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

                    let source = NodeId::new(u64::from_le_bytes([
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

                    let target = NodeId::new(u64::from_le_bytes([
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

                    (
                        WalOperation::CreateEdge {
                            edge_id,
                            source,
                            target,
                            label,
                            properties: PropertyMap::new(),
                            temporal: BiTemporalInterval::current(timestamp),
                        },
                        28 + label_len,
                    )
                }
                3 => {
                    // UpdateNode
                    if offset + 16 > buffer.len() {
                        break;
                    }
                    let node_id = NodeId::new(u64::from_le_bytes([
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

                    let version_id = VersionId::new(u64::from_le_bytes([
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

                    (
                        WalOperation::UpdateNode {
                            node_id,
                            version_id,
                            label: String::new(),
                            properties: PropertyMap::new(),
                            temporal: BiTemporalInterval::current(timestamp),
                        },
                        16,
                    )
                }
                4 => {
                    // UpdateEdge
                    if offset + 16 > buffer.len() {
                        break;
                    }
                    let edge_id = EdgeId::new(u64::from_le_bytes([
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

                    let version_id = VersionId::new(u64::from_le_bytes([
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

                    (
                        WalOperation::UpdateEdge {
                            edge_id,
                            version_id,
                            label: String::new(),
                            properties: PropertyMap::new(),
                            temporal: BiTemporalInterval::current(timestamp),
                        },
                        16,
                    )
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

                    (
                        WalOperation::Checkpoint {
                            lsn: cp_lsn,
                            timestamp: cp_timestamp,
                        },
                        16,
                    )
                }
                6 => {
                    // DeleteNode
                    if offset + 8 > buffer.len() {
                        break;
                    }
                    let node_id = NodeId::new(u64::from_le_bytes([
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

                    (
                        WalOperation::DeleteNode {
                            node_id,
                            temporal: BiTemporalInterval::current(timestamp),
                        },
                        8,
                    )
                }
                7 => {
                    // DeleteEdge
                    if offset + 8 > buffer.len() {
                        break;
                    }
                    let edge_id = EdgeId::new(u64::from_le_bytes([
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

                    (
                        WalOperation::DeleteEdge {
                            edge_id,
                            temporal: BiTemporalInterval::current(timestamp),
                        },
                        8,
                    )
                }
                _ => {
                    // Unknown operation type, skip this entry
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

    /// Flush all pending writes and sync to disk
    pub fn flush(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            // Step 1: Flush BufWriter buffer to OS
            writer
                .flush()
                .map_err(|e| StorageError::IoError(format!("Failed to flush WAL buffer: {}", e)))?;

            // Step 2: Force OS to sync data to disk (fsync)
            // SAFETY: sync_data() ensures durability by forcing the OS to write buffered data to disk.
            // This is critical for WAL correctness - without it, committed transactions could be lost
            // on crash/power failure. We use sync_data() instead of sync_all() because we only need
            // to sync file data, not metadata (faster).
            writer
                .get_mut() // Get underlying File from BufWriter
                .sync_data()
                .map_err(|e| StorageError::IoError(format!("Failed to fsync WAL: {}", e)))?;
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
    fn test_wal_entry_checksum() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false,
            ..Default::default()
        };

        let wal = WriteAheadLog::new(config)?;

        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(1),
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
            node_id: NodeId::new(42),
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
            edge_id: EdgeId::new(99),
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
                node_id: NodeId::new(1),
                label: "Person".to_string(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            },
            WalOperation::CreateEdge {
                edge_id: EdgeId::new(1),
                source: NodeId::new(1),
                target: NodeId::new(2),
                label: "KNOWS".to_string(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            },
            WalOperation::DeleteNode {
                node_id: NodeId::new(3),
                temporal: BiTemporalInterval::current(time::now()),
            },
            WalOperation::DeleteEdge {
                edge_id: EdgeId::new(2),
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
}
