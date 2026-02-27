//! Persistent commit log for Two-Phase Commit decisions.
//!
//! This module provides durable storage for 2PC commit decisions,
//! ensuring that the coordinator can recover after a crash and complete
//! any pending transactions.
//!
//! # File Format
//!
//! The commit log uses a simple append-only format:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ Header (16 bytes)                                           │
//! │   Magic: "GDB2" (4 bytes)                                  │
//! │   Version: u8                                               │
//! │   Reserved: 3 bytes                                         │
//! │   Entry count: u64                                          │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Entry 0                                                     │
//! │   Entry length: u32                                         │
//! │   Entry type: u8 (1=commit, 2=abort, 3=complete)           │
//! │   LSN: u64                                                  │
//! │   TxId: u64                                                 │
//! │   Timestamp: u64 (unix micros)                              │
//! │   Participant count: u16                                    │
//! │   Participant shard IDs: [u16; count]                       │
//! │   Checksum: u32                                             │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Entry 1...                                                  │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use super::types::ShardId;
use crate::core::hlc::HybridTimestamp;
use crate::core::id::TxId;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Magic bytes for the commit log file.
const COMMIT_LOG_MAGIC: [u8; 4] = *b"GDB2";

/// Current commit log format version.
const COMMIT_LOG_VERSION: u8 = 2;

/// Header size in bytes.
const HEADER_SIZE: usize = 16;

/// Entry type for commit decision.
const ENTRY_TYPE_COMMIT: u8 = 1;

/// Entry type for abort decision.
const ENTRY_TYPE_ABORT: u8 = 2;

/// Entry type for completion (all participants acknowledged).
const ENTRY_TYPE_COMPLETE: u8 = 3;

/// Error types for commit log operations.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum CommitLogError {
    /// I/O error.
    IoError(String),
    /// Corrupt log file.
    CorruptedLog(String),
    /// Invalid entry.
    InvalidEntry(String),
    /// Transaction not found.
    TransactionNotFound(TxId),
    /// Checksum mismatch.
    ChecksumMismatch { expected: u32, actual: u32 },
}

impl std::fmt::Display for CommitLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommitLogError::IoError(msg) => write!(f, "I/O error: {}", msg),
            CommitLogError::CorruptedLog(msg) => write!(f, "Corrupted log: {}", msg),
            CommitLogError::InvalidEntry(msg) => write!(f, "Invalid entry: {}", msg),
            CommitLogError::TransactionNotFound(tx_id) => {
                write!(f, "Transaction {} not found", tx_id)
            }
            CommitLogError::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "Checksum mismatch: expected {}, got {}",
                    expected, actual
                )
            }
        }
    }
}

impl std::error::Error for CommitLogError {}

/// Result type for commit log operations.
pub type CommitLogResult<T> = Result<T, CommitLogError>;

/// A commit log entry.
#[derive(Debug, Clone)]
pub struct CommitLogEntry {
    /// Log sequence number.
    pub lsn: u64,
    /// Entry type (commit, abort, complete).
    pub entry_type: EntryType,
    /// Transaction ID.
    pub tx_id: TxId,
    /// Timestamp (unix microseconds).
    pub timestamp_us: u64,
    /// Participating shards.
    pub participants: Vec<ShardId>,
    /// Commit timestamp (Hybrid Logical Clock).
    pub commit_timestamp: Option<HybridTimestamp>,
}

/// Type of commit log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// Commit decision.
    Commit,
    /// Abort decision.
    Abort,
    /// Transaction completed (all participants acknowledged).
    Complete,
}

impl CommitLogEntry {
    /// Create a commit entry.
    pub fn commit(
        lsn: u64,
        tx_id: TxId,
        participants: Vec<ShardId>,
        commit_timestamp: Option<HybridTimestamp>,
    ) -> Self {
        Self {
            lsn,
            entry_type: EntryType::Commit,
            tx_id,
            timestamp_us: current_timestamp_us(),
            participants,
            commit_timestamp,
        }
    }

    /// Create an abort entry.
    pub fn abort(lsn: u64, tx_id: TxId, participants: Vec<ShardId>) -> Self {
        Self {
            lsn,
            entry_type: EntryType::Abort,
            tx_id,
            timestamp_us: current_timestamp_us(),
            participants,
            commit_timestamp: None,
        }
    }

    /// Create a completion entry.
    pub fn complete(lsn: u64, tx_id: TxId) -> Self {
        Self {
            lsn,
            entry_type: EntryType::Complete,
            tx_id,
            timestamp_us: current_timestamp_us(),
            participants: Vec::new(),
            commit_timestamp: None,
        }
    }

    /// Serialize the entry to bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(64);

        // Entry type
        let type_byte = match self.entry_type {
            EntryType::Commit => ENTRY_TYPE_COMMIT,
            EntryType::Abort => ENTRY_TYPE_ABORT,
            EntryType::Complete => ENTRY_TYPE_COMPLETE,
        };
        data.push(type_byte);

        // LSN
        data.extend_from_slice(&self.lsn.to_le_bytes());

        // TxId
        data.extend_from_slice(&self.tx_id.as_u64().to_le_bytes());

        // Timestamp
        data.extend_from_slice(&self.timestamp_us.to_le_bytes());

        // Participant count and IDs
        data.extend_from_slice(&(self.participants.len() as u16).to_le_bytes());
        for shard_id in &self.participants {
            data.extend_from_slice(&shard_id.as_u16().to_le_bytes());
        }

        // Commit Timestamp (Optional)
        if let Some(ts) = self.commit_timestamp {
            data.push(1); // Present
            ts.serialize_into(&mut data);
        } else {
            data.push(0); // Absent
        }

        // Calculate checksum
        let checksum = compute_checksum(&data);
        data.extend_from_slice(&checksum.to_le_bytes());

        // Prepend length
        let len = data.len() as u32;
        let mut result = Vec::with_capacity(4 + data.len());
        result.extend_from_slice(&len.to_le_bytes());
        result.extend(data);

        result
    }

    /// Deserialize an entry from bytes.
    pub fn deserialize(data: &[u8]) -> CommitLogResult<Self> {
        if data.len() < 4 {
            return Err(CommitLogError::InvalidEntry("Entry too short".into()));
        }

        let len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if data.len() < 4 + len {
            return Err(CommitLogError::InvalidEntry("Incomplete entry".into()));
        }

        let entry_data = &data[4..4 + len];

        // Verify checksum
        if entry_data.len() < 4 {
            return Err(CommitLogError::InvalidEntry("No checksum".into()));
        }
        let checksum_offset = entry_data.len() - 4;
        let stored_checksum = u32::from_le_bytes([
            entry_data[checksum_offset],
            entry_data[checksum_offset + 1],
            entry_data[checksum_offset + 2],
            entry_data[checksum_offset + 3],
        ]);
        let computed_checksum = compute_checksum(&entry_data[..checksum_offset]);
        if stored_checksum != computed_checksum {
            return Err(CommitLogError::ChecksumMismatch {
                expected: stored_checksum,
                actual: computed_checksum,
            });
        }

        // Parse entry
        let mut offset = 0;

        // Entry type
        let entry_type = match entry_data[offset] {
            ENTRY_TYPE_COMMIT => EntryType::Commit,
            ENTRY_TYPE_ABORT => EntryType::Abort,
            ENTRY_TYPE_COMPLETE => EntryType::Complete,
            other => {
                return Err(CommitLogError::InvalidEntry(format!(
                    "Unknown entry type: {}",
                    other
                )));
            }
        };
        offset += 1;

        // LSN
        if offset + 8 > checksum_offset {
            return Err(CommitLogError::InvalidEntry("Missing LSN".into()));
        }
        let lsn = u64::from_le_bytes([
            entry_data[offset],
            entry_data[offset + 1],
            entry_data[offset + 2],
            entry_data[offset + 3],
            entry_data[offset + 4],
            entry_data[offset + 5],
            entry_data[offset + 6],
            entry_data[offset + 7],
        ]);
        offset += 8;

        // TxId
        if offset + 8 > checksum_offset {
            return Err(CommitLogError::InvalidEntry("Missing TxId".into()));
        }
        let tx_id_val = u64::from_le_bytes([
            entry_data[offset],
            entry_data[offset + 1],
            entry_data[offset + 2],
            entry_data[offset + 3],
            entry_data[offset + 4],
            entry_data[offset + 5],
            entry_data[offset + 6],
            entry_data[offset + 7],
        ]);
        let tx_id = TxId::new(tx_id_val);
        offset += 8;

        // Timestamp
        if offset + 8 > checksum_offset {
            return Err(CommitLogError::InvalidEntry("Missing timestamp".into()));
        }
        let timestamp_us = u64::from_le_bytes([
            entry_data[offset],
            entry_data[offset + 1],
            entry_data[offset + 2],
            entry_data[offset + 3],
            entry_data[offset + 4],
            entry_data[offset + 5],
            entry_data[offset + 6],
            entry_data[offset + 7],
        ]);
        offset += 8;

        // Participants
        if offset + 2 > checksum_offset {
            return Err(CommitLogError::InvalidEntry(
                "Missing participant count".into(),
            ));
        }
        let participant_count =
            u16::from_le_bytes([entry_data[offset], entry_data[offset + 1]]) as usize;
        offset += 2;

        let mut participants = Vec::with_capacity(participant_count);
        for _ in 0..participant_count {
            if offset + 2 > checksum_offset {
                return Err(CommitLogError::InvalidEntry(
                    "Incomplete participants".into(),
                ));
            }
            let shard_id_val = u16::from_le_bytes([entry_data[offset], entry_data[offset + 1]]);
            let shard_id = ShardId::new(shard_id_val).map_err(|_| {
                CommitLogError::InvalidEntry(format!(
                    "Invalid shard ID {} exceeds maximum allowed value",
                    shard_id_val
                ))
            })?;
            participants.push(shard_id);
            offset += 2;
        }

        // Commit Timestamp (Optional, appended in V2)
        // Check if there are bytes remaining before checksum
        let commit_timestamp = if offset < checksum_offset {
            let has_ts = entry_data[offset];
            offset += 1;
            if has_ts == 1 {
                // Deserialize HybridTimestamp (12 bytes)
                if offset + 12 > checksum_offset {
                    return Err(CommitLogError::InvalidEntry(
                        "Truncated commit timestamp".into(),
                    ));
                }
                let (ts, _consumed) = HybridTimestamp::deserialize(&entry_data[offset..])
                    .map_err(|e| CommitLogError::InvalidEntry(e.to_string()))?;
                Some(ts)
            } else {
                None
            }
        } else {
            None
        };

        Ok(CommitLogEntry {
            lsn,
            entry_type,
            tx_id,
            timestamp_us,
            participants,
            commit_timestamp,
        })
    }

    /// Get the total serialized size of this entry.
    pub fn serialized_size(&self) -> usize {
        let ts_size = if self.commit_timestamp.is_some() {
            1 + 12 // flag + timestamp
        } else {
            1 // flag
        };

        4  // length prefix
        + 1 // entry type
        + 8 // lsn
        + 8 // tx_id
        + 8 // timestamp
        + 2 // participant count
        + self.participants.len() * 2 // participant ids
        + ts_size
        + 4 // checksum
    }
}

/// Persistent commit log for 2PC decisions.
///
/// This log ensures that commit decisions survive coordinator crashes.
/// During recovery, the coordinator reads this log to determine which
/// transactions need to be completed (commit or abort).
#[derive(Debug)]
pub struct PersistentCommitLog {
    /// Path to the commit log file.
    #[allow(dead_code)]
    path: PathBuf,
    /// Current LSN.
    lsn: AtomicU64,
    /// File handle for writing.
    writer: RwLock<Option<BufWriter<File>>>,
    /// In-memory index of pending decisions.
    pending: RwLock<HashMap<TxId, CommitLogEntry>>,
    /// Configuration.
    config: CommitLogConfig,
    /// Total entries written.
    entries_written: AtomicU64,
    /// Total bytes written.
    bytes_written: AtomicU64,
    /// Maximum transaction ID seen.
    max_seen_tx_id: AtomicU64,
}

/// Configuration for the persistent commit log.
#[derive(Debug, Clone)]
pub struct CommitLogConfig {
    /// Whether to fsync after each write.
    pub sync_on_write: bool,
    /// Maximum file size before rotation (bytes).
    pub max_file_size: u64,
    /// Number of old log files to retain.
    pub files_to_retain: usize,
}

impl Default for CommitLogConfig {
    fn default() -> Self {
        Self {
            sync_on_write: true,
            max_file_size: 64 * 1024 * 1024, // 64 MB
            files_to_retain: 3,
        }
    }
}

impl PersistentCommitLog {
    /// Create a new persistent commit log.
    ///
    /// This will create the log file if it doesn't exist, or open
    /// the existing one for appending.
    pub fn new(path: impl Into<PathBuf>, config: CommitLogConfig) -> CommitLogResult<Self> {
        let path = path.into();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CommitLogError::IoError(format!("Failed to create directory: {}", e))
            })?;
        }

        let (writer, lsn, pending, max_tx_id) = if path.exists() {
            // Open existing file and recover state
            let (entries, max_lsn, max_tx_id, valid_len) = Self::read_entries(&path)?;

            // Build pending map (entries without completion)
            let mut pending_map = HashMap::new();
            let mut completed = std::collections::HashSet::new();

            for entry in entries {
                match entry.entry_type {
                    EntryType::Commit | EntryType::Abort => {
                        if !completed.contains(&entry.tx_id) {
                            pending_map.insert(entry.tx_id, entry);
                        }
                    }
                    EntryType::Complete => {
                        pending_map.remove(&entry.tx_id);
                        completed.insert(entry.tx_id);
                    }
                }
            }

            // Open for appending, making sure to truncate any garbage at the end
            let file = OpenOptions::new()
                .create(true)
                .write(true) // Need write access to truncate
                .append(true)
                .open(&path)
                .map_err(|e| CommitLogError::IoError(format!("Failed to open log: {}", e)))?;

            // Truncate to the valid length to remove any corrupted tail data
            file.set_len(valid_len).map_err(|e| {
                CommitLogError::IoError(format!("Failed to truncate corrupted log tail: {}", e))
            })?;

            (
                Some(BufWriter::new(file)),
                max_lsn + 1,
                pending_map,
                max_tx_id,
            )
        } else {
            // Create new file with header
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)
                .map_err(|e| CommitLogError::IoError(format!("Failed to create log: {}", e)))?;

            let mut writer = BufWriter::new(file);
            Self::write_header(&mut writer)?;

            (Some(writer), 1, HashMap::new(), 0)
        };

        Ok(Self {
            path,
            lsn: AtomicU64::new(lsn),
            writer: RwLock::new(writer),
            pending: RwLock::new(pending),
            config,
            entries_written: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            max_seen_tx_id: AtomicU64::new(max_tx_id),
        })
    }

    /// Create an in-memory commit log (for testing).
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::from(":memory:"),
            lsn: AtomicU64::new(1),
            writer: RwLock::new(None),
            pending: RwLock::new(HashMap::new()),
            config: CommitLogConfig::default(),
            entries_written: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            max_seen_tx_id: AtomicU64::new(0),
        }
    }

    /// Log a commit decision.
    ///
    /// CRITICAL: This must be called BEFORE sending commit messages to participants.
    pub fn log_commit(
        &self,
        tx_id: TxId,
        participants: Vec<ShardId>,
        commit_timestamp: Option<HybridTimestamp>,
    ) -> CommitLogResult<u64> {
        let lsn = self.lsn.fetch_add(1, Ordering::SeqCst);
        let entry = CommitLogEntry::commit(lsn, tx_id, participants, commit_timestamp);
        self.max_seen_tx_id
            .fetch_max(tx_id.as_u64(), Ordering::SeqCst);

        self.write_entry(&entry)?;

        // Add to pending map
        if let Ok(mut pending) = self.pending.write() {
            pending.insert(tx_id, entry);
        }

        Ok(lsn)
    }

    /// Log an abort decision.
    pub fn log_abort(&self, tx_id: TxId, participants: Vec<ShardId>) -> CommitLogResult<u64> {
        let lsn = self.lsn.fetch_add(1, Ordering::SeqCst);
        let entry = CommitLogEntry::abort(lsn, tx_id, participants);
        self.max_seen_tx_id
            .fetch_max(tx_id.as_u64(), Ordering::SeqCst);

        self.write_entry(&entry)?;

        // Add to pending map
        if let Ok(mut pending) = self.pending.write() {
            pending.insert(tx_id, entry);
        }

        Ok(lsn)
    }

    /// Log transaction completion (all participants acknowledged).
    ///
    /// This allows the entry to be garbage collected.
    pub fn log_complete(&self, tx_id: TxId) -> CommitLogResult<u64> {
        let lsn = self.lsn.fetch_add(1, Ordering::SeqCst);
        let entry = CommitLogEntry::complete(lsn, tx_id);
        self.max_seen_tx_id
            .fetch_max(tx_id.as_u64(), Ordering::SeqCst);

        self.write_entry(&entry)?;

        // Remove from pending map
        if let Ok(mut pending) = self.pending.write() {
            pending.remove(&tx_id);
        }

        Ok(lsn)
    }

    /// Get all pending decisions (for recovery).
    pub fn pending_decisions(&self) -> Vec<CommitLogEntry> {
        self.pending
            .read()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Get pending commit decisions.
    pub fn pending_commits(&self) -> Vec<CommitLogEntry> {
        self.pending
            .read()
            .map(|p| {
                p.values()
                    .filter(|e| e.entry_type == EntryType::Commit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get pending abort decisions.
    pub fn pending_aborts(&self) -> Vec<CommitLogEntry> {
        self.pending
            .read()
            .map(|p| {
                p.values()
                    .filter(|e| e.entry_type == EntryType::Abort)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if a transaction has a pending decision.
    pub fn has_pending_decision(&self, tx_id: TxId) -> bool {
        self.pending
            .read()
            .map(|p| p.contains_key(&tx_id))
            .unwrap_or(false)
    }

    /// Get the decision for a transaction.
    pub fn get_decision(&self, tx_id: TxId) -> Option<CommitLogEntry> {
        self.pending.read().ok()?.get(&tx_id).cloned()
    }

    /// Get the maximum transaction ID seen in the log.
    pub fn max_seen_tx_id(&self) -> u64 {
        self.max_seen_tx_id.load(Ordering::SeqCst)
    }

    /// Get the current LSN.
    pub fn current_lsn(&self) -> u64 {
        self.lsn.load(Ordering::SeqCst)
    }

    /// Get statistics.
    pub fn stats(&self) -> CommitLogStats {
        CommitLogStats {
            current_lsn: self.current_lsn(),
            entries_written: self.entries_written.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            pending_count: self.pending.read().map(|p| p.len()).unwrap_or(0),
        }
    }

    /// Sync the log to disk.
    pub fn sync(&self) -> CommitLogResult<()> {
        if let Ok(mut writer_guard) = self.writer.write()
            && let Some(ref mut writer) = *writer_guard
        {
            writer
                .flush()
                .map_err(|e| CommitLogError::IoError(format!("Flush failed: {}", e)))?;

            writer
                .get_ref()
                .sync_all()
                .map_err(|e| CommitLogError::IoError(format!("Sync failed: {}", e)))?;
        }
        Ok(())
    }

    /// Close the log.
    pub fn close(&self) -> CommitLogResult<()> {
        self.sync()?;
        if let Ok(mut writer_guard) = self.writer.write() {
            *writer_guard = None;
        }
        Ok(())
    }

    // ==================== Private Methods ====================

    fn write_header(writer: &mut BufWriter<File>) -> CommitLogResult<()> {
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(&COMMIT_LOG_MAGIC);
        header[4] = COMMIT_LOG_VERSION;
        // Bytes 5-7 reserved
        // Bytes 8-15: entry count (starts at 0)

        writer
            .write_all(&header)
            .map_err(|e| CommitLogError::IoError(format!("Failed to write header: {}", e)))?;

        writer
            .flush()
            .map_err(|e| CommitLogError::IoError(format!("Failed to flush header: {}", e)))?;

        Ok(())
    }

    fn write_entry(&self, entry: &CommitLogEntry) -> CommitLogResult<()> {
        let data = entry.serialize();
        let data_len = data.len() as u64;

        if let Ok(mut writer_guard) = self.writer.write() {
            if let Some(ref mut writer) = *writer_guard {
                writer
                    .write_all(&data)
                    .map_err(|e| CommitLogError::IoError(format!("Write failed: {}", e)))?;

                if self.config.sync_on_write {
                    writer
                        .flush()
                        .map_err(|e| CommitLogError::IoError(format!("Flush failed: {}", e)))?;

                    writer
                        .get_ref()
                        .sync_all()
                        .map_err(|e| CommitLogError::IoError(format!("Sync failed: {}", e)))?;
                }

                self.entries_written.fetch_add(1, Ordering::Relaxed);
                self.bytes_written.fetch_add(data_len, Ordering::Relaxed);
            } else {
                // In-memory mode: just update pending map (already done by caller)
                self.entries_written.fetch_add(1, Ordering::Relaxed);
                self.bytes_written.fetch_add(data_len, Ordering::Relaxed);
            }
        }

        Ok(())
    }

    fn read_entries(path: &Path) -> CommitLogResult<(Vec<CommitLogEntry>, u64, u64, u64)> {
        let file = File::open(path)
            .map_err(|e| CommitLogError::IoError(format!("Failed to open log: {}", e)))?;

        let mut reader = BufReader::new(file);
        let mut buffer = Vec::new();

        reader
            .read_to_end(&mut buffer)
            .map_err(|e| CommitLogError::IoError(format!("Failed to read log: {}", e)))?;

        // Verify header
        if buffer.len() < HEADER_SIZE {
            return Err(CommitLogError::CorruptedLog("File too short".into()));
        }

        if buffer[0..4] != COMMIT_LOG_MAGIC {
            return Err(CommitLogError::CorruptedLog("Invalid magic".into()));
        }

        let version = buffer[4];
        if version > COMMIT_LOG_VERSION {
            return Err(CommitLogError::CorruptedLog(format!(
                "Unsupported version: {}",
                version
            )));
        }

        // Parse entries
        let mut entries = Vec::new();
        let mut offset = HEADER_SIZE;
        let mut max_lsn = 0u64;
        let mut max_tx_id = 0u64;

        while offset < buffer.len() {
            if offset + 4 > buffer.len() {
                break; // Incomplete length prefix, truncated file
            }

            let len = u32::from_le_bytes([
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
            ]) as usize;

            if offset + 4 + len > buffer.len() {
                break; // Incomplete entry, truncated file
            }

            match CommitLogEntry::deserialize(&buffer[offset..]) {
                Ok(entry) => {
                    if entry.lsn > max_lsn {
                        max_lsn = entry.lsn;
                    }
                    let tx_id_val = entry.tx_id.as_u64();
                    if tx_id_val > max_tx_id {
                        max_tx_id = tx_id_val;
                    }
                    entries.push(entry);
                    offset += 4 + len;
                }
                Err(_) => {
                    // Skip corrupted entry at end of file
                    break;
                }
            }
        }

        Ok((entries, max_lsn, max_tx_id, offset as u64))
    }
}

/// Statistics for the commit log.
#[derive(Debug, Clone)]
pub struct CommitLogStats {
    /// Current LSN.
    pub current_lsn: u64,
    /// Total entries written.
    pub entries_written: u64,
    /// Total bytes written.
    pub bytes_written: u64,
    /// Number of pending decisions.
    pub pending_count: usize,
}

/// Compute CRC32 checksum.
fn compute_checksum(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

/// Get current timestamp in microseconds.
fn current_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_shard_id(id: u16) -> ShardId {
        ShardId::new(id).unwrap()
    }

    // ==================== Entry Tests ====================

    #[test]
    fn test_entry_serialize_deserialize_commit() {
        let ts = HybridTimestamp::new(1000, 0).ok();
        let entry = CommitLogEntry::commit(
            1,
            TxId::new(100),
            vec![make_shard_id(0), make_shard_id(1)],
            ts,
        );

        let serialized = entry.serialize();
        let deserialized = CommitLogEntry::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.lsn, 1);
        assert_eq!(deserialized.entry_type, EntryType::Commit);
        assert_eq!(deserialized.tx_id, TxId::new(100));
        assert_eq!(deserialized.participants.len(), 2);
        assert_eq!(deserialized.commit_timestamp, ts);
    }

    #[test]
    fn test_entry_serialize_deserialize_commit_no_ts() {
        let entry = CommitLogEntry::commit(
            1,
            TxId::new(100),
            vec![make_shard_id(0), make_shard_id(1)],
            None,
        );

        let serialized = entry.serialize();
        let deserialized = CommitLogEntry::deserialize(&serialized).unwrap();

        assert!(deserialized.commit_timestamp.is_none());
    }

    #[test]
    fn test_entry_serialize_deserialize_abort() {
        let entry = CommitLogEntry::abort(2, TxId::new(200), vec![make_shard_id(3)]);

        let serialized = entry.serialize();
        let deserialized = CommitLogEntry::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.lsn, 2);
        assert_eq!(deserialized.entry_type, EntryType::Abort);
        assert_eq!(deserialized.tx_id, TxId::new(200));
        assert!(deserialized.commit_timestamp.is_none());
    }

    #[test]
    fn test_entry_serialize_deserialize_complete() {
        let entry = CommitLogEntry::complete(3, TxId::new(300));

        let serialized = entry.serialize();
        let deserialized = CommitLogEntry::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.lsn, 3);
        assert_eq!(deserialized.entry_type, EntryType::Complete);
        assert!(deserialized.participants.is_empty());
    }

    #[test]
    fn test_entry_checksum_validation() {
        let entry = CommitLogEntry::commit(1, TxId::new(100), vec![make_shard_id(0)], None);
        let mut serialized = entry.serialize();

        // Corrupt the data
        if serialized.len() > 10 {
            serialized[10] ^= 0xFF;
        }

        let result = CommitLogEntry::deserialize(&serialized);
        assert!(matches!(
            result,
            Err(CommitLogError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn test_entry_invalid_type() {
        let mut data = vec![
            // Length: 30 bytes
            30, 0, 0, 0,  // Entry type: invalid (99)
            99, // LSN
            1, 0, 0, 0, 0, 0, 0, 0, // TxId
            100, 0, 0, 0, 0, 0, 0, 0, // Timestamp
            0, 0, 0, 0, 0, 0, 0, 0, // Participant count: 0
            0, 0,
        ];

        // Add a fake checksum (won't match but we'll hit invalid type first)
        let checksum = compute_checksum(&data[4..]);
        data.extend_from_slice(&checksum.to_le_bytes());

        // Update length
        let len = (data.len() - 4) as u32;
        data[0..4].copy_from_slice(&len.to_le_bytes());

        let result = CommitLogEntry::deserialize(&data);
        assert!(matches!(result, Err(CommitLogError::InvalidEntry(_))));
    }

    // ==================== In-Memory Log Tests ====================

    #[test]
    fn test_in_memory_log() {
        let log = PersistentCommitLog::in_memory();

        let lsn = log
            .log_commit(TxId::new(1), vec![make_shard_id(0), make_shard_id(1)], None)
            .unwrap();
        assert_eq!(lsn, 1);

        assert!(log.has_pending_decision(TxId::new(1)));
        assert!(!log.has_pending_decision(TxId::new(2)));

        let pending = log.pending_commits();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tx_id, TxId::new(1));
    }

    #[test]
    fn test_in_memory_log_complete() {
        let log = PersistentCommitLog::in_memory();

        log.log_commit(TxId::new(1), vec![make_shard_id(0)], None)
            .unwrap();
        assert!(log.has_pending_decision(TxId::new(1)));

        log.log_complete(TxId::new(1)).unwrap();
        assert!(!log.has_pending_decision(TxId::new(1)));
    }

    #[test]
    fn test_in_memory_log_stats() {
        let log = PersistentCommitLog::in_memory();

        log.log_commit(TxId::new(1), vec![make_shard_id(0)], None)
            .unwrap();
        log.log_abort(TxId::new(2), vec![make_shard_id(1)]).unwrap();

        let stats = log.stats();
        assert_eq!(stats.entries_written, 2);
        assert_eq!(stats.pending_count, 2);
    }

    // ==================== Persistent Log Tests ====================

    #[test]
    fn test_persistent_log_create() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("commit.log");

        let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
        assert_eq!(log.current_lsn(), 1);
        drop(log);

        assert!(path.exists());
    }

    #[test]
    fn test_persistent_log_write_read() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("commit.log");

        // Write some entries
        {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            log.log_commit(TxId::new(1), vec![make_shard_id(0), make_shard_id(1)], None)
                .unwrap();
            log.log_abort(TxId::new(2), vec![make_shard_id(2)]).unwrap();
            log.close().unwrap();
        }

        // Reopen and verify
        {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            let pending = log.pending_decisions();
            assert_eq!(pending.len(), 2);

            let commits = log.pending_commits();
            assert_eq!(commits.len(), 1);
            assert_eq!(commits[0].tx_id, TxId::new(1));

            let aborts = log.pending_aborts();
            assert_eq!(aborts.len(), 1);
            assert_eq!(aborts[0].tx_id, TxId::new(2));
        }
    }

    #[test]
    fn test_persistent_log_recovery_with_completion() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("commit.log");

        // Write entries including completion
        {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            log.log_commit(TxId::new(1), vec![make_shard_id(0)], None)
                .unwrap();
            log.log_commit(TxId::new(2), vec![make_shard_id(1)], None)
                .unwrap();
            log.log_complete(TxId::new(1)).unwrap();
            log.close().unwrap();
        }

        // Reopen and verify only tx 2 is pending
        {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            let pending = log.pending_decisions();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].tx_id, TxId::new(2));
        }
    }

    #[test]
    fn test_persistent_log_lsn_continuity() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("commit.log");

        // Write some entries
        let max_lsn = {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            log.log_commit(TxId::new(1), vec![make_shard_id(0)], None)
                .unwrap();
            log.log_commit(TxId::new(2), vec![make_shard_id(1)], None)
                .unwrap();
            let lsn = log.current_lsn();
            log.close().unwrap();
            lsn
        };

        // Reopen and verify LSN continues
        {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            let new_lsn = log
                .log_commit(TxId::new(3), vec![make_shard_id(2)], None)
                .unwrap();
            assert!(new_lsn >= max_lsn);
        }
    }

    #[test]
    fn test_persistent_log_get_decision() {
        let log = PersistentCommitLog::in_memory();

        log.log_commit(TxId::new(1), vec![make_shard_id(0), make_shard_id(1)], None)
            .unwrap();

        let decision = log.get_decision(TxId::new(1));
        assert!(decision.is_some());
        let decision = decision.unwrap();
        assert_eq!(decision.entry_type, EntryType::Commit);
        assert_eq!(decision.participants.len(), 2);

        assert!(log.get_decision(TxId::new(999)).is_none());
    }

    // ==================== Error Tests ====================

    #[test]
    fn test_commit_log_error_display() {
        let err = CommitLogError::IoError("test".to_string());
        assert!(format!("{}", err).contains("I/O error"));

        let err = CommitLogError::CorruptedLog("bad data".to_string());
        assert!(format!("{}", err).contains("Corrupted"));

        let err = CommitLogError::TransactionNotFound(TxId::new(42));
        assert!(format!("{}", err).contains("42"));

        let err = CommitLogError::ChecksumMismatch {
            expected: 100,
            actual: 200,
        };
        assert!(format!("{}", err).contains("Checksum"));
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use tempfile::TempDir;
    use std::io::Write;
    use std::fs::OpenOptions;

    fn make_shard_id(id: u16) -> ShardId {
        ShardId::new(id).unwrap()
    }

    #[test]
    fn test_persistent_log_truncation_on_recovery() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("commit.log");

        // 1. Create log and write Entry 1
        {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            log.log_commit(TxId::new(1), vec![make_shard_id(0)], None).unwrap();
            log.close().unwrap();
        }

        // 2. Corrupt the log by appending garbage
        {
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"GARBAGE_DATA_12345").unwrap();
            file.sync_all().unwrap();
        }

        // 3. Reopen log (should recover Entry 1 and ignore garbage) and write Entry 2
        {
            // This currently fails/panics or writes Entry 2 AFTER the garbage
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();

            // Verify we see Entry 1
            let pending = log.pending_decisions();
            assert_eq!(pending.len(), 1, "Should have recovered Entry 1");
            assert_eq!(pending[0].tx_id, TxId::new(1));

            // Write Entry 2
            log.log_commit(TxId::new(2), vec![make_shard_id(0)], None).unwrap();
            log.close().unwrap();
        }

        // 4. Reopen again and verify BOTH entries exist
        {
            let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
            let pending = log.pending_decisions();

            // If the file wasn't truncated, the reader will stop at the garbage
            // and never see Entry 2, so count will be 1 instead of 2.
            assert_eq!(pending.len(), 2, "Should have recovered both entries");
            assert_eq!(pending[0].tx_id, TxId::new(1));
            assert_eq!(pending[1].tx_id, TxId::new(2));
        }
    }
}
