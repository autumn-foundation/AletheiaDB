//! Flush coordinator for the concurrent WAL.
//!
//! The flush coordinator is responsible for:
//! - Draining entries from all stripes
//! - Sorting entries by LSN to restore global order
//! - Writing entries to segment files
//! - Performing fsync based on durability mode
//! - Notifying completion handles after durable writes
//!
//! # Architecture
//!
//! The coordinator runs as a background thread that periodically flushes
//! pending entries. It can also be triggered immediately for sync commits.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                  Flush Coordinator                       │
//! │  ┌─────────────────────────────────────────────────┐   │
//! │  │  1. Drain all stripes                            │   │
//! │  │  2. Sort by LSN                                  │   │
//! │  │  3. Write to segment file                        │   │
//! │  │  4. fsync (if required by durability mode)       │   │
//! │  │  5. Notify completion handles                    │   │
//! │  └─────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Thread Safety
//!
//! The coordinator is designed to be run from a single thread. Multiple
//! threads should not call `flush()` concurrently.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::LSN;
use super::ring_buffer::PendingEntry;

use crate::core::error::{Error, Result, StorageError};

/// Magic bytes identifying a AletheiaDB WAL segment file.
const WAL_MAGIC: [u8; 4] = *b"GWAL";

/// Current WAL format version.
const WAL_VERSION: u8 = 1;

/// Size of the WAL segment header (magic + version).
const WAL_HEADER_SIZE: usize = 5;

/// Metadata about a WAL segment's LSN range.
///
/// This is stored in a companion `.meta` file for each segment to enable
/// efficient LSN-based truncation without reading the entire segment.
#[derive(Debug, Clone)]
pub struct SegmentMetadata {
    /// Minimum LSN in this segment.
    pub min_lsn: LSN,
    /// Maximum LSN in this segment.
    pub max_lsn: LSN,
    /// Number of entries in this segment.
    pub entry_count: u64,
}

impl SegmentMetadata {
    /// Create new segment metadata.
    pub fn new(min_lsn: LSN, max_lsn: LSN, entry_count: u64) -> Self {
        Self {
            min_lsn,
            max_lsn,
            entry_count,
        }
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(24);
        bytes.extend_from_slice(&self.min_lsn.0.to_le_bytes());
        bytes.extend_from_slice(&self.max_lsn.0.to_le_bytes());
        bytes.extend_from_slice(&self.entry_count.to_le_bytes());
        bytes
    }

    /// Deserialize from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 24 {
            return None;
        }
        let min_lsn = LSN(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]));
        let max_lsn = LSN(u64::from_le_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]));
        let entry_count = u64::from_le_bytes([
            bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23],
        ]);
        Some(Self {
            min_lsn,
            max_lsn,
            entry_count,
        })
    }
}

/// Configuration for the flush coordinator.
#[derive(Debug, Clone)]
pub struct FlushCoordinatorConfig {
    /// WAL directory path.
    pub wal_dir: PathBuf,
    /// Maximum segment size in bytes before rotation.
    pub segment_size: usize,
    /// Number of segments to retain.
    pub segments_to_retain: usize,
    /// Flush interval in milliseconds (for background thread).
    pub flush_interval_ms: u64,
    /// Whether to fsync after each flush.
    pub sync_on_flush: bool,
    /// Write buffer size for segment files.
    pub write_buffer_size: usize,
}

impl Default for FlushCoordinatorConfig {
    fn default() -> Self {
        Self {
            wal_dir: PathBuf::from("data/wal"),
            segment_size: 64 * 1024 * 1024, // 64 MB
            segments_to_retain: 10,
            flush_interval_ms: 10, // 10ms
            sync_on_flush: true,
            write_buffer_size: 64 * 1024, // 64 KB
        }
    }
}

impl FlushCoordinatorConfig {
    /// Create a new config with the specified WAL directory.
    pub fn new(wal_dir: impl Into<PathBuf>) -> Self {
        Self {
            wal_dir: wal_dir.into(),
            ..Default::default()
        }
    }
}

/// Statistics from a single flush operation.
#[derive(Debug, Clone, Default)]
pub struct FlushStats {
    /// Number of entries flushed.
    pub entries_flushed: usize,
    /// Bytes written.
    pub bytes_written: usize,
    /// Time spent flushing (including fsync).
    pub flush_duration: Duration,
    /// Whether segment was rotated.
    pub segment_rotated: bool,
}

/// Flush coordinator for writing WAL entries to disk.
///
/// This struct manages segment files and coordinates flushing entries
/// from the concurrent WAL stripes to disk.
///
/// # LSN Tracking (ADR-0025)
///
/// The coordinator tracks min/max LSN for each segment to enable safe
/// WAL truncation. When a segment is closed, a companion `.meta` file is
/// written with the LSN range. This enables `truncate_to_lsn()` to determine
/// which segments can be safely removed.
///
/// # Mutex Poisoning Recovery
///
/// The coordinator uses `unwrap_or_else(|e| e.into_inner())` when acquiring
/// mutex locks on `writer` and `sync_handle`. This pattern recovers from
/// poisoned mutexes because:
///
/// 1. **Single-threaded access**: The flush coordinator is accessed by only
///    one flush thread at a time. Mutex poisoning would only occur if the
///    flush thread itself panicked during a previous operation.
///
/// 2. **File handle recovery**: If a panic occurred while holding the writer
///    or sync_handle lock, the underlying file handles are still valid. The
///    OS will have either completed or rolled back any in-progress writes.
///
/// 3. **Idempotent operations**: Flush operations are designed to be safe
///    to retry. Re-acquiring a poisoned lock and continuing is preferable
///    to propagating a panic to the caller.
///
/// 4. **Crash consistency**: The WAL already handles crash recovery at the
///    entry level via checksums. A panic during flush is treated the same
///    as a crash - entries are either fully written or not.
pub struct FlushCoordinator {
    /// Configuration.
    config: FlushCoordinatorConfig,
    /// Current segment ID.
    current_segment_id: AtomicU64,
    /// Current segment size.
    current_segment_size: AtomicU64,
    /// Current segment writer.
    writer: Mutex<Option<BufWriter<File>>>,
    /// Sync handle for fsync (separate from writer).
    sync_handle: Mutex<Option<File>>,
    /// Total entries flushed.
    total_entries_flushed: AtomicU64,
    /// Total bytes written.
    total_bytes_written: AtomicU64,
    /// Total flushes performed.
    total_flushes: AtomicU64,
    /// Minimum LSN in the current segment (for metadata tracking).
    current_segment_min_lsn: AtomicU64,
    /// Maximum LSN in the current segment (for metadata tracking).
    current_segment_max_lsn: AtomicU64,
    /// Entry count in the current segment.
    current_segment_entry_count: AtomicU64,
    /// Next expected LSN to flush (for sequential ordering).
    next_expected_lsn: AtomicU64,
    /// Buffer for out-of-order entries (waiting for gaps to fill).
    pending_buffer: Mutex<BTreeMap<u64, PendingEntry>>,
}

impl FlushCoordinator {
    /// Create a new flush coordinator.
    pub fn new(config: FlushCoordinatorConfig) -> Result<Self> {
        // Ensure WAL directory exists
        std::fs::create_dir_all(&config.wal_dir).map_err(|e| {
            Error::Storage(StorageError::IoError(format!(
                "Failed to create WAL directory: {}",
                e
            )))
        })?;

        let coordinator = Self {
            config,
            current_segment_id: AtomicU64::new(0),
            current_segment_size: AtomicU64::new(0),
            writer: Mutex::new(None),
            sync_handle: Mutex::new(None),
            total_entries_flushed: AtomicU64::new(0),
            total_bytes_written: AtomicU64::new(0),
            total_flushes: AtomicU64::new(0),
            current_segment_min_lsn: AtomicU64::new(u64::MAX),
            current_segment_max_lsn: AtomicU64::new(0),
            current_segment_entry_count: AtomicU64::new(0),
            next_expected_lsn: AtomicU64::new(1),
            pending_buffer: Mutex::new(BTreeMap::new()),
        };

        // Find the latest segment ID
        coordinator.initialize_from_existing()?;

        Ok(coordinator)
    }

    /// Initialize from existing WAL segments.
    fn initialize_from_existing(&self) -> Result<()> {
        let mut max_segment_id = 0u64;

        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(id) = path
                    .extension()
                    .filter(|ext| *ext == "log")
                    .and_then(|_| path.file_stem())
                    .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                {
                    max_segment_id = max_segment_id.max(id);
                }
            }
        }

        // Initialize next_expected_lsn from the latest segment
        let mut max_lsn = 0;
        if max_segment_id > 0 {
            if let Some(meta) = self.read_segment_metadata(max_segment_id) {
                max_lsn = meta.max_lsn.0;
            } else {
                // Fallback: Read segment to find max LSN
                let path = self.segment_path(max_segment_id);
                if let Ok(entries) = super::segment_reader::read_segment(&path, LSN(0)) {
                    if let Some(last) = entries.last() {
                        max_lsn = last.lsn.0;
                    }
                }
            }
        }

        self.next_expected_lsn.store(max_lsn + 1, Ordering::Relaxed);

        self.current_segment_id
            .store(max_segment_id, Ordering::Relaxed);
        Ok(())
    }

    /// Get the path for a segment file.
    fn segment_path(&self, segment_id: u64) -> PathBuf {
        self.config.wal_dir.join(format!("{:06}.log", segment_id))
    }

    /// Get the path for a segment's metadata file.
    fn segment_meta_path(&self, segment_id: u64) -> PathBuf {
        self.config
            .wal_dir
            .join(format!("{:06}.log.meta", segment_id))
    }

    /// Write segment metadata to a companion file.
    fn write_segment_metadata(&self, segment_id: u64) -> Result<()> {
        let min_lsn = self.current_segment_min_lsn.load(Ordering::Relaxed);
        let max_lsn = self.current_segment_max_lsn.load(Ordering::Relaxed);
        let entry_count = self.current_segment_entry_count.load(Ordering::Relaxed);

        // Only write metadata if we have valid LSN range
        if min_lsn <= max_lsn && entry_count > 0 {
            let metadata = SegmentMetadata::new(LSN(min_lsn), LSN(max_lsn), entry_count);
            let meta_path = self.segment_meta_path(segment_id);
            let bytes = metadata.to_bytes();

            std::fs::write(&meta_path, &bytes).map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to write segment metadata: {}",
                    e
                )))
            })?;
        }

        Ok(())
    }

    /// Read segment metadata from a companion file.
    pub fn read_segment_metadata(&self, segment_id: u64) -> Option<SegmentMetadata> {
        let meta_path = self.segment_meta_path(segment_id);
        let mut bytes = Vec::new();
        File::open(&meta_path).ok()?.read_to_end(&mut bytes).ok()?;
        SegmentMetadata::from_bytes(&bytes)
    }

    /// Open or create the current segment file.
    fn ensure_segment_open(&self, writer_guard: &mut Option<BufWriter<File>>) -> Result<()> {
        if writer_guard.is_some() {
            return Ok(());
        }

        // Increment segment ID for new segment
        let segment_id = self.current_segment_id.fetch_add(1, Ordering::Relaxed) + 1;
        let path = self.segment_path(segment_id);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to open WAL segment {}: {}",
                    path.display(),
                    e
                )))
            })?;

        // Clone handle for sync
        let sync_file = file.try_clone().map_err(|e| {
            Error::Storage(StorageError::IoError(format!(
                "Failed to clone WAL file handle: {}",
                e
            )))
        })?;

        let mut writer = BufWriter::with_capacity(self.config.write_buffer_size, file);

        // Write header for new segment
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) == 0 {
            writer.write_all(&WAL_MAGIC).map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to write WAL header: {}",
                    e
                )))
            })?;
            writer.write_all(&[WAL_VERSION]).map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to write WAL version: {}",
                    e
                )))
            })?;
            self.current_segment_size
                .store(WAL_HEADER_SIZE as u64, Ordering::Relaxed);
        }

        *writer_guard = Some(writer);

        let mut sync_guard = self.sync_handle.lock().unwrap_or_else(|e| e.into_inner());
        *sync_guard = Some(sync_file);

        self.current_segment_id.store(segment_id, Ordering::Relaxed);

        Ok(())
    }

    /// Rotate to a new segment if current exceeds size limit.
    ///
    /// # Thread Safety
    ///
    /// This method is called while holding the `writer` lock from `flush()`.
    /// This ensures that segment rotation is atomic with respect to other
    /// flush operations.
    fn maybe_rotate_segment(&self, writer_guard: &mut Option<BufWriter<File>>) -> Result<bool> {
        let current_size = self.current_segment_size.load(Ordering::Relaxed);

        if current_size >= self.config.segment_size as u64 {
            let closing_segment_id = self.current_segment_id.load(Ordering::Relaxed);

            // Flush current segment
            if let Some(writer) = writer_guard {
                writer.flush().map_err(|e| {
                    Error::Storage(StorageError::IoError(format!(
                        "Failed to flush WAL segment: {}",
                        e
                    )))
                })?;
            }
            // Close writer (drops BufWriter)
            *writer_guard = None;

            // Sync before closing
            if self.config.sync_on_flush {
                let sync_guard = self.sync_handle.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref sync_file) = *sync_guard {
                    sync_file.sync_data().map_err(|e| {
                        Error::Storage(StorageError::IoError(format!(
                            "Failed to sync WAL segment: {}",
                            e
                        )))
                    })?;
                }
            }

            // Write segment metadata before closing (ADR-0025)
            self.write_segment_metadata(closing_segment_id)?;

            // Clear sync handle
            {
                let mut sync_guard = self.sync_handle.lock().unwrap_or_else(|e| e.into_inner());
                *sync_guard = None;
            }

            // Reset size and LSN tracking for new segment
            self.current_segment_size.store(0, Ordering::Relaxed);
            self.current_segment_min_lsn
                .store(u64::MAX, Ordering::Relaxed);
            self.current_segment_max_lsn.store(0, Ordering::Relaxed);
            self.current_segment_entry_count.store(0, Ordering::Relaxed);

            // Clean up old segments
            self.cleanup_old_segments()?;

            return Ok(true);
        }

        Ok(false)
    }

    /// Clean up old segments beyond retention policy.
    fn cleanup_old_segments(&self) -> Result<()> {
        let current_id = self.current_segment_id.load(Ordering::Relaxed);
        let retain_from = current_id.saturating_sub(self.config.segments_to_retain as u64);

        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                // Check for .log files
                let is_old_segment = path.extension().is_some_and(|ext| ext == "log")
                    && path
                        .file_stem()
                        .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                        .is_some_and(|id| id < retain_from);

                // Check for .log.meta files
                let is_old_meta = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| {
                        name.ends_with(".log.meta")
                            && name
                                .strip_suffix(".log.meta")
                                .and_then(|s| s.parse::<u64>().ok())
                                .is_some_and(|id| id < retain_from)
                    });

                if is_old_segment || is_old_meta {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }

        Ok(())
    }

    /// Truncate WAL segments up to the specified LSN.
    ///
    /// This removes all segments where `max_lsn < truncate_lsn`. The current
    /// active segment is never removed.
    ///
    /// # Arguments
    ///
    /// * `truncate_lsn` - Remove segments with max_lsn strictly less than this
    ///
    /// # Returns
    ///
    /// The number of segments removed.
    ///
    /// # Safety
    ///
    /// This method should only be called after confirming that all operations
    /// up to `truncate_lsn` have been durably persisted to cold storage.
    /// The key invariant is: `truncate_lsn <= cold_storage.get_flushed_lsn()`
    pub fn truncate_to_lsn(&self, truncate_lsn: LSN) -> Result<usize> {
        let current_id = self.current_segment_id.load(Ordering::Relaxed);
        let mut removed_count = 0;

        // Collect segments to check
        let mut segments_to_check: Vec<(u64, PathBuf)> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log")
                    && let Some(segment_id) = path
                        .file_stem()
                        .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                    && segment_id < current_id
                {
                    segments_to_check.push((segment_id, path));
                }
            }
        }

        // Check each segment's metadata and remove if max_lsn < truncate_lsn
        for (segment_id, path) in segments_to_check {
            let should_remove = if let Some(metadata) = self.read_segment_metadata(segment_id) {
                // Remove if all entries in segment are before truncate point
                metadata.max_lsn.0 < truncate_lsn.0
            } else {
                // No metadata file - be conservative and don't remove
                // (This could happen for old segments created before LSN tracking)
                false
            };

            if should_remove {
                // Remove segment file
                if std::fs::remove_file(&path).is_ok() {
                    removed_count += 1;
                }
                // Remove metadata file
                let meta_path = self.segment_meta_path(segment_id);
                let _ = std::fs::remove_file(&meta_path);
            }
        }

        Ok(removed_count)
    }

    /// Get information about all WAL segments.
    ///
    /// Returns a list of (segment_id, metadata) for all segments that have
    /// metadata files. Segments without metadata are not included.
    pub fn list_segments_with_metadata(&self) -> Vec<(u64, SegmentMetadata)> {
        let mut segments = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log")
                    && let Some(segment_id) = path
                        .file_stem()
                        .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                    && let Some(metadata) = self.read_segment_metadata(segment_id)
                {
                    segments.push((segment_id, metadata));
                }
            }
        }

        segments.sort_by_key(|(id, _)| *id);
        segments
    }

    /// Get the minimum LSN that is still in the WAL.
    ///
    /// This can be used to determine what LSN to start recovery from.
    /// Returns `None` if no segments exist or no segments have metadata.
    pub fn get_min_lsn(&self) -> Option<LSN> {
        self.list_segments_with_metadata()
            .into_iter()
            .map(|(_, meta)| meta.min_lsn)
            .min()
    }

    /// Set the next expected LSN (used during recovery).
    pub fn set_expected_lsn(&self, lsn: LSN) {
        self.next_expected_lsn.store(lsn.0, Ordering::Relaxed);
    }

    /// Flush a batch of entries to disk.
    ///
    /// Entries should already be sorted by LSN.
    ///
    /// # Arguments
    ///
    /// * `entries` - Entries to flush (will be consumed)
    /// * `sync` - Whether to fsync after writing
    ///
    /// # Returns
    ///
    /// Statistics about the flush operation.
    pub fn flush(&self, mut entries: Vec<PendingEntry>, sync: bool) -> Result<FlushStats> {
        // Ensure entries are sorted by LSN (critical for gap detection)
        entries.sort_by_key(|e| e.lsn);

        // Lock the pending buffer to ensure serialization of the flush decision and update
        let mut pending = self
            .pending_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let mut batch_to_write = Vec::new();
        let mut next = self.next_expected_lsn.load(Ordering::Relaxed);

        // Process incoming entries
        for entry in entries {
            let lsn = entry.lsn.0;
            if lsn == next {
                batch_to_write.push(entry);
                next += 1;

                // Check if we can fill more from buffer
                while let Some(entry) = pending.remove(&next) {
                    batch_to_write.push(entry);
                    next += 1;
                }
            } else if lsn > next {
                // Future entry, buffer it
                pending.insert(lsn, entry);
            } else {
                // Duplicate or old entry (lsn < next)
                entry.notify_completion();
            }
        }

        if batch_to_write.is_empty() {
            return Ok(FlushStats::default());
        }

        let start = Instant::now();

        // Inner function to perform I/O operations.
        let do_flush = |batch: &[PendingEntry]| -> Result<FlushStats> {
            // Acquire lock once for the entire operation to ensure thread safety
            let mut writer_guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());

            // Ensure segment is open
            self.ensure_segment_open(&mut writer_guard)?;

            let mut bytes_written = 0usize;

            // Track LSN range for this batch (ADR-0025)
            let mut batch_min_lsn = u64::MAX;
            let mut batch_max_lsn = 0u64;

            // Write all entries
            {
                let writer = writer_guard.as_mut().ok_or_else(|| {
                    Error::Storage(StorageError::WalError {
                        reason: "WAL writer not initialized".to_string(),
                    })
                })?;

                for entry in batch {
                    // Track LSN range
                    batch_min_lsn = batch_min_lsn.min(entry.lsn.0);
                    batch_max_lsn = batch_max_lsn.max(entry.lsn.0);

                    writer.write_all(&entry.data).map_err(|e| {
                        Error::Storage(StorageError::IoError(format!(
                            "Failed to write WAL entry: {}",
                            e
                        )))
                    })?;
                    bytes_written += entry.data.len();
                }

                // Flush buffer to OS
                writer.flush().map_err(|e| {
                    Error::Storage(StorageError::IoError(format!(
                        "Failed to flush WAL buffer: {}",
                        e
                    )))
                })?;
            }

            // Update segment LSN tracking (ADR-0025)
            // Use fetch_min/fetch_max for atomic updates
            self.current_segment_min_lsn
                .fetch_min(batch_min_lsn, Ordering::Relaxed);
            self.current_segment_max_lsn
                .fetch_max(batch_max_lsn, Ordering::Relaxed);
            self.current_segment_entry_count
                .fetch_add(batch.len() as u64, Ordering::Relaxed);

            // Sync to disk if requested
            if sync && self.config.sync_on_flush {
                let sync_guard = self.sync_handle.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref sync_file) = *sync_guard {
                    sync_file.sync_data().map_err(|e| {
                        Error::Storage(StorageError::IoError(format!("Failed to sync WAL: {}", e)))
                    })?;
                }
            }

            // Update size
            self.current_segment_size
                .fetch_add(bytes_written as u64, Ordering::Relaxed);

            // Check for rotation
            let segment_rotated = self.maybe_rotate_segment(&mut writer_guard)?;

            // Update metrics
            self.total_entries_flushed
                .fetch_add(batch.len() as u64, Ordering::Relaxed);
            self.total_bytes_written
                .fetch_add(bytes_written as u64, Ordering::Relaxed);
            self.total_flushes.fetch_add(1, Ordering::Relaxed);

            Ok(FlushStats {
                entries_flushed: batch.len(),
                bytes_written,
                flush_duration: start.elapsed(),
                segment_rotated,
            })
        };

        // Execute flush logic holding the pending_buffer lock to ensure atomicity
        match do_flush(&batch_to_write) {
            Ok(stats) => {
                // Success: Update state and notify
                self.next_expected_lsn.store(next, Ordering::Relaxed);

                // Release lock
                drop(pending);

                for entry in batch_to_write {
                    entry.notify_completion();
                }
                Ok(stats)
            }
            Err(e) => {
                // Failure: Do NOT update state.
                // Entries in batch_to_write are dropped and treated as failed.
                // Callers can retry.
                drop(pending);

                let error_msg = e.to_string();
                for entry in batch_to_write {
                    entry.notify_error(&error_msg);
                }
                Err(e)
            }
        }
    }

    /// Get total entries flushed.
    #[inline]
    pub fn total_entries_flushed(&self) -> u64 {
        self.total_entries_flushed.load(Ordering::Relaxed)
    }

    /// Get total bytes written.
    #[inline]
    pub fn total_bytes_written(&self) -> u64 {
        self.total_bytes_written.load(Ordering::Relaxed)
    }

    /// Get total flushes performed.
    #[inline]
    pub fn total_flushes(&self) -> u64 {
        self.total_flushes.load(Ordering::Relaxed)
    }

    /// Get current segment ID.
    #[inline]
    pub fn current_segment_id(&self) -> u64 {
        self.current_segment_id.load(Ordering::Relaxed)
    }

    /// Get current segment size.
    #[inline]
    pub fn current_segment_size(&self) -> u64 {
        self.current_segment_size.load(Ordering::Relaxed)
    }

    /// Get the WAL directory.
    pub fn wal_dir(&self) -> &Path {
        &self.config.wal_dir
    }
}

/// Signal for requesting immediate flush.
pub struct FlushSignal {
    /// Flag indicating flush is requested.
    requested: AtomicBool,
    /// Mutex for condvar.
    mutex: Mutex<()>,
    /// Condition variable for waiting.
    condvar: Condvar,
}

impl FlushSignal {
    /// Create a new flush signal.
    pub fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            mutex: Mutex::new(()),
            condvar: Condvar::new(),
        }
    }

    /// Request an immediate flush.
    pub fn request_flush(&self) {
        self.requested.store(true, Ordering::Release);
        self.condvar.notify_all();
    }

    /// Check if flush was requested and clear the flag.
    pub fn take_request(&self) -> bool {
        self.requested.swap(false, Ordering::AcqRel)
    }

    /// Wait for flush request with timeout.
    ///
    /// Returns true if a flush was requested, false if timeout occurred.
    /// Handles spurious wakeups by checking the actual requested flag.
    pub fn wait_for_request(&self, timeout: Duration) -> bool {
        let guard = self.mutex.lock().unwrap_or_else(|e| e.into_inner());

        if self.requested.load(Ordering::Acquire) {
            return true;
        }

        let (_guard, _result) = self
            .condvar
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|e| e.into_inner());

        // Only return true if actually requested, regardless of spurious wakeups
        // or timeout status. This is the source of truth.
        self.requested.load(Ordering::Acquire)
    }
}

impl Default for FlushSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Background flush thread handle.
pub struct FlushThread {
    /// Thread handle.
    handle: Option<JoinHandle<()>>,
    /// Shutdown signal.
    shutdown: Arc<AtomicBool>,
    /// Flush signal.
    flush_signal: Arc<FlushSignal>,
}

impl FlushThread {
    /// Start a new background flush thread.
    ///
    /// The thread will periodically drain entries from the provided drain
    /// function and flush them to the coordinator.
    pub fn start<F>(
        coordinator: Arc<FlushCoordinator>,
        drain_fn: F,
        flush_interval: Duration,
    ) -> Self
    where
        F: Fn() -> Vec<PendingEntry> + Send + 'static,
    {
        let shutdown = Arc::new(AtomicBool::new(false));
        let flush_signal = Arc::new(FlushSignal::new());

        let shutdown_clone = Arc::clone(&shutdown);
        let signal_clone = Arc::clone(&flush_signal);

        let handle = thread::spawn(move || {
            while !shutdown_clone.load(Ordering::Acquire) {
                // Wait for flush interval or signal
                let _ = signal_clone.wait_for_request(flush_interval);

                // Clear any pending request
                signal_clone.take_request();

                // Drain and flush
                let entries = drain_fn();
                if let Err(e) = coordinator.flush(entries, true) {
                    eprintln!("WAL flush error: {:?}", e);
                }
            }

            // Final flush on shutdown
            let entries = drain_fn();
            let _ = coordinator.flush(entries, true);
        });

        Self {
            handle: Some(handle),
            shutdown,
            flush_signal,
        }
    }

    /// Request an immediate flush.
    pub fn request_flush(&self) {
        self.flush_signal.request_flush();
    }

    /// Shutdown the flush thread.
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.flush_signal.request_flush(); // Wake up the thread

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for FlushThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::super::LSN;
    use super::*;
    use tempfile::tempdir;

    fn create_test_entry(lsn: u64, data: &[u8]) -> PendingEntry {
        PendingEntry::new_async(LSN(lsn), data.to_vec())
    }

    // ============================================================
    // TDD Tests - Written FIRST to define expected behavior
    // ============================================================

    #[test]
    fn test_flush_coordinator_creation() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        assert_eq!(coordinator.total_entries_flushed(), 0);
        assert_eq!(coordinator.total_bytes_written(), 0);
        assert_eq!(coordinator.total_flushes(), 0);
    }

    #[test]
    fn test_flush_empty_entries() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        let stats = coordinator.flush(vec![], true).unwrap();

        assert_eq!(stats.entries_flushed, 0);
        assert_eq!(stats.bytes_written, 0);
        assert!(!stats.segment_rotated);
    }

    #[test]
    fn test_flush_single_entry() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        let entry = create_test_entry(1, b"test data");
        let data_len = entry.data.len();

        let stats = coordinator.flush(vec![entry], true).unwrap();

        assert_eq!(stats.entries_flushed, 1);
        assert_eq!(stats.bytes_written, data_len);
        assert_eq!(coordinator.total_entries_flushed(), 1);
    }

    #[test]
    fn test_flush_multiple_entries() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        let entries: Vec<_> = (1..=10)
            .map(|i| create_test_entry(i, &[i as u8; 100]))
            .collect();
        let total_bytes: usize = entries.iter().map(|e| e.data.len()).sum();

        let stats = coordinator.flush(entries, true).unwrap();

        assert_eq!(stats.entries_flushed, 10);
        assert_eq!(stats.bytes_written, total_bytes);
    }

    #[test]
    fn test_segment_rotation() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 100; // Very small for testing

        let coordinator = FlushCoordinator::new(config).unwrap();

        // 1. Boundary Test: EXACTLY 100 bytes (header + data)
        // WAL Header is 5 bytes. So we need 95 bytes of data.
        // Actually, `current_segment_size` includes header.
        // When we open segment, we write 5 bytes. Size = 5.
        // We want to reach exactly 100.
        // PendingEntry wrapper overhead is NOT written to disk in raw `write_all`.
        // So if we write 95 bytes, total size = 5 + 95 = 100.
        // 100 >= 100 -> Should Rotate.

        let entry1 = create_test_entry(1, &[0u8; 95]);
        let stats1 = coordinator.flush(vec![entry1], true).unwrap();

        // Should rotate because 5 (header) + 95 (data) = 100 >= 100
        assert!(
            stats1.segment_rotated,
            "Should rotate at exactly segment_size (100). Size was {}",
            coordinator.current_segment_size() // This will be 0 if rotated, or 100 if not
        );
        assert_eq!(coordinator.current_segment_size(), 0); // Reset after rotation

        // 2. Boundary Test: 99 bytes (header + data)
        // Size = 5 (new header) + 94 (data) = 99.
        // 99 < 100 -> Should NOT Rotate.
        let entry2 = create_test_entry(2, &[0u8; 94]);
        let stats2 = coordinator.flush(vec![entry2], true).unwrap();

        assert!(
            !stats2.segment_rotated,
            "Should NOT rotate at segment_size - 1 (99)"
        );
        assert_eq!(coordinator.current_segment_size(), 99);
    }

    #[test]
    fn test_completion_notification() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        let (entry, handle) = PendingEntry::new_sync(LSN(1), vec![1, 2, 3]);

        assert!(!handle.is_complete());

        coordinator.flush(vec![entry], true).unwrap();

        assert!(handle.is_complete());
        assert!(handle.wait().is_ok());
    }

    #[test]
    fn test_flush_signal() {
        let signal = FlushSignal::new();

        assert!(!signal.take_request());

        signal.request_flush();
        assert!(signal.take_request());
        assert!(!signal.take_request()); // Should be cleared
    }

    #[test]
    fn test_flush_signal_wait_timeout() {
        let signal = FlushSignal::new();

        // Should timeout
        let result = signal.wait_for_request(Duration::from_millis(10));
        assert!(!result);
    }

    #[test]
    fn test_flush_signal_wait_immediate() {
        let signal = FlushSignal::new();
        signal.request_flush();

        // Should return immediately
        let result = signal.wait_for_request(Duration::from_secs(10));
        assert!(result);
    }

    #[test]
    fn test_segment_file_creation() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        let entry = create_test_entry(1, b"test");
        coordinator.flush(vec![entry], true).unwrap();

        // Check segment file exists
        let segment_path = coordinator.segment_path(coordinator.current_segment_id());
        assert!(segment_path.exists());
    }

    #[test]
    fn test_wal_header() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        let entry = create_test_entry(1, b"test");
        coordinator.flush(vec![entry], true).unwrap();

        // Read segment file and verify header
        let segment_path = coordinator.segment_path(coordinator.current_segment_id());
        let data = std::fs::read(&segment_path).unwrap();

        assert!(data.len() >= WAL_HEADER_SIZE);
        assert_eq!(&data[0..4], &WAL_MAGIC);
        assert_eq!(data[4], WAL_VERSION);
    }

    #[test]
    fn test_flush_thread_basic() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        let entries = Arc::new(Mutex::new(vec![
            create_test_entry(1, b"one"),
            create_test_entry(2, b"two"),
        ]));

        let entries_clone = Arc::clone(&entries);
        let mut thread = FlushThread::start(
            Arc::clone(&coordinator),
            move || {
                let mut guard = entries_clone.lock().unwrap();
                std::mem::take(&mut *guard)
            },
            Duration::from_millis(10),
        );

        // Request flush
        thread.request_flush();

        // Poll with timeout for flush to complete (more reliable than fixed sleep)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        while coordinator.total_entries_flushed() < 2 {
            if start.elapsed() > timeout {
                panic!(
                    "Timeout waiting for flush: only {} entries flushed",
                    coordinator.total_entries_flushed()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // Should have flushed
        assert!(coordinator.total_entries_flushed() >= 2);

        thread.shutdown();
    }

    #[test]
    fn test_cleanup_old_segments() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 50; // Very small
        config.segments_to_retain = 2; // Retain 2 previous + 1 current = 3 total

        let coordinator = FlushCoordinator::new(config).unwrap();

        // Create segments 1..10
        // Each flush creates a new segment because data (100) > size (50)
        for i in 1..=10 {
            let entry = create_test_entry(i, &[i as u8; 100]);
            coordinator.flush(vec![entry], true).unwrap();
        }

        // We expect segments 8, 9, 10 to remain.
        // 10 is current. 9 and 8 are retained.
        // 1..7 should be deleted.

        let mut segments: Vec<u64> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    path.file_stem()
                        .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                } else {
                    None
                }
            })
            .collect();
        segments.sort();

        // Strict assertion: We must have exactly 3 segments.
        assert_eq!(
            segments.len(),
            3,
            "Should retain exactly 3 segments (2 + current). Found: {:?}",
            segments
        );

        // Strict assertion: Verify exact IDs
        // Current ID is 10 (created by 10th flush).
        // cleanup retains `current_id` and `segments_to_retain` previous ones.
        // retain_from = 10 - 2 = 8.
        // Keeps ID >= 8: 8, 9, 10.
        assert_eq!(segments, vec![8, 9, 10]);
    }

    // ============================================================
    // TDD Tests for LSN-based truncation (ADR-0025)
    // ============================================================

    #[test]
    fn test_segment_metadata_serialization() {
        let metadata = SegmentMetadata::new(LSN(100), LSN(200), 50);

        let bytes = metadata.to_bytes();
        assert_eq!(bytes.len(), 24);

        let restored = SegmentMetadata::from_bytes(&bytes).unwrap();
        assert_eq!(restored.min_lsn, LSN(100));
        assert_eq!(restored.max_lsn, LSN(200));
        assert_eq!(restored.entry_count, 50);
    }

    #[test]
    fn test_segment_metadata_from_bytes_too_short() {
        let bytes = vec![0u8; 10]; // Too short
        assert!(SegmentMetadata::from_bytes(&bytes).is_none());
    }

    #[test]
    fn test_flush_tracks_lsn_range() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 50; // Very small to force rotation

        let coordinator = FlushCoordinator::new(config).unwrap();
        coordinator.set_expected_lsn(LSN(10));

        // Flush entries with various LSNs
        let entries = vec![
            create_test_entry(10, &[1u8; 20]),
            create_test_entry(20, &[2u8; 20]), // Gap 11-14, 16-19
            create_test_entry(15, &[3u8; 20]),
        ];
        // Note: 20 and 15 will be buffered because of gaps. Only 10 will flush.
        // Wait, if 10 flushes, next is 11.
        // 15 is buffered. 20 is buffered.
        // Metadata is written on rotation.
        // We need contiguous stream to flush and rotate.
        // So we should provide contiguous entries if we want to test metadata writing.
        // Or update expected LSN manually? No, flush() updates it.

        // Let's use contiguous entries 10, 11, 12.
        let entries = vec![
            create_test_entry(10, &[1u8; 20]),
            create_test_entry(11, &[2u8; 20]),
            create_test_entry(12, &[3u8; 20]),
        ];
        coordinator.flush(entries, true).unwrap();

        // Force rotation to write metadata
        let entry = create_test_entry(100, &[4u8; 100]);
        coordinator.flush(vec![entry], true).unwrap();

        // Check segments have metadata
        let segments = coordinator.list_segments_with_metadata();
        assert!(!segments.is_empty());

        // First segment should have min_lsn=10, max_lsn=20
        if let Some((_, meta)) = segments.first() {
            assert_eq!(meta.min_lsn, LSN(10));
            assert_eq!(meta.max_lsn, LSN(12));
            assert_eq!(meta.entry_count, 3);
        }
    }

    #[test]
    fn test_truncate_to_lsn_removes_old_segments() {
        let dir = tempdir().unwrap();

        // Use segment size 200.
        // 10 entries of 20 bytes = 200 bytes.
        // + Header (5 bytes) = 205 bytes.
        // 205 >= 200 -> Rotates immediately after the batch.
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 200;
        config.segments_to_retain = 100; // Don't auto-cleanup

        let coordinator = FlushCoordinator::new(config).unwrap();

        // Segment 1: LSN 1-10
        let entries1: Vec<_> = (1..=10)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries1, true).unwrap();

        // Segment 2: LSN 11-20
        let entries2: Vec<_> = (11..=20)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries2, true).unwrap();

        // Segment 3: LSN 21-30
        let entries3: Vec<_> = (21..=30)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries3, true).unwrap();

        // We expect 3 segments.
        // Seg 1 (LSN 1-10) -> Rotated
        // Seg 2 (LSN 11-20) -> Rotated
        // Seg 3 (LSN 21-30) -> Rotated

        // Truncate to LSN 15.
        // Should remove Seg 1 (Max 10 < 15).
        // Should KEEP Seg 2 (Max 20 >= 15).
        // Should KEEP Seg 3 (Max 30 >= 15).

        let removed = coordinator.truncate_to_lsn(LSN(15)).unwrap();

        assert_eq!(removed, 1, "Should remove exactly 1 segment (LSN 1-10)");

        // Verify Seg 1 is PHYSICALLY gone from disk.
        // We use fs::read_dir directly to avoid relying on list_segments_with_metadata
        // which might filter out files if metadata is missing (false negative).
        let segment_ids: Vec<u64> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    path.file_stem()
                        .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !segment_ids.contains(&1),
            "Segment 1 should be physically deleted"
        );
        assert!(segment_ids.contains(&2), "Segment 2 should remain");
        assert!(segment_ids.contains(&3), "Segment 3 should remain");
    }

    #[test]
    fn test_truncate_to_lsn_keeps_needed_segments() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 30; // Very small
        config.segments_to_retain = 100; // Don't auto-cleanup

        let coordinator = FlushCoordinator::new(config).unwrap();
        coordinator.set_expected_lsn(LSN(100));

        // Create a segment with LSN 100-110
        for i in 100..=110 {
            coordinator
                .flush(vec![create_test_entry(i, &[i as u8; 20])], true)
                .unwrap();
        }

        // Force rotation
        coordinator
            .flush(vec![create_test_entry(200, &[200u8; 100])], true)
            .unwrap();

        // Truncate to LSN 50 - should not remove the segment with LSN 100-110
        let removed = coordinator.truncate_to_lsn(LSN(50)).unwrap();
        assert_eq!(removed, 0);

        // Verify segment still exists
        let segments = coordinator.list_segments_with_metadata();
        assert!(!segments.is_empty());
    }

    #[test]
    fn test_truncate_to_lsn_never_removes_active_segment() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segments_to_retain = 100;

        let coordinator = FlushCoordinator::new(config).unwrap();

        // Create a single active segment
        coordinator
            .flush(vec![create_test_entry(1, b"test")], true)
            .unwrap();

        let current_id = coordinator.current_segment_id();

        // Try to truncate everything
        let removed = coordinator.truncate_to_lsn(LSN(1000)).unwrap();
        assert_eq!(removed, 0);

        // Active segment should still exist
        let segment_path = coordinator.segment_path(current_id);
        assert!(segment_path.exists());
    }

    #[test]
    fn test_get_min_lsn() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 30;
        config.segments_to_retain = 100;

        let coordinator = FlushCoordinator::new(config).unwrap();
        coordinator.set_expected_lsn(LSN(50));

        // Initially no segments with metadata
        assert!(coordinator.get_min_lsn().is_none());

        // Create segment with LSN 50-60
        for i in 50..=60 {
            coordinator
                .flush(vec![create_test_entry(i, &[i as u8; 20])], true)
                .unwrap();
        }

        // Force rotation to write metadata
        coordinator
            .flush(vec![create_test_entry(100, &[100u8; 100])], true)
            .unwrap();

        // min_lsn should be 50
        let min_lsn = coordinator.get_min_lsn();
        assert!(min_lsn.is_some());
        assert_eq!(min_lsn.unwrap(), LSN(50));
    }

    #[test]
    fn test_cleanup_removes_meta_files() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 30;
        config.segments_to_retain = 1;

        let coordinator = FlushCoordinator::new(config).unwrap();

        // Create multiple segments to trigger cleanup
        for i in 1..=50 {
            coordinator
                .flush(vec![create_test_entry(i, &[i as u8; 30])], true)
                .unwrap();
        }

        // Count .meta files - should be limited due to cleanup
        let meta_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|s| s.ends_with(".log.meta"))
            })
            .count();

        // Should have at most segments_to_retain + 1 meta files
        assert!(meta_count <= 3);
    }

    #[test]
    #[cfg(unix)]
    fn test_sync_logic_correctness() {
        use tempfile::tempdir;

        // 1. Setup coordinator with sync_on_flush = true
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.sync_on_flush = true;
        let coordinator = FlushCoordinator::new(config).unwrap();

        // 2. Open segment to get sync handle
        {
            let mut writer_guard = coordinator.writer.lock().unwrap();
            coordinator.ensure_segment_open(&mut writer_guard).unwrap();
        }

        // 3. Replace sync_handle with a File opening /dev/null
        // fsync on /dev/null returns EINVAL on Linux, which causes sync_data to fail.
        // This avoids unsafe libc::close and double-close issues.
        {
            let dev_null = File::open("/dev/null").expect("Failed to open /dev/null");
            let mut guard = coordinator.sync_handle.lock().unwrap();
            *guard = Some(dev_null);
        }

        // 4. Create dummy entry
        let entry = create_test_entry(1, &[1, 2, 3]);

        // 5. Test Case 1: sync=false.
        // Correct Logic: sync (false) && sync_on_flush (true) -> false. No sync -> Success.
        // Mutant Logic (||): sync (false) || sync_on_flush (true) -> true. Sync -> Fail (EINVAL).
        // This assertion KILLS the mutant.
        let result = coordinator.flush(vec![entry], false);
        assert!(
            result.is_ok(),
            "flush(false) should NOT sync, so it should succeed even if sync handle is broken"
        );

        // 6. Test Case 2: sync=true.
        // Correct Logic: sync (true) && sync_on_flush (true) -> true. Sync -> Fail.
        // This confirms our test setup works (broken sync handle actually causes failure).
        let entry2 = create_test_entry(2, &[4, 5, 6]);
        let result = coordinator.flush(vec![entry2], true);
        assert!(
            result.is_err(),
            "flush(true) SHOULD sync, so it should fail due to broken sync handle"
        );

        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Failed to sync WAL"),
            "Error should be about syncing, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_truncate_safe_defaults_on_missing_metadata() {
        // 💣 Risk: If .meta file is missing, truncation logic should be conservative
        // and NOT delete the segment, to prevent accidental data loss.

        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 50; // Small size to force rotation
        config.segments_to_retain = 100; // Don't auto-cleanup

        let coordinator = FlushCoordinator::new(config).unwrap();

        // 1. Create a segment with LSN 1-10
        // Entries: 10 * ~20 bytes = 200 bytes > 50 bytes, so it will rotate
        let entries: Vec<_> = (1..=10)
            .map(|i| create_test_entry(i, &[i as u8; 10]))
            .collect();

        coordinator.flush(entries, true).unwrap();

        // Force rotation by writing more entries
        let entries2: Vec<_> = (11..=20)
            .map(|i| create_test_entry(i, &[i as u8; 10]))
            .collect();
        coordinator.flush(entries2, true).unwrap();

        // 2. Identify the first segment
        let segments = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "log"))
            .collect::<Vec<_>>();

        // Should have at least 2 segments (one closed, one active)
        assert!(segments.len() >= 2, "Expected at least 2 segments");

        // Find the oldest segment (lowest number)
        let oldest_segment_path = segments.iter().min_by_key(|e| e.path()).unwrap().path();

        // Find its metadata file
        // Note: FlushCoordinator naming is {:06}.log and {:06}.log.meta
        // We handle path extension carefully
        let mut meta_path = oldest_segment_path.clone();
        if let Some(name) = meta_path.file_name() {
            let mut name = name.to_os_string();
            name.push(".meta");
            meta_path.set_file_name(name);
        }

        assert!(
            meta_path.exists(),
            "Metadata file should exist: {:?}",
            meta_path
        );

        // 3. Delete the metadata file
        std::fs::remove_file(&meta_path).unwrap();
        assert!(!meta_path.exists());

        // 4. Try to truncate up to LSN 100 (which is > max LSN of 10)
        // If metadata were present, this would delete the segment.
        // Without metadata, it should be CONSERVATIVE and keep it.
        let removed = coordinator.truncate_to_lsn(LSN(100)).unwrap();

        // 5. Verify conservative behavior
        assert_eq!(
            removed, 0,
            "Should not remove segment if metadata is missing"
        );
        assert!(
            oldest_segment_path.exists(),
            "Segment log file should still exist"
        );
    }

    #[test]
    fn test_flush_buffers_out_of_order_entries() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        // 1. Flush 1, 2, 4 (Gap at 3)
        let entries1 = vec![
            create_test_entry(1, b"1"),
            create_test_entry(2, b"2"),
            create_test_entry(4, b"4"),
        ];

        let stats1 = coordinator.flush(entries1, true).unwrap();

        // Should flush 1, 2. Buffer 4.
        assert_eq!(stats1.entries_flushed, 2, "Should only flush 1 and 2");
        assert_eq!(coordinator.total_entries_flushed(), 2);

        // Verify next expected is 3
        assert_eq!(coordinator.next_expected_lsn.load(Ordering::Relaxed), 3);

        // 2. Flush 3, 5 (Fills gap, adds new)
        let entries2 = vec![create_test_entry(3, b"3"), create_test_entry(5, b"5")];

        let stats2 = coordinator.flush(entries2, true).unwrap();

        // Should flush 3, then 4 (from buffer), then 5. Total 3.
        assert_eq!(stats2.entries_flushed, 3, "Should flush 3, 4 (buffered), 5");
        assert_eq!(coordinator.total_entries_flushed(), 5);

        // Verify next expected is 6
        assert_eq!(coordinator.next_expected_lsn.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn test_flush_handles_duplicates() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        // Flush 1
        coordinator
            .flush(vec![create_test_entry(1, b"1")], true)
            .unwrap();

        // Flush 1 again (Duplicate)
        let stats = coordinator
            .flush(vec![create_test_entry(1, b"1")], true)
            .unwrap();

        assert_eq!(stats.entries_flushed, 0, "Duplicate should be ignored");
        assert_eq!(coordinator.total_entries_flushed(), 1);
    }
}
