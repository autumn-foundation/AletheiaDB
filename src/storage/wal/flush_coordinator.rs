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

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::LSN;
use super::ring_buffer::PendingEntry;

use crate::core::error::{Error, Result, StorageError};

use super::segment_reader::{
    WAL_HEADER_SIZE, WAL_KEYVERSIONED_HEADER_SIZE, WAL_MAGIC, WAL_VERSION_ENCRYPTED_KEYVERSIONED,
    WAL_VERSION_STRING_LABELS,
};
use crate::encryption::wal_encryption::WalKeyring;
use arc_swap::ArcSwapOption;

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
    ///
    /// ⚡ Bolt Optimization: Return a fixed 24-byte stack array `[u8; 24]` instead of
    /// a heap-allocated `Vec<u8>` to eliminate memory allocation during WAL segment rotation.
    pub fn to_bytes(&self) -> [u8; 24] {
        let mut bytes = [0u8; 24];
        bytes[0..8].copy_from_slice(&self.min_lsn.0.to_le_bytes()[..]);
        bytes[8..16].copy_from_slice(&self.max_lsn.0.to_le_bytes()[..]);
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes()[..]);
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
#[derive(Clone)]
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
    /// Optional WAL DEK keyring for encrypting WAL entries before writing to
    /// disk, held in a runtime-swappable presence cell (Issues #3617, #3616).
    ///
    /// When the cell holds a keyring, entries are encrypted (4-byte
    /// length-prefixed) under its CURRENT generation and fresh segments use the
    /// keyversioned (v16) header stamping that generation's `key_version`; a
    /// full-MEK rotation advances the current generation so subsequent segments
    /// are written under the new DEK while legacy/old-DEK segments still replay
    /// under the retained old generation. When empty (`None`), segments use the
    /// plaintext (v13) format (backward compatible).
    ///
    /// This is the **same** `Arc<ArcSwapOption<..>>` cell shared with the owning
    /// [`ConcurrentWalSystem`](super::concurrent_system::ConcurrentWalSystem), so a
    /// runtime install (`None` → `Some`, Issue #3616 PR2) performed through the
    /// system is observed here on the write path immediately. All write-path reads
    /// happen under the coordinator `writer` mutex, and the install stores into the
    /// cell inside the seal→reopen hand-off, so the (segment header, encrypting
    /// cipher) pair is always consistent.
    pub wal_keyring: Arc<ArcSwapOption<WalKeyring>>,
}

impl std::fmt::Debug for FlushCoordinatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlushCoordinatorConfig")
            .field("wal_dir", &self.wal_dir)
            .field("segment_size", &self.segment_size)
            .field("segments_to_retain", &self.segments_to_retain)
            .field("flush_interval_ms", &self.flush_interval_ms)
            .field("sync_on_flush", &self.sync_on_flush)
            .field("write_buffer_size", &self.write_buffer_size)
            .field("wal_keyring", &*self.wal_keyring.load())
            .finish()
    }
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
            wal_keyring: Arc::new(ArcSwapOption::from(None)),
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
    /// Highest `max_lsn` among segments removed by [`Self::truncate_to_lsn`]
    /// this process lifetime (0 = never truncated). Sealed-segment metadata
    /// alone cannot answer "was anything truncated?" once truncation removes
    /// EVERY sealed segment (the active segment carries no metadata file), so
    /// the replication feed needs this watermark to distinguish "fresh WAL,
    /// everything available" from "history was truncated away" -- otherwise a
    /// lagging replica requesting a truncated LSN would get a gapped batch
    /// instead of `ResyncRequired` (Issue #3355 review). In-memory only: after
    /// a restart, surviving sealed segments repopulate `get_min_lsn`, and the
    /// feed's gap-withholding turns the residual all-sealed-truncated corner
    /// into an observable stall rather than silent skip.
    truncated_up_to_lsn: AtomicU64,
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
            truncated_up_to_lsn: AtomicU64::new(0),
        };

        // Find the latest segment ID
        coordinator.initialize_from_existing()?;

        Ok(coordinator)
    }

    /// Initialize from existing WAL segments.
    ///
    /// # Failure semantics (Issue #3423)
    ///
    /// The maximum existing segment id determines where `ensure_segment_open`
    /// starts allocating. Under-reporting it (e.g. defaulting to 0 after a
    /// failed directory scan) can land the writer on a stale/existing segment
    /// and — combined with a payload-version bump — produce an unparseable
    /// mixed-version segment. We therefore distinguish two cases:
    ///
    /// - **Directory does not exist yet** (`NotFound`): a normal first run on
    ///   a fresh data dir. The scan yields no segments and we start at id 0.
    ///   This is not an error.
    /// - **Any other I/O error** (permission denied, not-a-directory, a
    ///   transient failure): we cannot trust that segment id 0 is unused, so
    ///   we **fail loud** and propagate the error rather than silently
    ///   defaulting to 0. The header-version check in `ensure_segment_open`
    ///   remains a second line of defense, but a swallowed scan error must not
    ///   be the reason we rely on it.
    fn initialize_from_existing(&self) -> Result<()> {
        let max_segment_id = match std::fs::read_dir(&self.config.wal_dir) {
            // Map each yielded entry to its path (a cheap, non-fallible op)
            // while preserving per-entry iteration errors as `Err`, then fold
            // to the max segment id. `scan_max_segment_id` propagates those
            // per-entry errors instead of swallowing them with `.flatten()`.
            Ok(entries) => Self::scan_max_segment_id(
                &self.config.wal_dir,
                entries.map(|entry| entry.map(|e| e.path())),
            )?,
            // First run: the WAL directory has not been created yet. Treat an
            // absent directory as "no existing segments" and start at id 0.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            // A genuine I/O error opening the directory: refuse to proceed
            // rather than under-report the max segment id and risk appending
            // to an existing segment.
            Err(e) => {
                return Err(Error::Storage(StorageError::IoError(format!(
                    "Failed to scan WAL directory {} for existing segments: {}; \
                     refusing to start segment allocation at 0 (Issue #3423)",
                    self.config.wal_dir.display(),
                    e
                ))));
            }
        };

        self.current_segment_id
            .store(max_segment_id, Ordering::Relaxed);
        Ok(())
    }

    /// Fold a WAL-directory listing into the maximum `NNNNNN.log` segment id.
    ///
    /// A per-entry iteration error (an `Err` yielded partway through the
    /// directory scan) is **propagated**, never swallowed: skipping an entry
    /// could let the returned max id under-report and default the writer
    /// toward segment id 0, exactly the fail-loud gap this hardening closes
    /// (Issue #3423). Extracted from `initialize_from_existing` so the
    /// per-entry error path is unit-testable without a filesystem mock.
    fn scan_max_segment_id(
        wal_dir: &Path,
        entries: impl Iterator<Item = std::io::Result<PathBuf>>,
    ) -> Result<u64> {
        let mut max_segment_id = 0u64;
        for entry in entries {
            let path = entry.map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to read a WAL directory entry in {}: {}; \
                     refusing to start segment allocation at 0 (Issue #3423)",
                    wal_dir.display(),
                    e
                )))
            })?;
            if let Some(id) = path
                .extension()
                .filter(|ext| *ext == "log")
                .and_then(|_| path.file_stem())
                .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
            {
                max_segment_id = max_segment_id.max(id);
            }
        }
        Ok(max_segment_id)
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
    fn write_segment_metadata(
        &self,
        segment_id: u64,
        min_lsn: u64,
        max_lsn: u64,
        entry_count: u64,
    ) -> Result<()> {
        // Only write metadata if we have valid LSN range
        if min_lsn <= max_lsn && entry_count > 0 {
            let metadata = SegmentMetadata::new(LSN(min_lsn), LSN(max_lsn), entry_count);
            let meta_path = self.segment_meta_path(segment_id);
            let bytes = metadata.to_bytes();

            std::fs::write(&meta_path, bytes).map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to write segment metadata: {}",
                    e
                )))
            })?;
        }

        Ok(())
    }

    /// Read segment metadata from a companion file.
    ///
    /// ⚡ Bolt Optimization: Replace Vec::new() heap allocation with a fixed 24-byte stack array when reading metadata.
    pub fn read_segment_metadata(&self, segment_id: u64) -> Option<SegmentMetadata> {
        let meta_path = self.segment_meta_path(segment_id);
        let mut bytes = [0u8; 24];
        File::open(&meta_path).ok()?.read_exact(&mut bytes).ok()?;
        SegmentMetadata::from_bytes(&bytes)
    }

    /// Read the format version byte + optional `key_version` from an existing
    /// segment's header (Issue #3617).
    ///
    /// Returns `None` when the file cannot be read, is shorter than a full
    /// header, or does not start with the WAL magic bytes. For a keyversioned
    /// (v16) segment the second element carries the stamped `key_version`; every
    /// legacy (5-byte header) segment yields `None` there.
    fn read_segment_header_id(path: &Path) -> Option<(u8, Option<u32>)> {
        let mut header = [0u8; WAL_KEYVERSIONED_HEADER_SIZE];
        let mut file = File::open(path).ok()?;
        // Read at least the 5-byte legacy header; the extra 4 bytes are only
        // meaningful (and required) for a keyversioned segment.
        file.read_exact(&mut header[..WAL_HEADER_SIZE]).ok()?;
        if header[0..4] != WAL_MAGIC {
            return None;
        }
        let version = header[WAL_HEADER_SIZE - 1];
        if version == WAL_VERSION_ENCRYPTED_KEYVERSIONED {
            file.read_exact(&mut header[WAL_HEADER_SIZE..WAL_KEYVERSIONED_HEADER_SIZE])
                .ok()?;
            let kv = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
            Some((version, Some(kv)))
        } else {
            Some((version, None))
        }
    }

    /// The (version, key_version) a freshly created segment is stamped with,
    /// derived from the configured WAL keyring's CURRENT generation (Issue
    /// #3617). Encrypted → keyversioned (v16) header stamping the current
    /// generation's `key_version`; plaintext → the string-label (v13) header.
    ///
    /// Reading the keyring here (rather than caching a version) is what lets a
    /// force-roll pick up the NEW generation the moment the rotation driver
    /// advances it — all serialized under the coordinator `writer` mutex so the
    /// (segment header, encrypting cipher) pair is always consistent.
    fn current_write_format(&self) -> (u8, Option<u32>) {
        match self.config.wal_keyring.load().as_ref() {
            Some(keyring) => (
                WAL_VERSION_ENCRYPTED_KEYVERSIONED,
                Some(keyring.current_version()),
            ),
            None => (WAL_VERSION_STRING_LABELS, None),
        }
    }

    /// Open or create the current segment file.
    ///
    /// # Version safety (Issue #3423)
    ///
    /// Appending to an existing non-empty segment is only safe when its
    /// header version matches the version this writer emits: at replay the
    /// parse version comes solely from the file header (entries carry no
    /// per-entry format tag), so appending newer-format entries to an
    /// older-version segment produces a mixed-version file whose appended
    /// entries fail CRC/parsing. If the allocated id points at a non-empty
    /// segment with a different (or unreadable/partial) header, the file is
    /// left untouched and the id rolls forward until a fresh or
    /// version-matching segment is found.
    fn ensure_segment_open(&self, writer_guard: &mut Option<BufWriter<File>>) -> Result<()> {
        if writer_guard.is_some() {
            return Ok(());
        }

        // New segments use the string-label payload format (Issue #3506). For an
        // ENCRYPTED WAL they are additionally wrapped in the keyversioned (v16)
        // container (Issue #3617), whose 9-byte header stamps the WAL DEK
        // generation (`key_version`) the frames are encrypted under, so a
        // full-MEK rotation can advance the write generation while legacy/old-DEK
        // segments still replay under the retained old generation. Plaintext WALs
        // keep the 5-byte v13 header. The version bump signals the new header so
        // an older reader rejects the segment cleanly rather than misparsing it.
        let (write_version, write_key_version) = self.current_write_format();
        let header_size = if write_key_version.is_some() {
            WAL_KEYVERSIONED_HEADER_SIZE
        } else {
            WAL_HEADER_SIZE
        };

        // Allocate the next segment id, rolling past any existing non-empty
        // segment whose header (version AND key_version) differs from what this
        // writer emits — appending new-format frames to an old-format segment
        // would produce a mixed, unparseable file.
        let (segment_id, path, current_len) = loop {
            let segment_id = self.current_segment_id.fetch_add(1, Ordering::Relaxed) + 1;
            let path = self.segment_path(segment_id);
            let current_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if current_len == 0 {
                break (segment_id, path, current_len);
            }
            match Self::read_segment_header_id(&path) {
                Some((version, kv)) if version == write_version && kv == write_key_version => {
                    break (segment_id, path, current_len);
                }
                _ => {
                    super::log_wal_diagnostic(&format!(
                        "WAL: not appending to existing segment {} (header version/key_version \
                         differs from writer version {} or is unreadable); rolling to next segment",
                        path.display(),
                        write_version
                    ));
                }
            }
        };

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
        if current_len == 0 {
            writer.write_all(&WAL_MAGIC).map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to write WAL header: {}",
                    e
                )))
            })?;
            writer.write_all(&[write_version]).map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to write WAL version: {}",
                    e
                )))
            })?;
            // Keyversioned (v16) segments carry the 4-byte key_version (LE) after
            // the version byte (Issue #3617). Only the integer version is
            // written — never any key material.
            if let Some(kv) = write_key_version {
                writer.write_all(&kv.to_le_bytes()).map_err(|e| {
                    Error::Storage(StorageError::IoError(format!(
                        "Failed to write WAL key_version: {}",
                        e
                    )))
                })?;
            }
            self.current_segment_size
                .store(header_size as u64, Ordering::Relaxed);
        } else {
            // For existing (version-matching) segments, we must initialize
            // the size correctly
            self.current_segment_size
                .store(current_len, Ordering::Relaxed);
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
            self.seal_current(writer_guard)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Seal the current active segment: flush its buffer, capture and durably
    /// write its `.meta` LSN range, fsync, drop the writer, and reset the
    /// per-segment counters so the next [`ensure_segment_open`](Self::ensure_segment_open)
    /// starts a fresh segment. Extracted from [`maybe_rotate_segment`](Self::maybe_rotate_segment)
    /// so a force-roll (Issue #3617) can reuse the exact same seal/flush/meta
    /// path without the size predicate.
    ///
    /// # Thread Safety
    ///
    /// The caller MUST hold the `writer` lock (`writer_guard`), so sealing is
    /// atomic with respect to concurrent flushes. On any I/O error the writer is
    /// already closed and counters reset, so the next flush opens a fresh
    /// segment rather than smearing LSN ranges.
    fn seal_current(&self, writer_guard: &mut Option<BufWriter<File>>) -> Result<()> {
        let closing_segment_id = self.current_segment_id.load(Ordering::Relaxed);

        // 1. Flush current segment first.
        // If this fails, we return error and DO NOT reset state or close the writer.
        // This allows the next flush attempt to retry.
        if let Some(writer) = writer_guard {
            writer.flush().map_err(|e| {
                Error::Storage(StorageError::IoError(format!(
                    "Failed to flush WAL segment: {}",
                    e
                )))
            })?;
        }

        // 2. Capture state for metadata/cleanup before resetting
        let min_lsn = self.current_segment_min_lsn.load(Ordering::Relaxed);
        let max_lsn = self.current_segment_max_lsn.load(Ordering::Relaxed);
        let entry_count = self.current_segment_entry_count.load(Ordering::Relaxed);

        // 3. Close writer (drops BufWriter) - effectively "committing" to rotation
        *writer_guard = None;

        // 4. Reset size and LSN tracking for new segment immediately.
        // We do this BEFORE sync/metadata write. This ensures that even if those subsequent
        // steps fail, the internal state is clean for the next segment (which will be a NEW segment
        // because writer is None). This prevents "smearing" LSN ranges.
        self.current_segment_size.store(0, Ordering::Relaxed);
        self.current_segment_min_lsn
            .store(u64::MAX, Ordering::Relaxed);
        self.current_segment_max_lsn.store(0, Ordering::Relaxed);
        self.current_segment_entry_count.store(0, Ordering::Relaxed);

        // 5. Sync before closing (using separate handle)
        // If this fails, we return error, but state is already reset and writer closed.
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

        // 6. Write segment metadata (ADR-0025) using captured state
        // If this fails, we return error, but state is already reset.
        self.write_segment_metadata(closing_segment_id, min_lsn, max_lsn, entry_count)?;

        // Clear sync handle
        {
            let mut sync_guard = self.sync_handle.lock().unwrap_or_else(|e| e.into_inner());
            *sync_guard = None;
        }

        // Clean up old segments
        self.cleanup_old_segments()?;

        Ok(())
    }

    /// Force-seal the active segment and reopen a fresh one, running `advance`
    /// under the coordinator `writer` mutex BETWEEN the seal and the reopen
    /// (Issue #3617 WAL key rotation).
    ///
    /// This is the WAL force-roll primitive. The whole seal → `advance` →
    /// reopen sequence is one critical section holding the `writer` mutex, so it
    /// is serialized against every concurrent [`flush`](Self::flush) (which also
    /// reads the write cipher AND writes the segment header under the same lock).
    /// A rotation passes an `advance` closure that flips the WAL keyring to the
    /// new generation; because the flip happens strictly between sealing the old
    /// segment and opening the new one, the freshly opened segment's header
    /// (`ensure_segment_open` re-reads the keyring) and every frame written into
    /// it use the NEW generation, while the just-sealed segment and all its
    /// frames stay on the OLD generation — never a split.
    ///
    /// The reopened segment's id is `> ` the sealed segment's id, so a subsequent
    /// [`truncate_to_lsn`](Self::truncate_to_lsn) can drop the just-sealed
    /// (old-generation) segment. `advance` must NOT acquire any lock ordered
    /// after `wal` (e.g. it must not trigger a cold flush) — it only mutates the
    /// in-memory keyring.
    pub fn seal_active_segment_and_reopen<F: FnOnce()>(&self, advance: F) -> Result<()> {
        let mut writer_guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        // Ensure there IS an active segment to seal (idempotent: a no-op reopen
        // if nothing has been written yet still advances the id so the roll
        // boundary is well-defined).
        self.ensure_segment_open(&mut writer_guard)?;
        self.seal_current(&mut writer_guard)?;
        // Flip the write generation while the old segment is sealed and no new
        // one is open — the atomic hand-off point.
        advance();
        // Open the fresh segment under the (possibly advanced) current generation.
        self.ensure_segment_open(&mut writer_guard)?;
        Ok(())
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

        // Remove old segments in a single pass without allocating an intermediate vector
        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log")
                    && let Some(segment_id) = path
                        .file_stem()
                        .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                    && segment_id < current_id
                {
                    // Remove if all entries in segment are before the
                    // truncate point. No metadata file: be conservative and
                    // don't remove.
                    let removable_max_lsn = self
                        .read_segment_metadata(segment_id)
                        .map(|m| m.max_lsn.0)
                        .filter(|max| *max < truncate_lsn.0);

                    if let Some(max_lsn) = removable_max_lsn {
                        // Remove segment file
                        if std::fs::remove_file(&path).is_ok() {
                            removed_count += 1;
                            // Record the truncation watermark so
                            // `get_replication_min_lsn` keeps answering
                            // correctly even once no sealed segment (and
                            // hence no metadata file) remains.
                            self.truncated_up_to_lsn
                                .fetch_max(max_lsn, Ordering::Relaxed);
                        }
                        // Remove metadata file
                        let meta_path = self.segment_meta_path(segment_id);
                        let _ = std::fs::remove_file(&meta_path);
                    }
                }
            }
        }

        Ok(removed_count)
    }

    /// Get information about all WAL segments.
    ///
    /// Returns a list of (segment_id, metadata) for all segments that have
    /// metadata files. Segments without metadata are not included.
    pub fn list_segments_with_metadata(&self) -> Vec<(u64, SegmentMetadata)> {
        let mut segments = Vec::with_capacity(16); // ⚡ Bolt Optimization: Pre-allocate space for WAL segment metadata to prevent small heap reallocations when reading directories.

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

    /// The minimum LSN still available to the replication feed (Issue #3355
    /// review): like [`Self::get_min_lsn`], but truncation-aware. Once
    /// [`Self::truncate_to_lsn`] has removed EVERY sealed segment,
    /// `get_min_lsn` returns `None` -- indistinguishable from a fresh WAL --
    /// while history genuinely is gone; this method answers
    /// `truncated_up_to + 1` in that case so the feed reports
    /// `ResyncRequired` instead of handing a lagging replica a silently
    /// gapped batch. `None` still means "nothing ever truncated, no sealed
    /// metadata": every LSN is satisfiable (modulo the active segment, which
    /// the feed's gap-withholding covers).
    pub fn get_replication_min_lsn(&self) -> Option<LSN> {
        let truncated = self.truncated_up_to_lsn.load(Ordering::Relaxed);
        match self.get_min_lsn() {
            Some(min) => Some(min),
            None if truncated > 0 => Some(LSN(truncated.saturating_add(1))),
            None => None,
        }
    }

    /// Best-known maximum LSN actually written to disk (Issue #3355
    /// replication observability): the max of every sealed segment's
    /// recorded `max_lsn` and the currently-active segment's in-progress
    /// high-water mark (`current_segment_max_lsn`, updated after each flush
    /// writes/fsyncs a batch — see [`Self::flush`]).
    ///
    /// This is an approximation for telemetry (`entries_behind`/lag
    /// reporting), not a durability guarantee: under a non-synchronous
    /// durability mode the active segment's tracked max can be updated
    /// slightly ahead of an fsync. Callers needing the durability-guaranteed
    /// view should rely on [`Self::read_from`] instead, which only ever
    /// returns entries actually decodable from segment files.
    pub fn get_max_flushed_lsn(&self) -> Option<LSN> {
        let current_max = self.current_segment_max_lsn.load(Ordering::Relaxed);
        let mut best = if current_max > 0 {
            Some(LSN(current_max))
        } else {
            None
        };
        for (_, meta) in self.list_segments_with_metadata() {
            best = Some(match best {
                Some(b) if b.0 >= meta.max_lsn.0 => b,
                _ => meta.max_lsn,
            });
        }
        best
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

        // Inner function to perform I/O operations.
        // This allows us to catch errors and notify pending entries before returning.
        let do_flush = || -> Result<FlushStats> {
            // Acquire lock once for the entire operation to ensure thread safety
            let mut writer_guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());

            // Ensure segment is open
            self.ensure_segment_open(&mut writer_guard)?;

            // Capture start size for rollback on sync failure (Phantom Commit Prevention)
            let start_size = self.current_segment_size.load(Ordering::Relaxed);

            let mut bytes_written = 0usize;

            // Track LSN range for this batch (ADR-0025)
            let mut batch_min_lsn = u64::MAX;
            let mut batch_max_lsn = 0u64;

            // Resolve the CURRENT WAL DEK cipher once, under the writer lock, so
            // every entry in this batch is encrypted under the same generation as
            // the segment header stamped by `ensure_segment_open` above (Issue
            // #3617). A force-roll advances the keyring's current generation only
            // while holding this same lock, so a batch can never be split across
            // two generations.
            let batch_cipher = self
                .config
                .wal_keyring
                .load()
                .as_ref()
                .and_then(|k| k.current())
                .map(|(cipher, _version)| cipher);

            // Write all entries
            {
                let writer = writer_guard.as_mut().ok_or_else(|| {
                    Error::Storage(StorageError::WalError {
                        reason: "WAL writer not initialized".to_string(),
                    })
                })?;

                for entry in &entries {
                    // Track LSN range
                    batch_min_lsn = batch_min_lsn.min(entry.lsn.0);
                    batch_max_lsn = batch_max_lsn.max(entry.lsn.0);

                    // When a cipher is present, encrypt the entry and prepend a
                    // 4-byte LE length prefix so the reader knows how many bytes
                    // each encrypted entry occupies. Without a cipher, write the
                    // raw entry data directly (no allocation, zero overhead).
                    let write_data: Cow<'_, [u8]> = if let Some(ref cipher) = batch_cipher {
                        let encrypted = crate::encryption::wal_encryption::encrypt_wal_payload(
                            &entry.data,
                            cipher,
                        )
                        .map_err(|e| Error::Storage(StorageError::Encryption(e.to_string())))?;
                        // Prepend 4-byte LE length of the encrypted block
                        let len_bytes = (encrypted.len() as u32).to_le_bytes();
                        let mut framed = Vec::with_capacity(4 + encrypted.len());
                        framed.extend_from_slice(&len_bytes);
                        framed.extend_from_slice(&encrypted);
                        Cow::Owned(framed)
                    } else {
                        Cow::Borrowed(&entry.data)
                    };

                    writer.write_all(&write_data).map_err(|e| {
                        Error::Storage(StorageError::IoError(format!(
                            "Failed to write WAL entry: {}",
                            e
                        )))
                    })?;
                    bytes_written += write_data.len();
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
                .fetch_add(entries.len() as u64, Ordering::Relaxed);

            // Sync to disk if requested
            if sync && self.config.sync_on_flush {
                let sync_guard = self.sync_handle.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref sync_file) = *sync_guard
                    && let Err(e) = sync_file.sync_data()
                {
                    // 🛡️ CRITICAL: If sync fails, we MUST truncate the file back to start_size.
                    // Otherwise, the data we just wrote to the OS cache remains in the file
                    // but we return an error to the client. If the system restarts, this "failed"
                    // transaction would be replayed (Phantom Commit).
                    //
                    // We use the writer_guard to access the underlying file since it is still locked.
                    if let Some(writer) = writer_guard.as_mut() {
                        // writer.flush() was already called above, so data is in the file (OS cache).
                        // We get the mutable reference to the inner File to truncate it.
                        let file = writer.get_mut();

                        // Attempt truncation. If this fails, we are in a very bad state (likely disk failure),
                        // but we must try to rollback the phantom data.
                        if let Err(trunc_err) = file.set_len(start_size) {
                            // We can't do much if truncation fails, but we log it critically.
                            super::log_wal_diagnostic(&format!(
                                "CRITICAL: Failed to truncate WAL segment after sync failure. \
                                 Data consistency may be compromised. Error: {}",
                                trunc_err
                            ));
                        } else {
                            // Truncation succeeded. We successfully prevented a phantom commit.
                            // The file size is now restored to start_size.

                            // 🛡️ CRITICAL: We must also reset the file cursor (seek) back to start_size.
                            // set_len() truncates the file but leaves the cursor at the end of the failed write.
                            // If we don't seek, the next write will start after the hole, creating a sparse file.
                            if let Err(seek_err) = file.seek(SeekFrom::Start(start_size)) {
                                super::log_wal_diagnostic(&format!(
                                    "CRITICAL: Failed to seek WAL segment after sync failure. \
                                     Data consistency may be compromised. Error: {}",
                                    seek_err
                                ));
                            }

                            // Reset in-memory size to match file state (defensive, though flush updates it later)
                            self.current_segment_size
                                .store(start_size, Ordering::Relaxed);
                        }
                    }

                    return Err(Error::Storage(StorageError::IoError(format!(
                        "Failed to sync WAL: {}",
                        e
                    ))));
                }
            }

            // Update size
            self.current_segment_size
                .fetch_add(bytes_written as u64, Ordering::Relaxed);

            // Check for rotation
            let segment_rotated = self.maybe_rotate_segment(&mut writer_guard)?;

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
        };

        // Execute flush logic
        match do_flush() {
            Ok(stats) => {
                // Notify success
                for entry in entries {
                    entry.notify_completion();
                }
                Ok(stats)
            }
            Err(e) => {
                // Notify error to prevent deadlocks (Sentry 🛡️)
                // If we don't notify, threads waiting on these entries will hang forever.
                let error_msg = e.to_string();
                for entry in entries {
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

    /// Delete every sealed old-generation WAL segment during a key rotation
    /// (Issue #3617), identified by HEADER — a legacy header (no key_version) or
    /// a keyversioned header whose `key_version != keep_key_version`.
    ///
    /// Returns the number of `.log` files removed. The active segment
    /// (`current_segment_id`) is never removed. Unlike
    /// [`truncate_to_lsn`](Self::truncate_to_lsn), this does NOT require a
    /// companion `.meta` file (an un-rotated active segment closed by a prior
    /// shutdown has none), so it can retire the pre-rotation tail that
    /// `truncate_to_lsn` would conservatively keep.
    ///
    /// # Safety
    ///
    /// The caller MUST have already captured every old-generation entry in a
    /// durable snapshot outside the WAL (the index manifest, persisted BEFORE
    /// the rotation ledger was written), so deleting these segments loses
    /// nothing. The forward driver and crash-resume both guarantee this.
    pub fn retire_old_generation_segments(&self, keep_key_version: u32) -> Result<usize> {
        let current_id = self.current_segment_id.load(Ordering::Relaxed);
        let mut removed = 0usize;
        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "log") {
                    continue;
                }
                let Some(segment_id) = path
                    .file_stem()
                    .and_then(|s| s.to_string_lossy().parse::<u64>().ok())
                else {
                    continue;
                };
                // Never remove the active segment.
                if segment_id >= current_id {
                    continue;
                }
                let is_old_generation = match Self::read_segment_header_id(&path) {
                    Some((WAL_VERSION_ENCRYPTED_KEYVERSIONED, Some(kv))) => kv != keep_key_version,
                    // Legacy header (pre-#3617) = old generation.
                    Some((_, None)) => true,
                    // Unreadable/empty header: leave it (defensive).
                    _ => false,
                };
                if is_old_generation {
                    // DEFECT #3617-PR2: a `remove_file` failure MUST propagate, not
                    // be swallowed. Swallowing it let the rotation driver report
                    // success (and clear the ledger) while a stranded old-DEK
                    // segment remained on disk — after the operator dropped the old
                    // MEK it could no longer be decrypted, wedging startup and
                    // leaving the DB permanently unopenable. A NotFound race (the
                    // file was already removed) is benign and treated as removed;
                    // any other error propagates so the caller leaves the ledger
                    // pending for a later resume.
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            removed += 1;
                            let _ = std::fs::remove_file(self.segment_meta_path(segment_id));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Already gone (benign race): treat as retired.
                            let _ = std::fs::remove_file(self.segment_meta_path(segment_id));
                        }
                        Err(e) => {
                            return Err(StorageError::IoError(format!(
                                "Failed to retire old-generation WAL segment {path:?}: {e}"
                            ))
                            .into());
                        }
                    }
                }
            }
        }
        Ok(removed)
    }

    /// Whether EVERY on-disk `.log` segment is a keyversioned (v16) segment
    /// stamped with `key_version` (Issue #3617).
    ///
    /// The rotation driver's "old generation fully retired" signal: it returns
    /// `false` if any segment carries a legacy header (no key_version — a
    /// pre-#3617 segment) or a different key_version (an old generation). A file
    /// whose header cannot be read yet (a freshly created segment whose buffered
    /// header has not been flushed to disk) is skipped, not treated as old — so
    /// the just-opened new-generation segment never blocks completion. An empty
    /// directory trivially satisfies the check.
    pub fn all_segments_use_key_version(&self, key_version: u32) -> bool {
        if let Ok(entries) = std::fs::read_dir(&self.config.wal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "log") {
                    continue;
                }
                match Self::read_segment_header_id(&path) {
                    // A keyversioned segment: old generation iff its version differs.
                    Some((WAL_VERSION_ENCRYPTED_KEYVERSIONED, Some(kv))) if kv != key_version => {
                        return false;
                    }
                    // A legacy header (no key_version) is a pre-rotation generation.
                    Some((_, None)) => return false,
                    // New-generation segment, or an unreadable/not-yet-flushed
                    // fresh segment header — neither blocks completion.
                    _ => {}
                }
            }
        }
        true
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
        // 👺 HAVOC FIX: Must acquire the mutex before notifying condvar
        // to prevent a "lost wakeup" race condition where a waiter checks
        // the flag just before we set it, and we notify before they sleep.
        let _guard = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
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
                    super::log_wal_diagnostic(&format!("WAL flush error: {:?}", e));
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

    /// Regression test for Issue #3423: the coordinator must never append
    /// to an existing segment whose header version differs from the version
    /// this writer emits (mixed-version segments fail CRC/parsing on replay
    /// because the parse version comes solely from the file header).
    #[test]
    fn test_ensure_segment_open_rolls_past_mismatched_version_segment() {
        use crate::core::id::NodeId;
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::provenance::Provenance;
        use crate::core::temporal::time;
        use crate::storage::wal::segment_reader::{WAL_VERSION_PROVENANCE, read_entries_from_dir};
        use crate::storage::wal::serialization::{OP_CREATE_NODE, serialize_entry_into};
        use crate::storage::wal::{WalEntry, WalOperation};

        let dir = tempdir().unwrap();

        // Hand-write a genuine v3-header segment at id 1 containing one valid,
        // provenance-less CreateNode with a RAW 4-byte interner-id label (the
        // pre-#3506 label encoding). We build it by hand rather than via the
        // modern serializer, which now writes string labels at v13 — a v13 body
        // under a v3 header would not replay. A provenance-less entry's other
        // bytes are identical across the v3/v5 payload formats, so this file
        // replays cleanly as v3.
        let legacy_label = GLOBAL_INTERNER.intern("Legacy").unwrap();
        let legacy_ts = time::now();
        let mut old_bytes = Vec::new();
        old_bytes.extend_from_slice(&LSN(1).0.to_le_bytes());
        legacy_ts.serialize_into(&mut old_bytes);
        let cs = old_bytes.len();
        old_bytes.extend_from_slice(&[0u8; 4]);
        old_bytes.push(OP_CREATE_NODE);
        old_bytes.extend_from_slice(&NodeId::new(1).unwrap().as_u64().to_le_bytes());
        old_bytes.extend_from_slice(&legacy_label.as_u32().to_le_bytes()); // raw id label
        PropertyMap::new().serialize_into(&mut old_bytes).unwrap();
        legacy_ts.serialize_into(&mut old_bytes);
        old_bytes.push(0u8); // provenance: absent
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&old_bytes[0..20]);
        hasher.update(&old_bytes[cs + 4..]);
        old_bytes[cs..cs + 4].copy_from_slice(&hasher.finalize().to_le_bytes());
        let mut v3_segment = Vec::new();
        v3_segment.extend_from_slice(&WAL_MAGIC);
        v3_segment.push(WAL_VERSION_PROVENANCE);
        v3_segment.extend_from_slice(&old_bytes);
        let old_path = dir.path().join("000001.log");
        std::fs::write(&old_path, &v3_segment).unwrap();

        let config = FlushCoordinatorConfig::new(dir.path());
        let coordinator = FlushCoordinator::new(config).unwrap();

        // Simulate under-reported segment-id allocation (Issue #3423): a
        // failed directory scan (or an externally placed / partially
        // restored segment) leaves the counter below an existing file, so
        // the next allocation collides with the v3 segment. Without the
        // header-version check this would take the append-to-existing path.
        coordinator.current_segment_id.store(0, Ordering::Relaxed);

        // Flush an entry whose provenance carries an authenticated
        // principal (bytes only parseable under the v5 payload format).
        let new_entry = WalEntry::new(
            LSN(2),
            WalOperation::CreateNode {
                node_id: NodeId::new(2).unwrap(),
                label: GLOBAL_INTERNER.intern("Modern").unwrap(),
                properties: PropertyMap::new(),
                valid_from: time::now(),
                provenance: Some(
                    Provenance::builder()
                        .source("test")
                        .principal("alice")
                        .build()
                        .unwrap(),
                ),
            },
        );
        let mut new_bytes = Vec::new();
        serialize_entry_into(&new_entry, &mut new_bytes).unwrap();
        coordinator
            .flush(vec![PendingEntry::new_async(LSN(2), new_bytes)], true)
            .unwrap();

        // The v3 segment must be byte-for-byte untouched...
        assert_eq!(
            std::fs::read(&old_path).unwrap(),
            v3_segment,
            "existing v3 segment must not be appended to"
        );

        // ...the write must have rolled forward to a fresh segment with the
        // current writer (v13 string-labels) header...
        assert_eq!(coordinator.current_segment_id(), 2);
        let new_segment = std::fs::read(dir.path().join("000002.log")).unwrap();
        assert_eq!(&new_segment[0..4], &WAL_MAGIC);
        assert_eq!(new_segment[4], WAL_VERSION_STRING_LABELS);

        // ...and a full-directory replay succeeds, with the new entry's
        // principal intact.
        let replayed = read_entries_from_dir(dir.path(), LSN(0)).unwrap();
        assert_eq!(replayed.len(), 2, "both segments must replay");
        match &replayed[1].operation {
            WalOperation::CreateNode { provenance, .. } => {
                let p = provenance.as_ref().expect("provenance must survive replay");
                assert_eq!(p.principal(), Some("alice"));
                assert_eq!(p.source(), Some("test"));
            }
            other => panic!("unexpected replayed operation: {other:?}"),
        }
    }

    /// A version-matching existing segment IS appended to (the roll-forward
    /// must not trigger for same-version segments).
    #[test]
    fn test_ensure_segment_open_appends_to_version_matching_segment() {
        let dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig::new(dir.path());

        // First coordinator writes a v5 segment at id 1.
        {
            let coordinator = FlushCoordinator::new(config.clone()).unwrap();
            coordinator
                .flush(vec![create_test_entry(1, b"first")], true)
                .unwrap();
        }
        let path = dir.path().join("000001.log");
        let len_before = std::fs::metadata(&path).unwrap().len();

        // Second coordinator with an under-reported counter must reuse it:
        // the header version matches what this writer emits.
        let coordinator = FlushCoordinator::new(config).unwrap();
        coordinator.current_segment_id.store(0, Ordering::Relaxed);
        coordinator
            .flush(vec![create_test_entry(2, b"second")], true)
            .unwrap();

        assert_eq!(coordinator.current_segment_id(), 1);
        let len_after = std::fs::metadata(&path).unwrap().len();
        assert!(
            len_after > len_before,
            "version-matching segment must be appended to in place"
        );
        assert!(
            !dir.path().join("000002.log").exists(),
            "no roll-forward for a version-matching segment"
        );
    }

    /// Regression test for Issue #3423 (secondary hardening): a genuine I/O
    /// failure while scanning the WAL directory for existing segments must
    /// **propagate** rather than being swallowed and silently defaulting the
    /// segment-id counter to 0. Defaulting to 0 after a failed scan is one of
    /// the ways the writer can land on a stale/existing segment; failing loud
    /// lets the operator see the problem instead of risking a poisoned
    /// mixed-version segment.
    #[test]
    fn test_initialize_from_existing_propagates_real_io_error() {
        let dir = tempdir().unwrap();
        let wal_dir = dir.path().join("wal");
        let config = FlushCoordinatorConfig::new(&wal_dir);

        // `new()` creates the directory and scans it successfully.
        let coordinator = FlushCoordinator::new(config).unwrap();

        // Now replace the WAL directory with a regular FILE at the same path.
        // `read_dir` on a non-directory fails with a real I/O error (NOT
        // `NotFound`), which the scan must surface rather than swallow.
        std::fs::remove_dir_all(&wal_dir).unwrap();
        std::fs::write(&wal_dir, b"not a directory").unwrap();

        let result = coordinator.initialize_from_existing();
        assert!(
            result.is_err(),
            "a real read_dir I/O error must propagate, not silently default to segment id 0"
        );
    }

    /// The first-run case -- the WAL directory does not exist yet -- must NOT
    /// be treated as an error: a missing directory is normal on a fresh data
    /// dir and must still succeed with segment id 0.
    #[test]
    fn test_initialize_from_existing_missing_dir_is_ok() {
        let dir = tempdir().unwrap();
        let wal_dir = dir.path().join("wal");
        let config = FlushCoordinatorConfig::new(&wal_dir);

        // `new()` creates the directory; remove it entirely so the scan hits
        // a genuinely absent directory (`NotFound`).
        let coordinator = FlushCoordinator::new(config).unwrap();
        std::fs::remove_dir_all(&wal_dir).unwrap();
        assert!(!wal_dir.exists());

        let result = coordinator.initialize_from_existing();
        assert!(
            result.is_ok(),
            "a missing WAL directory (first run) must succeed, not error"
        );
        assert_eq!(
            coordinator.current_segment_id.load(Ordering::Relaxed),
            0,
            "an absent WAL directory yields segment id 0"
        );
    }

    /// Regression test for Issue #3423 (secondary hardening): a per-entry
    /// iteration error yielded partway through the WAL-directory scan must
    /// **propagate**, not be swallowed by `.flatten()`. Swallowing it could
    /// skip an existing segment, under-report the max id, and default the
    /// writer toward segment id 0. Exercised directly on the extracted
    /// `scan_max_segment_id` helper with a synthetic mid-iteration I/O error,
    /// since a genuine `ReadDir::next()` error is not portably injectable.
    #[test]
    fn test_scan_max_segment_id_propagates_per_entry_error() {
        let wal_dir = Path::new("/some/wal");

        // A well-formed listing with an I/O error yielded between valid
        // entries must surface the error, not skip past it.
        let entries: Vec<std::io::Result<PathBuf>> = vec![
            Ok(wal_dir.join("000001.log")),
            Err(std::io::Error::other("simulated readdir failure")),
            Ok(wal_dir.join("000009.log")),
        ];
        let result = FlushCoordinator::scan_max_segment_id(wal_dir, entries.into_iter());
        assert!(
            result.is_err(),
            "a per-entry iteration error must propagate, not be swallowed"
        );

        // Sanity: an all-Ok listing folds to the max id.
        let ok_entries: Vec<std::io::Result<PathBuf>> = vec![
            Ok(wal_dir.join("000003.log")),
            Ok(wal_dir.join("notes.txt")), // ignored (wrong extension)
            Ok(wal_dir.join("000007.log")),
        ];
        let max = FlushCoordinator::scan_max_segment_id(wal_dir, ok_entries.into_iter()).unwrap();
        assert_eq!(max, 7, "max id folds over well-formed .log entries");
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
        assert_eq!(data[4], WAL_VERSION_STRING_LABELS);
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

        // Flush entries with various LSNs
        let entries = vec![
            create_test_entry(10, &[1u8; 20]),
            create_test_entry(20, &[2u8; 20]),
            create_test_entry(15, &[3u8; 20]),
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
            assert_eq!(meta.max_lsn, LSN(20));
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
    fn test_phantom_commit_prevention() {
        // 🛡️ Sentry Test: Verify that if sync fails, the data flushed to OS cache is rolled back.
        // This prevents "Phantom Commits" where a client gets an error but the data persists
        // and reappears after restart/recovery.

        use tempfile::tempdir;

        // 1. Setup coordinator with sync_on_flush = true
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.sync_on_flush = true;
        let coordinator = FlushCoordinator::new(config).unwrap();

        // 2. Write initial valid entry
        let entry1 = create_test_entry(1, b"valid_data");
        coordinator.flush(vec![entry1], true).unwrap();

        // Get file size after valid write
        let segment_path = coordinator.segment_path(coordinator.current_segment_id());
        let valid_size = std::fs::metadata(&segment_path).unwrap().len();

        // 3. Sabotage the sync handle (replace with a File wrapping a UnixStream)
        // fsync on a socket returns EINVAL on Linux, guaranteeing failure.
        // We use IntoRawFd to transfer ownership to the File, preventing double-close.
        {
            use std::os::unix::io::{FromRawFd, IntoRawFd};
            use std::os::unix::net::UnixStream;

            let (s1, _s2) = UnixStream::pair().expect("Failed to create socket pair");
            let fd = s1.into_raw_fd();
            // SAFETY: We own the FD (transferred from UnixStream).
            let bad_file = unsafe { File::from_raw_fd(fd) };

            let mut guard = coordinator.sync_handle.lock().unwrap();
            *guard = Some(bad_file);
        }

        // 4. Attempt to write phantom entry
        let entry2 = create_test_entry(2, b"phantom_data");
        let result = coordinator.flush(vec![entry2], true);

        // 5. Assertions
        assert!(
            result.is_err(),
            "Flush should fail due to broken sync handle"
        );

        // CRITICAL CHECK: The file size must NOT have increased.
        // If it increased, the phantom data is in the file (OS cache) and will persist.
        let new_size = std::fs::metadata(&segment_path).unwrap().len();

        assert_eq!(
            new_size, valid_size,
            "File size increased despite sync failure! Phantom commit detected. \
             Expected {} bytes (valid only), got {} bytes (valid + phantom).",
            valid_size, new_size
        );
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
    fn test_truncate_to_lsn_boundary_exact_match() {
        // 🤖 Sentinel: This test kills the mutant where `max_lsn < truncate_lsn` is replaced with `max_lsn <= truncate_lsn`.
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 50; // Small size
        config.segments_to_retain = 100; // Manual control

        let coordinator = FlushCoordinator::new(config).unwrap();

        // 1. Create a segment with LSN range [10, 20]
        // 20 bytes * 11 entries = 220 bytes.
        let entries: Vec<_> = (10..=20)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries, true).unwrap();

        // 2. Force rotation by flushing a new batch
        let entries2: Vec<_> = (21..=30)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries2, true).unwrap();

        // Verify we have the historical segment (LSN 10-20)
        let segments = coordinator.list_segments_with_metadata();
        // The first segment should be the one ending at 20.
        // There should be at least one segment (the active one) and one historical one.
        assert!(segments.len() >= 2);
        let historical = segments
            .iter()
            .find(|(_, m)| m.max_lsn == LSN(20))
            .expect("Historical segment should exist");

        // 3. Truncate to LSN 20.
        // The segment ends at 20. It contains entry 20.
        // It should NOT be removed.
        let removed = coordinator.truncate_to_lsn(LSN(20)).unwrap();

        assert_eq!(
            removed, 0,
            "Should NOT remove segment ending at LSN 20 when truncating to 20"
        );

        // Verify it still exists
        let segments_after = coordinator.list_segments_with_metadata();
        assert!(segments_after.iter().any(|(id, _)| *id == historical.0));
    }

    #[test]
    fn test_get_min_lsn_with_multiple_segments() {
        // 🤖 Sentinel: This test kills the mutant where `.min()` is replaced with `.max()` in `get_min_lsn`.
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 30;
        config.segments_to_retain = 100;

        let coordinator = FlushCoordinator::new(config).unwrap();

        // Create Segment 1: LSN 10-20
        let entries1: Vec<_> = (10..=20)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries1, true).unwrap();

        // Create Segment 2: LSN 30-40
        let entries2: Vec<_> = (30..=40)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries2, true).unwrap();

        // Create Segment 3: LSN 50-60
        let entries3: Vec<_> = (50..=60)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries3, true).unwrap();

        // Force rotation to ensure all previous segments have metadata written
        coordinator
            .flush(vec![create_test_entry(100, &[100u8; 100])], true)
            .unwrap();

        // Check min LSN
        // We have segments starting at 10, 30, 50.
        // min() should be 10.
        // max() would be 50.
        let min_lsn = coordinator.get_min_lsn();
        assert_eq!(min_lsn, Some(LSN(10)));
    }

    /// DEFECT #3617-PR2: a real `remove_file` failure during old-generation
    /// retirement MUST propagate, not be swallowed. Previously a swallowed error
    /// let the rotation driver report success (and clear the ledger) while a
    /// stranded old-DEK segment remained on disk — after the old MEK was dropped
    /// it could no longer be decrypted, wedging startup permanently.
    #[cfg(unix)]
    #[test]
    fn retire_old_generation_propagates_remove_file_errors() {
        use std::os::unix::fs::PermissionsExt;

        // The read-only-directory injection below relies on DAC permission
        // checks, which the root user bypasses (unlink still succeeds for uid 0).
        // Skip under root — the same propagation is exercised end-to-end under any
        // uid by the rotation-level completion-gating test
        // (`incomplete_wal_retirement_leaves_ledger_pending_and_no_success`).
        // SAFETY: `geteuid` is an always-safe libc call with no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempdir().unwrap();
        let wal_dir = dir.path();

        // Coordinator over an (initially empty) dir — no active segment is created
        // eagerly, so no file collides with our planted old-generation segment.
        let config = FlushCoordinatorConfig::new(wal_dir);
        let coordinator = FlushCoordinator::new(config).unwrap();
        // Make the active id high, so id 0 below is treated as OLD (id < active).
        coordinator.current_segment_id.store(5, Ordering::Relaxed);

        // Plant a legacy (old-generation) segment header at id 0.
        let mut hdr = Vec::new();
        hdr.extend_from_slice(&WAL_MAGIC);
        hdr.push(WAL_VERSION_STRING_LABELS); // legacy header (no key_version)
        std::fs::write(wal_dir.join("000000.log"), &hdr).unwrap();

        // Make the directory read-only so `remove_file` fails with PermissionDenied
        // (removal needs write permission on the parent directory, not the file).
        let original = std::fs::metadata(wal_dir).unwrap().permissions();
        let mut ro = original.clone();
        ro.set_mode(0o500);
        std::fs::set_permissions(wal_dir, ro).unwrap();

        let result = coordinator.retire_old_generation_segments(2);

        // Restore permissions BEFORE asserting so tempdir cleanup succeeds.
        std::fs::set_permissions(wal_dir, original).unwrap();

        assert!(
            result.is_err(),
            "a real remove_file failure must propagate as Err, not be swallowed"
        );
    }

    /// Issue #3355 review finding: once `truncate_to_lsn` removes EVERY
    /// sealed segment, `get_min_lsn` returns `None` (no metadata left) --
    /// indistinguishable from a fresh WAL -- so the replication feed would
    /// hand a lagging replica a gapped batch instead of `ResyncRequired`.
    /// `get_replication_min_lsn` must keep answering from the truncation
    /// watermark in that state.
    #[test]
    fn test_replication_min_lsn_survives_full_sealed_truncation() {
        let dir = tempdir().unwrap();
        let mut config = FlushCoordinatorConfig::new(dir.path());
        config.segment_size = 50;
        config.segments_to_retain = 100;

        let coordinator = FlushCoordinator::new(config).unwrap();

        // Seal a segment spanning LSN 10..=20, then start a new active one.
        let entries: Vec<_> = (10..=20)
            .map(|i| create_test_entry(i, &[i as u8; 20]))
            .collect();
        coordinator.flush(entries, true).unwrap();
        coordinator
            .flush(vec![create_test_entry(21, &[21u8; 20])], true)
            .unwrap();

        // Fresh coordinator, nothing truncated: both answers agree.
        assert!(coordinator.get_replication_min_lsn().is_some());

        // Truncate away every sealed segment.
        let removed = coordinator.truncate_to_lsn(LSN(21)).unwrap();
        assert!(removed >= 1, "the sealed segment must be removed");

        // Plain get_min_lsn may now be None (only the active segment, which
        // has no metadata file, remains) -- but the replication view must
        // still report that history up to LSN 20 is gone.
        let min = coordinator
            .get_replication_min_lsn()
            .expect("watermark must survive full sealed truncation");
        assert_eq!(
            min,
            LSN(21),
            "min available after truncating [10, 20] must be 21"
        );
    }

    /// No stderr `print` macro may remain in this module's production code.
    ///
    /// The deepest error paths here -- a segment header mismatch, a failed
    /// post-sync truncate, a failed post-sync seek, and the legacy
    /// [`FlushThread`] drain loop -- all execute on a background flush/drain
    /// thread. The `std` print macros PANIC when the write fails, and a
    /// daemonized process whose stderr pipe has been closed hits exactly that
    /// (EPIPE), so a panic fires precisely while the never-drop-an-outcome
    /// machinery is handling a flush failure: the flusher dies mid-recovery
    /// and nothing drains the ring buffers afterwards, which IS the #3798
    /// stall (Issue #3798 review round 3).
    ///
    /// Unlike the marker-scoped scan in `concurrent_system`, this one is
    /// file-wide up to the test module: every production path in this file is
    /// either the flush thread's own or background maintenance sharing its
    /// hazard, so there is no region here where a print macro would be safe.
    /// Route diagnostics through `super::log_wal_diagnostic` instead.
    #[test]
    fn test_no_panicking_print_macros_in_flush_coordinator() {
        const SOURCE: &str = include_str!("flush_coordinator.rs");

        // The boundary and the needles are assembled at runtime so this
        // test's own source can never match itself.
        let boundary = format!("{}{}", "mod te", "sts {");
        let production = match SOURCE.find(boundary.as_str()) {
            Some(at) => &SOURCE[..at],
            None => panic!("the test module declaration moved; this guard scans the wrong range"),
        };

        // Coverage anchor: a boundary that slid to the top of the file would
        // keep the scan green while guarding no code at all.
        assert!(
            production.contains("pub fn flush(&self"),
            "the scanned range no longer covers `FlushCoordinator::flush`: the guard is \
             watching the wrong code"
        );

        for needle in [
            format!("{}{}", "eprint", "ln!"),
            format!("{}{}", "print", "ln!"),
        ] {
            let hits = production.matches(needle.as_str()).count();
            assert_eq!(
                hits, 0,
                "found {} `{}` in flush-coordinator production code: logging on the flush \
                 thread must go through the non-panicking `super::log_wal_diagnostic` helper \
                 (the print macros panic on EPIPE, killing the only drainer)",
                hits, needle
            );
        }
    }
}
