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

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::ring_buffer::PendingEntry;

use crate::utils::error::{Error, Result, StorageError};

/// Magic bytes identifying a GallifreyDB WAL segment file.
const WAL_MAGIC: [u8; 4] = *b"GWAL";

/// Current WAL format version.
const WAL_VERSION: u8 = 1;

/// Size of the WAL segment header (magic + version).
const WAL_HEADER_SIZE: usize = 5;

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

        self.current_segment_id
            .store(max_segment_id, Ordering::Relaxed);
        Ok(())
    }

    /// Get the path for a segment file.
    fn segment_path(&self, segment_id: u64) -> PathBuf {
        self.config.wal_dir.join(format!("{:06}.log", segment_id))
    }

    /// Open or create the current segment file.
    fn ensure_segment_open(&self) -> Result<()> {
        let mut writer_guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());

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
    fn maybe_rotate_segment(&self) -> Result<bool> {
        let current_size = self.current_segment_size.load(Ordering::Relaxed);

        if current_size >= self.config.segment_size as u64 {
            // Flush and sync current segment
            {
                let mut writer_guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut writer) = *writer_guard {
                    writer.flush().map_err(|e| {
                        Error::Storage(StorageError::IoError(format!(
                            "Failed to flush WAL segment: {}",
                            e
                        )))
                    })?;
                }
                *writer_guard = None;
            }

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

            // Clear sync handle
            {
                let mut sync_guard = self.sync_handle.lock().unwrap_or_else(|e| e.into_inner());
                *sync_guard = None;
            }

            // Reset size
            self.current_segment_size.store(0, Ordering::Relaxed);

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
                let is_old_segment = path.extension().is_some_and(|ext| ext == "log")
                    && path
                        .file_stem()
                        .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                        .is_some_and(|id| id < retain_from);

                if is_old_segment {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }

        Ok(())
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
    pub fn flush(&self, entries: Vec<PendingEntry>, sync: bool) -> Result<FlushStats> {
        if entries.is_empty() {
            return Ok(FlushStats::default());
        }

        let start = Instant::now();

        // Ensure segment is open
        self.ensure_segment_open()?;

        let mut bytes_written = 0usize;

        // Write all entries
        {
            let mut writer_guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
            let writer = writer_guard.as_mut().ok_or_else(|| {
                Error::Storage(StorageError::WalError {
                    reason: "WAL writer not initialized".to_string(),
                })
            })?;

            for entry in &entries {
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
        let segment_rotated = self.maybe_rotate_segment()?;

        // Notify all completion handles
        for entry in &entries {
            entry.notify_completion();
        }

        // Update metrics
        self.total_entries_flushed
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        self.total_bytes_written
            .fetch_add(bytes_written as u64, Ordering::Relaxed);
        self.total_flushes.fetch_add(1, Ordering::Relaxed);

        Ok(FlushStats {
            entries_flushed: entries.len(),
            bytes_written,
            flush_duration: start.elapsed(),
            segment_rotated,
        })
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
    pub fn wait_for_request(&self, timeout: Duration) -> bool {
        let guard = self.mutex.lock().unwrap_or_else(|e| e.into_inner());

        if self.requested.load(Ordering::Acquire) {
            return true;
        }

        let (_guard, result) = self
            .condvar
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|e| e.into_inner());

        !result.timed_out() || self.requested.load(Ordering::Acquire)
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

        // First flush - creates segment
        let entries: Vec<_> = (1..=5)
            .map(|i| create_test_entry(i, &[i as u8; 50]))
            .collect();
        let stats = coordinator.flush(entries, true).unwrap();

        // Should have rotated due to small segment size
        assert!(stats.segment_rotated || coordinator.current_segment_size() < 100);
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

        // Wait a bit for flush to complete
        std::thread::sleep(Duration::from_millis(50));

        // Should have flushed
        assert!(coordinator.total_entries_flushed() >= 2);

        thread.shutdown();
    }

    #[test]
    fn test_cleanup_old_segments() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 50; // Very small
        config.segments_to_retain = 2;

        let coordinator = FlushCoordinator::new(config).unwrap();

        // Create multiple small segments
        for i in 1..=10 {
            let entry = create_test_entry(i, &[i as u8; 100]);
            coordinator.flush(vec![entry], true).unwrap();
        }

        // Count remaining segments
        let segment_count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "log")
                    .unwrap_or(false)
            })
            .count();

        // Should have at most segments_to_retain + 1 (current)
        assert!(segment_count <= 3);
    }
}
