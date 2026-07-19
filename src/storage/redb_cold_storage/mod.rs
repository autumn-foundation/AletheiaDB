//! Redb-based cold storage backend for tiered storage architecture.
//!
//! This module implements the "Cold" tier of AletheiaDB's storage hierarchy using [Redb](https://github.com/cberner/redb),
//! a pure Rust embedded database. It provides durable, disk-based storage for historical data that has
//! aged out of the "Warm" memory tier.
//!
//! # Role in Architecture
//!
//! 1.  **Historical Storage**: Persists `NodeVersion` and `EdgeVersion` objects that represent the
//!     state of the graph at past points in time.
//! 2.  **WAL Truncation Coordination**: Tracks the "Flushed LSN" (Log Sequence Number). The Write-Ahead
//!     Log (WAL) can only be truncated up to the point that has been safely persisted here.
//! 3.  **Compression**: Automatically compresses data using Zstd or LZ4 (via `compression` module)
//!     to minimize disk usage.
//!
//! # Schema
//!
//! The storage uses three internal tables:
//! - **`node_versions`**: Maps `VersionId` (u64) -> Compressed `NodeVersion` (bytes).
//! - **`edge_versions`**: Maps `VersionId` (u64) -> Compressed `EdgeVersion` (bytes).
//! - **`metadata`**: Singleton table storing system state, primarily the `flushed_lsn`.
//!
//! # Concurrency
//!
//! Redb provides ACID guarantees. Read transactions are snapshot-isolated and non-blocking.
//! Write transactions are serialized (one active writer at a time). This module handles
//! the transaction management internally.
//!
//! # Example
//!
//! ```no_run
//! use aletheiadb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
//! use aletheiadb::storage::wal::LSN;
//! use aletheiadb::core::version::{NodeVersion, EdgeVersion};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let config = RedbConfig::default();
//! let storage = RedbColdStorage::new("data/cold.redb", config)?;
//!
//! // Store versions with LSN tracking (atomic batch)
//! let node_versions: Vec<NodeVersion> = vec![]; // ... populate
//! let edge_versions: Vec<EdgeVersion> = vec![]; // ... populate
//! let lsn = LSN(1000);
//!
//! storage.store_batch_with_lsn(&node_versions, &edge_versions, lsn)?;
//!
//! // Get flushed LSN for WAL truncation
//! let flushed_lsn = storage.get_flushed_lsn()?;
//! assert_eq!(flushed_lsn, Some(lsn));
//! # Ok(())
//! # }
//! ```

use crate::core::changefeed::{
    BoundedChanges, ChangeCursor, EntityKind, RawChange, consider_version,
};
use crate::core::error::{Result, StorageError};
use crate::core::id::VersionId;
use crate::core::namespace::{
    NamespaceId, intern_namespace, namespace_of, unresolved_namespace_id,
};
use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, Timestamp};
use crate::core::version::{EdgeVersion, EntityVersion, NodeVersion, TemporalVersion, VersionData};
use crate::storage::wal::LSN;
use arc_swap::ArcSwapOption;
use rayon::prelude::*;
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, TableHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(test)]
use std::sync::atomic::AtomicBool;

mod keyring;

pub use keyring::ColdKeyring;
use keyring::{parse_cold_wrapper, wrap_cold_value};

// Table definitions with static lifetimes
const NODE_VERSIONS_TABLE: redb::TableDefinition<'static, u64, &'static [u8]> =
    redb::TableDefinition::new("node_versions");
const EDGE_VERSIONS_TABLE: redb::TableDefinition<'static, u64, &'static [u8]> =
    redb::TableDefinition::new("edge_versions");
const METADATA_TABLE: redb::TableDefinition<'static, &'static str, &'static [u8]> =
    redb::TableDefinition::new("metadata");

/// Metadata keys stored in the metadata table.
const FLUSHED_LSN_KEY: &str = "flushed_lsn";

/// Metadata key for the persisted cold-tier bi-temporal extent (Issue #3389).
const TEMPORAL_EXTENT_KEY: &str = "temporal_extent";

/// Metadata key for the durable cold key-rotation resume cursor (Issue #3617
/// PR3). Present only WHILE a bulk re-encrypt pass is in flight; advanced in the
/// SAME redb write transaction as each batch of value rewrites (so it is atomic
/// with them) and deleted on completion.
const COLD_ROTATION_CURSOR_KEY: &str = "cold_rotation";

/// Metadata key for the terminal cold value-format marker (Issue #3617 PR3).
/// Written once every value is re-wrapped under the target generation: records
/// `wrapped@{target_version}`, the whole-store flag that authoritatively
/// resolves any residual legacy/wrapped ambiguity.
const COLD_VALUE_FORMAT_KEY: &str = "cold_value_format";

/// Default batch size for the transactional bulk re-encrypt pass (Issue #3617
/// PR3): the number of `(key, value)` pairs rewritten per redb write
/// transaction. Bounds per-transaction memory and work; the pass is resumable
/// at batch granularity via [`COLD_ROTATION_CURSOR_KEY`], so a larger value
/// trades a longer crash-replay window for fewer commits. 4096 keeps each
/// transaction's materialized slice small while amortizing commit cost. Runtime
/// tunable via [`RedbConfig::reencrypt_batch_size`] /
/// [`RedbConfig::with_reencrypt_batch_size`]; this const is the unset default.
const COLD_REENCRYPT_BATCH_SIZE: usize = 4096;

/// Layout-version tag for the hand-rolled [`COLD_ROTATION_CURSOR_KEY`] /
/// [`COLD_VALUE_FORMAT_KEY`] records. Records with an unrecognized tag are
/// treated as absent (an older binary falls back to a full pass rather than
/// misreading bytes), mirroring [`EXTENT_RECORD_VERSION`].
const COLD_ROTATION_RECORD_VERSION: u8 = 1;

/// Which value-bearing table a cold rotation cursor is pointing into. Encoded as
/// a single discriminant byte in the durable cursor record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColdTableKind {
    Node,
    Edge,
}

impl ColdTableKind {
    fn as_u8(self) -> u8 {
        match self {
            ColdTableKind::Node => 0,
            ColdTableKind::Edge => 1,
        }
    }

    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ColdTableKind::Node),
            1 => Some(ColdTableKind::Edge),
            _ => None,
        }
    }

    fn table(self) -> redb::TableDefinition<'static, u64, &'static [u8]> {
        match self {
            ColdTableKind::Node => NODE_VERSIONS_TABLE,
            ColdTableKind::Edge => EDGE_VERSIONS_TABLE,
        }
    }
}

/// Durable resume cursor for the cold bulk re-encrypt pass (Issue #3617 PR3).
///
/// Hand-rolled fixed-width bytes (no serde, so the Feature Matrix stays clean):
/// `[version:u8][target_version:u32 LE][table:u8][last_completed_key:u64 LE]`.
/// `last_completed_key` is the highest `VersionId` already re-wrapped in
/// `table`; the pass resumes at `> last_completed_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColdRotationCursor {
    target_version: u32,
    table: ColdTableKind,
    last_completed_key: u64,
}

impl ColdRotationCursor {
    const LEN: usize = 1 + 4 + 1 + 8;

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0] = COLD_ROTATION_RECORD_VERSION;
        out[1..5].copy_from_slice(&self.target_version.to_le_bytes());
        out[5] = self.table.as_u8();
        out[6..14].copy_from_slice(&self.last_completed_key.to_le_bytes());
        out
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::LEN || bytes[0] != COLD_ROTATION_RECORD_VERSION {
            return None;
        }
        let target_version = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        let table = ColdTableKind::from_u8(bytes[5])?;
        let last_completed_key = u64::from_le_bytes([
            bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13],
        ]);
        Some(Self {
            target_version,
            table,
            last_completed_key,
        })
    }
}

/// How the shared cold value-migration driver obtains the plaintext to encrypt
/// under the target generation for each source value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColdSourceMode {
    /// Rekey (Issue #3617 PR3): decrypt each source value under its own
    /// (old/legacy) generation, then re-encrypt under the target DEK.
    Rekey,
    /// Enable wrap-only (Issue #3616 PR3): each un-`ACV1` source value is BARE
    /// plaintext (never encrypted), used directly as the plaintext to encrypt.
    WrapPlaintext,
}

/// Statistics returned by a cold bulk re-encrypt pass (Issue #3617 PR3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColdReencryptStats {
    /// Values re-encrypted+re-wrapped under the target generation.
    pub values_rewrapped: usize,
    /// Values skipped because they were already wrapped at the target version
    /// (idempotent re-run / stale cursor).
    pub values_skipped: usize,
    /// Number of redb write transactions committed across the whole pass, one
    /// per non-empty batch (bounded by the configurable
    /// [`RedbConfig::reencrypt_batch_size`]). Lets callers/tests observe that a
    /// small batch cap was honored (a multi-batch pass reports `> 1`).
    pub batches_committed: usize,
}

/// Statistics returned by a cold `ACV1` → bare unwrap pass (Issue #3616 PR4
/// disable — the inverse of [`wrap_plaintext_cold_values`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColdUnwrapStats {
    /// Values decrypted out of their `ACV1` wrapper and rewritten BARE.
    pub values_unwrapped: usize,
    /// Values skipped because they were already bare (idempotent re-run /
    /// resumed pass).
    pub values_skipped: usize,
    /// Number of redb write transactions committed across the whole pass, one
    /// per non-empty batch (bounded by [`RedbConfig::reencrypt_batch_size`]).
    pub batches_committed: usize,
}

/// Layout version tag for the persisted extent record. Records with an
/// unrecognized version are treated as absent, so an older binary that cannot
/// understand a newer layout simply falls back to hot-tier-only bounds rather
/// than misreading bytes.
const EXTENT_RECORD_VERSION: u8 = 1;

/// Byte length of a serialized [`ColdTemporalExtentBounds`]:
/// 1 version tag + 4 dimensions × (1 presence flag + 8 wallclock + 4 logical).
const EXTENT_RECORD_LEN: usize = 1 + 4 * 13;

/// Persisted bi-temporal extent bounds for the cold tier (Issue #3389).
///
/// Cold-migrated versions are removed from the hot-tier temporal indexes, so
/// after a restart the write-time extent aggregate — which is rebuilt from the
/// hot tier only — would silently omit them, wrongly narrowing the reported
/// [`temporal_extent`](crate::AletheiaDB::temporal_extent). To close that gap
/// the cold store maintains these bounds incrementally as versions are written
/// to it, persisted durably in the `metadata` table;
/// [`RedbColdStorage::get_temporal_extent_bounds`] returns them at startup so
/// they can be merged back into the aggregate.
///
/// Bounds follow the exact convention of the in-memory extent aggregate:
/// `earliest` is the minimum interval start; `latest` is the maximum of
/// interval starts and *closed* interval ends — an open (`TIMESTAMP_MAX`) end
/// contributes only its start, so the open-interval sentinel never leaks. All
/// four fields are `None` on an empty cold tier and become `Some` together
/// once any version is stored. Merging is monotonic (bounds only ever widen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColdTemporalExtentBounds {
    /// Minimum valid-time interval start observed across cold versions.
    pub valid_earliest: Option<Timestamp>,
    /// Maximum finite valid-time coordinate observed across cold versions.
    pub valid_latest: Option<Timestamp>,
    /// Minimum transaction-time interval start observed across cold versions.
    pub tx_earliest: Option<Timestamp>,
    /// Maximum finite transaction-time coordinate observed across cold versions.
    pub tx_latest: Option<Timestamp>,
}

impl ColdTemporalExtentBounds {
    /// `true` when no bounds have been observed (all fields `None`).
    fn is_empty(&self) -> bool {
        self.valid_earliest.is_none()
            && self.valid_latest.is_none()
            && self.tx_earliest.is_none()
            && self.tx_latest.is_none()
    }

    /// Fold one `[start, end)` range into a single dimension's bounds, applying
    /// the extent convention: `earliest` = min start; `latest` = max of starts
    /// and *closed* ends (an open `TIMESTAMP_MAX` end contributes only its
    /// start, so the sentinel never leaks).
    fn observe_range(
        earliest: &mut Option<Timestamp>,
        latest: &mut Option<Timestamp>,
        start: Timestamp,
        end: Timestamp,
    ) {
        *earliest = Some(earliest.map_or(start, |e| e.min(start)));
        // TimeRange guarantees end >= start, so a closed end is itself the
        // latest candidate; open ends fall back to the start.
        let candidate = if end == TIMESTAMP_MAX { start } else { end };
        *latest = Some(latest.map_or(candidate, |l| l.max(candidate)));
    }

    /// Fold one bi-temporal interval (both dimensions) into these bounds.
    fn observe_interval(&mut self, temporal: &BiTemporalInterval) {
        let valid = temporal.valid_time();
        Self::observe_range(
            &mut self.valid_earliest,
            &mut self.valid_latest,
            valid.start(),
            valid.end(),
        );
        let tx = temporal.transaction_time();
        Self::observe_range(
            &mut self.tx_earliest,
            &mut self.tx_latest,
            tx.start(),
            tx.end(),
        );
    }

    /// Accumulate the bounds contributed by a batch of versions.
    fn from_versions<V: TemporalVersion>(versions: &[V]) -> Self {
        let mut bounds = Self::default();
        for version in versions {
            bounds.observe_interval(version.temporal());
        }
        bounds
    }

    /// Merge another set of bounds into these (monotonic widen-only union).
    fn merge(&mut self, other: &Self) {
        Self::merge_dim(
            &mut self.valid_earliest,
            &mut self.valid_latest,
            other.valid_earliest,
            other.valid_latest,
        );
        Self::merge_dim(
            &mut self.tx_earliest,
            &mut self.tx_latest,
            other.tx_earliest,
            other.tx_latest,
        );
    }

    fn merge_dim(
        earliest: &mut Option<Timestamp>,
        latest: &mut Option<Timestamp>,
        other_earliest: Option<Timestamp>,
        other_latest: Option<Timestamp>,
    ) {
        if let Some(e) = other_earliest {
            *earliest = Some(earliest.map_or(e, |cur| cur.min(e)));
        }
        if let Some(l) = other_latest {
            *latest = Some(latest.map_or(l, |cur| cur.max(l)));
        }
    }

    /// Serialize to the fixed-length metadata record layout.
    fn to_bytes(self) -> [u8; EXTENT_RECORD_LEN] {
        let mut buf = [0u8; EXTENT_RECORD_LEN];
        buf[0] = EXTENT_RECORD_VERSION;
        let mut offset = 1;
        for field in [
            self.valid_earliest,
            self.valid_latest,
            self.tx_earliest,
            self.tx_latest,
        ] {
            if let Some(ts) = field {
                buf[offset] = 1;
                buf[offset + 1..offset + 9].copy_from_slice(&ts.wallclock().to_le_bytes());
                buf[offset + 9..offset + 13].copy_from_slice(&ts.logical().to_le_bytes());
            }
            offset += 13;
        }
        buf
    }

    /// Deserialize from the metadata record layout. Returns `Ok(None)` for an
    /// unrecognized version tag or wrong length (treated as absent), never an
    /// error, so a forward-incompatible record can never panic startup.
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != EXTENT_RECORD_LEN || bytes[0] != EXTENT_RECORD_VERSION {
            return None;
        }
        let read_field = |offset: usize| -> Option<Timestamp> {
            if bytes[offset] == 0 {
                return None;
            }
            let mut wall = [0u8; 8];
            wall.copy_from_slice(&bytes[offset + 1..offset + 9]);
            let mut logical = [0u8; 4];
            logical.copy_from_slice(&bytes[offset + 9..offset + 13]);
            // Values were validated when the source versions were created, so
            // reconstructing them from trusted internal storage is sound.
            Some(Timestamp::new_unchecked(
                i64::from_le_bytes(wall),
                u32::from_le_bytes(logical),
            ))
        };
        Some(Self {
            valid_earliest: read_field(1),
            valid_latest: read_field(14),
            tx_earliest: read_field(27),
            tx_latest: read_field(40),
        })
    }
}

/// Batch size threshold where parallel pre-compression becomes worthwhile.
const PARALLEL_COMPRESSION_THRESHOLD: usize = 1_024;

#[derive(Debug, Default)]
struct PreparedVersionBatch {
    entries: Vec<(u64, Vec<u8>)>,
    raw_size_bytes: u64,
    compressed_size_bytes: u64,
}

impl PreparedVersionBatch {
    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            raw_size_bytes: 0,
            compressed_size_bytes: 0,
        }
    }

    #[inline]
    fn add_entry(&mut self, version_id: VersionId, payload: Vec<u8>, raw_size_bytes: u64) {
        self.raw_size_bytes += raw_size_bytes;
        self.compressed_size_bytes += payload.len() as u64;
        self.entries.push((version_id.as_u64(), payload));
    }

    #[inline]
    fn merge(&mut self, mut other: Self) {
        self.raw_size_bytes += other.raw_size_bytes;
        self.compressed_size_bytes += other.compressed_size_bytes;
        self.entries.append(&mut other.entries);
    }
}

// ============================================================================
// Types moved from cold_storage.rs
// ============================================================================

/// Compression algorithm for cold storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum CompressionAlgorithm {
    /// No compression (fastest, but uses more disk space)
    None,
    /// Zstd compression (ratio-optimized, default)
    #[default]
    Zstd,
    /// LZ4-compatible fast compression using Zstd level 1
    /// Provides similar speed characteristics to LZ4 with better compatibility
    Fast,
}

impl CompressionAlgorithm {
    /// Get the `zstd` compression level for this algorithm.
    ///
    /// The compression level balances speed and ratio. This method translates
    /// the abstract `CompressionAlgorithm` enum into the specific integer level
    /// expected by the `zstd` crate.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::CompressionAlgorithm;
    ///
    /// assert_eq!(CompressionAlgorithm::Zstd.zstd_level(), Some(3));
    /// assert_eq!(CompressionAlgorithm::Fast.zstd_level(), Some(1));
    /// assert_eq!(CompressionAlgorithm::None.zstd_level(), None);
    /// ```
    pub fn zstd_level(&self) -> Option<i32> {
        match self {
            CompressionAlgorithm::None => None,
            CompressionAlgorithm::Zstd => Some(3), // Balanced ratio/speed
            CompressionAlgorithm::Fast => Some(1), // Speed-optimized
        }
    }
}

/// Configuration for cold storage.
///
/// Configures behavior like compression, batch sizes, and durability.
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::storage::redb_cold_storage::{ColdStorageConfig, CompressionAlgorithm};
///
/// let config = ColdStorageConfig::default();
/// assert!(matches!(config.compression, CompressionAlgorithm::Zstd));
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct ColdStorageConfig {
    /// Compression algorithm to use.
    pub compression: CompressionAlgorithm,

    /// Whether to sync writes to disk immediately.
    /// When false, relies on OS buffer cache (faster but less durable).
    pub sync_writes: bool,

    /// Maximum number of versions to batch in a single write operation.
    /// Higher values improve throughput but increase memory usage.
    pub batch_size: usize,

    /// Enable CRC32 checksums for data integrity verification.
    pub enable_checksums: bool,
}

impl Default for ColdStorageConfig {
    fn default() -> Self {
        Self {
            compression: CompressionAlgorithm::Zstd,
            sync_writes: false,
            batch_size: 1000,
            enable_checksums: true,
        }
    }
}

/// Statistics for cold storage operations.
///
/// Tracks bytes written, read, compression ratios, and error counts.
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::storage::redb_cold_storage::ColdStorageStats;
///
/// let mut stats = ColdStorageStats::default();
/// stats.bytes_written_raw = 1000;
/// stats.bytes_written_compressed = 500;
/// assert_eq!(stats.compression_ratio(), 2.0);
/// ```
#[derive(Debug, Clone, Default)]
pub struct ColdStorageStats {
    /// Total number of node versions stored. Seeded from the persisted table
    /// length when an existing database is opened, then incremented per store.
    pub node_versions_stored: u64,
    /// Total number of edge versions stored. Seeded from the persisted table
    /// length when an existing database is opened, then incremented per store.
    pub edge_versions_stored: u64,
    /// Total number of node version reads.
    pub node_version_reads: u64,
    /// Total number of edge version reads.
    pub edge_version_reads: u64,
    /// Total bytes written (before compression).
    pub bytes_written_raw: u64,
    /// Total bytes written (after compression).
    pub bytes_written_compressed: u64,
    /// Total bytes read (compressed size from storage).
    pub bytes_read_compressed: u64,
    /// Total bytes read (after decompression).
    pub bytes_read_decompressed: u64,
    /// Number of read errors.
    pub read_errors: u64,
    /// Number of write errors.
    pub write_errors: u64,
}

impl ColdStorageStats {
    /// Calculate the compression ratio (raw bytes divided by compressed bytes).
    ///
    /// This helps monitor the effectiveness of the chosen compression algorithm
    /// in the cold storage tier. A higher ratio indicates better compression.
    ///
    /// The underlying byte counters are not persisted, so this ratio covers
    /// writes made since the storage was opened (1.0 when nothing has been
    /// written yet by this process).
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::ColdStorageStats;
    ///
    /// let mut stats = ColdStorageStats::default();
    /// stats.bytes_written_raw = 1000;
    /// stats.bytes_written_compressed = 250;
    /// assert_eq!(stats.compression_ratio(), 4.0); // 4x compression!
    /// ```
    pub fn compression_ratio(&self) -> f64 {
        if self.bytes_written_compressed == 0 {
            1.0
        } else {
            self.bytes_written_raw as f64 / self.bytes_written_compressed as f64
        }
    }
}

/// Atomic statistics tracker for cold storage.
#[derive(Debug, Default)]
pub struct AtomicColdStorageStats {
    /// Total number of node versions stored.
    pub node_versions_stored: AtomicU64,
    /// Total number of edge versions stored.
    pub edge_versions_stored: AtomicU64,
    /// Total number of node version reads.
    pub node_version_reads: AtomicU64,
    /// Total number of edge version reads.
    pub edge_version_reads: AtomicU64,
    /// Total bytes written (before compression).
    pub bytes_written_raw: AtomicU64,
    /// Total bytes written (after compression).
    pub bytes_written_compressed: AtomicU64,
    /// Total bytes read (compressed size from storage).
    pub bytes_read_compressed: AtomicU64,
    /// Total bytes read (after decompression).
    pub bytes_read_decompressed: AtomicU64,
    /// Number of read errors.
    pub read_errors: AtomicU64,
    /// Number of write errors.
    pub write_errors: AtomicU64,
}

impl AtomicColdStorageStats {
    /// Create a new atomic statistics tracker.
    ///
    /// Initializes all atomic counters to zero. This is used by the cold storage
    /// backend to track metrics across multiple concurrent threads safely.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::AtomicColdStorageStats;
    ///
    /// let stats = AtomicColdStorageStats::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a point-in-time snapshot of the current statistics.
    ///
    /// Uses relaxed memory ordering to gather all metrics without expensive locking.
    /// Since the counters are independent, the snapshot might be slightly "fuzzy"
    /// during high contention, but it's perfect for observability.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::AtomicColdStorageStats;
    /// use std::sync::atomic::Ordering;
    ///
    /// let atomic_stats = AtomicColdStorageStats::new();
    /// atomic_stats.bytes_written_raw.store(500, Ordering::Relaxed);
    ///
    /// let snapshot = atomic_stats.snapshot();
    /// assert_eq!(snapshot.bytes_written_raw, 500);
    /// ```
    pub fn snapshot(&self) -> ColdStorageStats {
        ColdStorageStats {
            node_versions_stored: self.node_versions_stored.load(Ordering::Relaxed),
            edge_versions_stored: self.edge_versions_stored.load(Ordering::Relaxed),
            node_version_reads: self.node_version_reads.load(Ordering::Relaxed),
            edge_version_reads: self.edge_version_reads.load(Ordering::Relaxed),
            bytes_written_raw: self.bytes_written_raw.load(Ordering::Relaxed),
            bytes_written_compressed: self.bytes_written_compressed.load(Ordering::Relaxed),
            bytes_read_compressed: self.bytes_read_compressed.load(Ordering::Relaxed),
            bytes_read_decompressed: self.bytes_read_decompressed.load(Ordering::Relaxed),
            read_errors: self.read_errors.load(Ordering::Relaxed),
            write_errors: self.write_errors.load(Ordering::Relaxed),
        }
    }
}

// ============================================================================
// Error Handling Helpers
// ============================================================================

#[inline]
fn map_io_error(context: &str) -> impl Fn(std::io::Error) -> crate::core::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_db_error(context: &str) -> impl Fn(redb::DatabaseError) -> crate::core::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_table_error(context: &str) -> impl Fn(redb::TableError) -> crate::core::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_commit_error(context: &str) -> impl Fn(redb::CommitError) -> crate::core::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_transaction_error(
    context: &str,
) -> impl Fn(redb::TransactionError) -> crate::core::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_storage_error(
    context: &str,
) -> impl Fn(redb::StorageError) -> crate::core::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_compaction_error(
    context: &str,
) -> impl Fn(redb::CompactionError) -> crate::core::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

/// Configuration for Redb cold storage.
///
/// Used to tune the internal Redb environment and setup cache sizes,
/// compression, and checksumming.
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::storage::redb_cold_storage::{RedbConfig, CompressionAlgorithm};
///
/// let config = RedbConfig::new()
///     .compression(CompressionAlgorithm::Fast)
///     .enable_checksums(true)
///     .cache_size_bytes(1024 * 1024 * 64); // 64MB cache
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct RedbConfig {
    /// Compression algorithm for stored values.
    pub compression: CompressionAlgorithm,

    /// Enable CRC32 checksums for data integrity (in addition to Redb's built-in checksums).
    pub enable_checksums: bool,

    /// Cache size in bytes for Redb (0 = use default).
    pub cache_size_bytes: usize,

    /// Number of `(key, value)` pairs re-encrypted per redb write transaction
    /// during a cold-tier key-rotation bulk re-encrypt pass (Issue #3617 PR3).
    ///
    /// Defaults to [`COLD_REENCRYPT_BATCH_SIZE`] (4096). A larger value trades a
    /// longer crash-replay window and more per-transaction memory for fewer
    /// commits; a smaller value yields more granular resume and lower memory at
    /// the cost of more commits. A value of `0` makes no progress, so it is
    /// floored to `1` at both the [`RedbConfig::with_reencrypt_batch_size`]
    /// setter and the read site.
    pub reencrypt_batch_size: usize,
}

impl Default for RedbConfig {
    fn default() -> Self {
        Self {
            compression: CompressionAlgorithm::Zstd,
            enable_checksums: true,
            cache_size_bytes: 0,
            reencrypt_batch_size: COLD_REENCRYPT_BATCH_SIZE,
        }
    }
}

impl RedbConfig {
    /// Create a new Redb configuration with default settings.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// let config = RedbConfig::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the compression algorithm for stored data.
    ///
    /// This allows overriding the default `Zstd` compression. Use `Fast` if you
    /// prioritize write speed over disk space, or `None` to disable entirely.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::{RedbConfig, CompressionAlgorithm};
    ///
    /// let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
    /// ```
    pub fn compression(mut self, compression: CompressionAlgorithm) -> Self {
        self.compression = compression;
        self
    }

    /// Enable or disable CRC32 checksums for data integrity.
    ///
    /// Redb has built-in checksums, but this adds an application-level layer
    /// of verification for compressed payloads. Enabled by default.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// // Disable checksums to squeeze out a tiny bit more performance
    /// let config = RedbConfig::new().enable_checksums(false);
    /// ```
    pub fn enable_checksums(mut self, enable: bool) -> Self {
        self.enable_checksums = enable;
        self
    }

    /// Set the Redb internal cache size in bytes.
    ///
    /// A larger cache improves read performance for frequently accessed historical data.
    /// Set to 0 to use Redb's default cache sizing.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// let config = RedbConfig::new().cache_size_bytes(1024 * 1024 * 64); // 64 MB
    /// ```
    pub fn cache_size_bytes(mut self, size: usize) -> Self {
        self.cache_size_bytes = size;
        self
    }

    /// Set the cold-tier rotation re-encrypt batch size: the number of
    /// `(key, value)` pairs re-encrypted per redb write transaction during a
    /// bulk key-rotation pass (Issue #3617 PR3).
    ///
    /// A larger value amortizes commit cost (fewer, larger transactions) at the
    /// price of longer write-transaction holds, more per-transaction memory, and
    /// a longer crash-replay window; a smaller value resumes at finer
    /// granularity and uses less memory but commits more often. `0` would make
    /// no forward progress, so it is floored to `1`.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// let config = RedbConfig::new().with_reencrypt_batch_size(1024);
    /// // A 0 is clamped up to a minimum of 1 so the pass always advances.
    /// let clamped = RedbConfig::new().with_reencrypt_batch_size(0);
    /// assert_eq!(clamped.reencrypt_batch_size, 1);
    /// ```
    pub fn with_reencrypt_batch_size(mut self, size: usize) -> Self {
        self.reencrypt_batch_size = size.max(1);
        self
    }

    /// Convert this `RedbConfig` into a standard `ColdStorageConfig`.
    ///
    /// This is an internal adapter to reuse the common compression logic.
    /// Since Redb handles ACID durability itself, `sync_writes` is always forced to `true`.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// let redb_config = RedbConfig::new();
    /// let cold_config = redb_config.to_cold_storage_config();
    /// assert_eq!(cold_config.sync_writes, true);
    /// ```
    pub fn to_cold_storage_config(&self) -> ColdStorageConfig {
        ColdStorageConfig {
            compression: self.compression,
            enable_checksums: self.enable_checksums,
            sync_writes: true, // Redb handles durability
            batch_size: 1000,
        }
    }
}

/// Redb-based cold storage implementation.
///
/// This struct provides a durable, disk-based storage backend for historical version data.
/// It uses Redb for ACID-compliant storage and supports compression (Zstd/LZ4) and
/// checksum validation.
///
/// # Key Features
///
/// - **Atomic Batches**: Store multiple node and edge versions atomically.
/// - **LSN Tracking**: Tracks the highest LSN flushed to disk to coordinate WAL truncation.
/// - **Compression**: Transparent compression of version data (Zstd by default).
/// - **Integrity**: Optional CRC32 checksums on top of Redb's guarantees.
pub struct RedbColdStorage {
    /// Path to the database file.
    path: PathBuf,
    /// Redb database instance.
    db: redb::Database,
    /// Configuration.
    config: RedbConfig,
    /// Statistics tracker.
    stats: AtomicColdStorageStats,
    /// Optional cold key ring for encrypting data at rest (Issue #3617 PR3),
    /// held in a runtime-swappable presence cell (Issue #3708).
    ///
    /// Empty (`None`) when encryption is disabled (values stored as bare
    /// compressed bytes, unchanged from the unencrypted path). When present it
    /// holds one generation in the steady state and two during a full-MEK key
    /// rotation, so a half-rotated store reads every value under its own
    /// generation via the `ACV1` wrapper dispatch.
    ///
    /// The cell is an [`ArcSwapOption`] so a keyring can be **installed at
    /// runtime** (`None` -> `Some`, Issue #3708) into a live plaintext store —
    /// the cold-tier mirror of the WAL's
    /// [`install_wal_keyring`](crate::storage::wal::concurrent_system::ConcurrentWalSystem::install_wal_keyring)
    /// seam (#3669) — without reopening the store. Every read path
    /// (`is_encrypted`, `encrypt_if_needed`, `decrypt_if_needed`,
    /// `prepare_batch`, ...) `load()`s it lock-free, so an install is observed
    /// atomically and never as a torn read; the install itself is serialized by
    /// [`install_lock`](Self::install_lock).
    keyring: ArcSwapOption<ColdKeyring>,
    /// Serializes runtime keyring installs (Issue #3708) so two concurrent
    /// installers cannot both observe the cell as `None`, both pass the presence
    /// check, and both store -- the second silently replacing the first keyring.
    /// Held for the entire [`install_cold_keyring`](Self::install_cold_keyring)
    /// body (presence check + store) so a second concurrent installer blocks,
    /// then observes `Some`, and returns the existing rejection `Err`. A private
    /// leaf taken only at the top of install; the hot read/write paths never
    /// touch it.
    install_lock: Mutex<()>,
    /// Fault injection flag for testing.
    #[cfg(test)]
    fail_writes: AtomicBool,
    #[cfg(test)]
    writes_attempted: AtomicBool,
}

impl RedbColdStorage {
    /// Create a new Redb cold storage at the given path.
    ///
    /// This initializes the database and creates the necessary internal tables
    /// (`node_versions`, `edge_versions`, `metadata`) if they do not exist.
    ///
    /// # Usage
    ///
    /// Use this to explicitly configure the cold storage backend. If you don't need
    /// custom configuration, use [`with_default_config`](Self::with_default_config).
    ///
    /// # Details
    ///
    /// If the parent directories do not exist, this method will attempt to create them.
    ///
    /// When opening an existing database, the `node_versions_stored` and
    /// `edge_versions_stored` statistics counters are seeded from the
    /// persisted table lengths (an O(1) metadata read), so
    /// [`stats`](Self::stats) reflects the full on-disk history rather than
    /// only writes made by the current process. Byte and read counters are
    /// not persisted and always start at zero.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("my_cold_data.redb");
    ///
    /// let config = RedbConfig::new();
    /// let storage = RedbColdStorage::new(&path, config)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new<P: AsRef<Path>>(path: P, config: RedbConfig) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(map_io_error("Failed to create directory"))?;
        }

        // Open or create the database
        let db =
            redb::Database::create(&path).map_err(map_db_error("Failed to open Redb database"))?;

        // Initialize tables by creating them if they don't exist
        let write_txn = db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        // Open tables to create them, and read their persisted entry counts so
        // the version counters can be seeded below. `Table::len()` is an O(1)
        // read of the B-tree root's cached length — no scan.
        let (persisted_node_versions, persisted_edge_versions) = {
            let node_table = write_txn
                .open_table(NODE_VERSIONS_TABLE)
                .map_err(map_table_error("Failed to create node_versions table"))?;
            let edge_table = write_txn
                .open_table(EDGE_VERSIONS_TABLE)
                .map_err(map_table_error("Failed to create edge_versions table"))?;
            write_txn
                .open_table(METADATA_TABLE)
                .map_err(map_table_error("Failed to create metadata table"))?;
            (
                node_table
                    .len()
                    .map_err(map_storage_error("Failed to read node_versions length"))?,
                edge_table
                    .len()
                    .map_err(map_storage_error("Failed to read edge_versions length"))?,
            )
        };

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit table creation"))?;

        // Seed the version counters from the persisted tables so a reopened
        // database reports its full cold history instead of misleading zeros
        // (Issue #3222). Byte counters (raw/compressed) are not recoverable
        // from table metadata, so `compression_ratio()` and the read counters
        // remain since-process-start.
        let stats = AtomicColdStorageStats::new();
        stats
            .node_versions_stored
            .store(persisted_node_versions, Ordering::Relaxed);
        stats
            .edge_versions_stored
            .store(persisted_edge_versions, Ordering::Relaxed);

        Ok(Self {
            path,
            db,
            config,
            stats,
            keyring: ArcSwapOption::empty(),
            install_lock: Mutex::new(()),
            #[cfg(test)]
            fail_writes: AtomicBool::new(false),
            #[cfg(test)]
            writes_attempted: AtomicBool::new(false),
        })
    }

    /// Create a new Redb cold storage using the default configuration.
    ///
    /// This is a convenience wrapper around [`new`](Self::new) for when you want
    /// standard Zstd compression and default cache sizes.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("default_cold.redb");
    ///
    /// let storage = RedbColdStorage::with_default_config(&path)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_default_config<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::new(path, RedbConfig::default())
    }

    /// Set the encryption cipher for at-rest encryption of stored data.
    ///
    /// This enables transparent encryption for all historical versions. The data is
    /// compressed *before* being encrypted to preserve compression effectiveness.
    /// Note that table structure and the `flushed_lsn` metadata are NOT encrypted.
    ///
    /// # Usage
    ///
    /// Use this as part of a builder pattern after constructing the storage instance.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::encryption::Aes256GcmCipher;
    /// # use zeroize::Zeroizing;
    /// # use std::sync::Arc;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("secure_cold.redb");
    ///
    /// // Normally you would load this key securely!
    /// let key = Zeroizing::new([0u8; 32]);
    /// let cipher = Arc::new(Aes256GcmCipher::new(&key));
    ///
    /// let storage = RedbColdStorage::with_default_config(&path)?
    ///     .with_cipher(cipher);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_cipher(self, cipher: Arc<dyn crate::encryption::cipher::Cipher>) -> Self {
        self.with_cold_keyring(ColdKeyring::single(cipher))
    }

    /// Set the encryption cipher pinned to an explicit `key_version` for at-rest
    /// encryption (Issue #3617 PR3 version-provisioning parity).
    ///
    /// Reads decrypt every value with this one cipher (`match_any`, identical to
    /// [`with_cipher`](Self::with_cipher)); only the write-stamp / reported cold
    /// key version is pinned to `key_version`. The durable `open()` path uses this
    /// to PROVISION the cold keyring at the same version index/WAL are provisioned
    /// to after a rotate-then-reopen, so freshly written cold values stamp the
    /// real version rather than a stale base.
    #[must_use]
    pub fn with_cipher_versioned(
        self,
        cipher: Arc<dyn crate::encryption::cipher::Cipher>,
        key_version: u32,
    ) -> Self {
        self.with_cold_keyring(ColdKeyring::single_versioned(cipher, key_version))
    }

    /// Set the full cold key ring for at-rest encryption (Issue #3617 PR3).
    ///
    /// Used by the rotation driver to install a keyring already holding both the
    /// old and new generations before a bulk re-encrypt pass. Most callers should
    /// use [`with_cipher`](Self::with_cipher).
    #[must_use]
    pub fn with_cold_keyring(mut self, keyring: ColdKeyring) -> Self {
        self.keyring = ArcSwapOption::from(Some(Arc::new(keyring)));
        self
    }

    /// Whether the cold store encrypts values at rest.
    pub fn is_encrypted(&self) -> bool {
        self.keyring.load().is_some()
    }

    /// The `key_version` freshly written cold values are stamped with, or `None`
    /// when encryption is disabled (Issue #3617 PR3).
    pub fn current_cold_key_version(&self) -> Option<u32> {
        self.keyring
            .load()
            .as_deref()
            .map(ColdKeyring::current_version)
    }

    /// Install a cold DEK keyring at runtime, flipping a live **plaintext** cold
    /// store to **encrypted** (`None` -> `Some`, Issue #3708). This is the
    /// cold-tier mirror of the WAL's
    /// [`install_wal_keyring`](crate::storage::wal::concurrent_system::ConcurrentWalSystem::install_wal_keyring)
    /// seam (#3669): the structural primitive a hot-live `encryption enable`
    /// driver installs so the cold tier is re-keyed without reopening the store.
    ///
    /// # Transition
    ///
    /// Only the `None` -> `Some` transition is supported. Installing when a
    /// keyring is already present is **rejected** with a structured error (never
    /// a silent replace, so no generation is lost): rotation (`Some` -> `Some'`)
    /// is the job of [`install_cold_generation`](Self::install_cold_generation),
    /// not this seam.
    ///
    /// # Concurrency
    ///
    /// The whole body -- presence check and store -- runs under a dedicated leaf
    /// [`install_lock`](Self::install_lock), so two concurrent installers cannot
    /// both observe the cell as `None` and both store (the second silently
    /// replacing the first): the loser blocks, then observes `Some`, and takes
    /// the rejection path. Readers `load()` the cell lock-free, so an install is
    /// observed atomically and never as a torn read.
    ///
    /// # Data at rest
    ///
    /// Unlike the WAL, no segment seal/reopen is required: cold values are
    /// individually self-describing (`ACV1`), so values written **after** the
    /// install are wrapped under the new keyring while values written before it
    /// stay bare. Wrapping the pre-install plaintext corpus (via
    /// [`wrap_plaintext_cold_values`](Self::wrap_plaintext_cold_values)) is the
    /// enable **driver's** responsibility, exactly as the WAL seam leaves its
    /// downstream migration to its driver -- installing the keyring alone does
    /// NOT retroactively encrypt already-stored values.
    ///
    /// # Errors
    ///
    /// Returns an error if a keyring is already installed (a double-install).
    // The production consumer is the hot-live plaintext -> encrypted enable
    // engine (Issue #3708 follow-up), which drives this from `enable_encryption`
    // after the WAL's `install_wal_keyring` and a bare -> `ACV1` wrap pass. A
    // dedicated `StorageError` variant for the double-install rejection (so the
    // enable engine can map ONLY it to `FAILED_PRECONDITION`, as the WAL seam's
    // `WalKeyringAlreadyInstalled` allows) is a trivial follow-up for the
    // enable lane; reusing `InconsistentState` here keeps the change within the
    // cold-storage module.
    pub fn install_cold_keyring(&self, keyring: ColdKeyring) -> Result<()> {
        // Serialize the ENTIRE install -- presence check + store -- under a
        // dedicated leaf mutex. Without it two concurrent installers could both
        // observe `None`, both pass the check, and both store, the second
        // silently replacing the first keyring.
        let _install_guard = self.install_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Reject a double-install: presence is a one-way None -> Some transition.
        if self.keyring.load().is_some() {
            return Err(StorageError::InconsistentState {
                reason: "a cold keyring is already installed; runtime install only \
                         supports the plaintext -> encrypted (None -> Some) transition \
                         (key rotation is install_cold_generation)"
                    .to_string(),
            }
            .into());
        }

        self.keyring.store(Some(Arc::new(keyring)));
        Ok(())
    }

    /// Install a new cold DEK generation for a key rotation (Issue #3617 PR3),
    /// keeping the existing generation(s) live so a half-rotated store still reads
    /// old-generation values. Idempotent for an already-present version. Makes the
    /// new generation current, so subsequent writes stamp it.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is not encrypted (no keyring to extend).
    pub fn install_cold_generation(
        &self,
        key_version: u32,
        cipher: Arc<dyn crate::encryption::cipher::Cipher>,
    ) -> Result<()> {
        let guard = self.keyring.load();
        let ring = guard.as_deref().ok_or_else(|| {
            Into::<crate::core::error::Error>::into(StorageError::InconsistentState {
                reason: "cold storage is not encrypted; cannot install a key generation"
                    .to_string(),
            })
        })?;
        ring.add_generation(key_version, cipher);
        Ok(())
    }

    /// Retire every cold DEK generation except `key_version`, which becomes the
    /// sole current generation (Issue #3617 PR3). Called after a verified full
    /// re-encrypt pass so the old cold DEK can be dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is not encrypted.
    pub fn retire_cold_generations(&self, key_version: u32) -> Result<()> {
        let guard = self.keyring.load();
        let ring = guard.as_deref().ok_or_else(|| {
            Into::<crate::core::error::Error>::into(StorageError::InconsistentState {
                reason: "cold storage is not encrypted; cannot retire key generations".to_string(),
            })
        })?;
        ring.retain_only(key_version);
        Ok(())
    }

    /// Encrypt a compressed value for storage, applying the self-describing
    /// `ACV1` wrapper when encryption is configured (Issue #3617 PR3).
    ///
    /// The value is stamped with the cold keyring's CURRENT `key_version` so a
    /// half-rotated store distinguishes old- from new-generation values on read.
    /// A no-op (returns the input) when encryption is disabled.
    fn encrypt_if_needed(&self, data: Vec<u8>) -> Result<Vec<u8>> {
        let guard = self.keyring.load();
        match guard.as_deref() {
            Some(ring) => {
                let (cipher, key_version) = ring.current().ok_or_else(|| {
                    Into::<crate::core::error::Error>::into(StorageError::Encryption(
                        "Cold storage keyring holds no generation".to_string(),
                    ))
                })?;
                let ciphertext =
                    cipher
                        .encrypt(&data, &[])
                        .map_err(|e| -> crate::core::error::Error {
                            StorageError::Encryption(format!("Cold storage encryption failed: {e}"))
                                .into()
                        })?;
                Ok(wrap_cold_value(key_version, &ciphertext))
            }
            None => Ok(data),
        }
    }

    /// Decrypt a stored value, transparently handling both `ACV1`-wrapped
    /// (post-rotation) and legacy bare-ciphertext values (Issue #3617 PR3).
    ///
    /// Dispatch: a value is treated as wrapped ONLY IF it begins with the `ACV1`
    /// magic AND names a `key_version` the keyring holds; otherwise it is a legacy
    /// value decrypted under the oldest (pre-rotation) cold generation. See
    /// [`keyring`](crate::storage::redb_cold_storage::keyring) for the collision
    /// argument. A no-op (returns a copy) when encryption is disabled.
    fn decrypt_if_needed(&self, data: &[u8]) -> Result<Vec<u8>> {
        let guard = self.keyring.load();
        let Some(ring) = guard.as_deref() else {
            return Ok(data.to_vec());
        };
        // Wrapped iff ACV1 magic AND the stamped version names a held generation.
        if let Some((key_version, ciphertext)) = parse_cold_wrapper(data)
            && ring.has_version(key_version)
            && let Some(cipher) = ring.cipher_for_value(Some(key_version))
        {
            return cipher
                .decrypt(ciphertext, &[])
                .map_err(|e| -> crate::core::error::Error {
                    StorageError::Encryption(format!("Cold storage decryption failed: {e}")).into()
                });
        }
        // Legacy bare ciphertext → oldest (pre-rotation) / sole generation.
        let cipher = ring.cipher_for_value(None).ok_or_else(|| {
            Into::<crate::core::error::Error>::into(StorageError::Encryption(
                "Cold storage keyring holds no generation".to_string(),
            ))
        })?;
        cipher
            .decrypt(data, &[])
            .map_err(|e| -> crate::core::error::Error {
                StorageError::Encryption(format!("Cold storage decryption failed: {e}")).into()
            })
    }

    /// Get the absolute or relative path to the Redb database file.
    ///
    /// This is useful for logging or debugging storage locations.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use std::path::Path;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("my_db.redb");
    /// let storage = RedbColdStorage::with_default_config(&path)?;
    ///
    /// assert_eq!(storage.path(), path.as_path());
    /// # Ok(())
    /// # }
    /// ```
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Compress data using the configured algorithm.
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        crate::storage::compression::compress(data, &self.config.to_cold_storage_config())
    }

    /// Decompress data using the configured algorithm.
    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        crate::storage::compression::decompress(data, &self.config.to_cold_storage_config())
    }

    #[inline]
    fn should_parallel_compress(&self, batch_len: usize) -> bool {
        matches!(self.config.compression, CompressionAlgorithm::Zstd)
            && batch_len >= PARALLEL_COMPRESSION_THRESHOLD
    }

    fn prepare_batch<V, EncodeFn>(
        &self,
        versions: &[V],
        encode_version: EncodeFn,
    ) -> Result<PreparedVersionBatch>
    where
        V: EntityVersion + Sync,
        EncodeFn: Fn(&V) -> Vec<u8> + Sync + Send,
    {
        let cold_config = self.config.to_cold_storage_config();
        // Resolve the current cold generation ONCE up front so the parallel
        // compression closure is `Sync` (no borrow of `self.keyring`'s lock) and
        // every value in the batch is wrapped under the same `key_version`.
        let keyring_guard = self.keyring.load();
        let cold_generation: Option<(Arc<dyn crate::encryption::cipher::Cipher>, u32)> =
            match keyring_guard.as_deref() {
                Some(ring) => Some(ring.current().ok_or_else(|| {
                    Into::<crate::core::error::Error>::into(StorageError::Encryption(
                        "Cold storage keyring holds no generation".to_string(),
                    ))
                })?),
                None => None,
            };
        let cold_generation_ref = &cold_generation;

        // Helper closure: compress then optionally encrypt+`ACV1`-wrap.
        let compress_and_encrypt = |data: &[u8]| -> Result<Vec<u8>> {
            let compressed = crate::storage::compression::compress(data, &cold_config)?;
            match cold_generation_ref {
                Some((cipher, key_version)) => {
                    let ciphertext = cipher.encrypt(&compressed, &[]).map_err(
                        |e| -> crate::core::error::Error {
                            StorageError::Encryption(format!("Cold storage encryption failed: {e}"))
                                .into()
                        },
                    )?;
                    Ok(wrap_cold_value(*key_version, &ciphertext))
                }
                None => Ok(compressed),
            }
        };

        if self.should_parallel_compress(versions.len()) {
            versions
                .par_iter()
                .try_fold(PreparedVersionBatch::default, |mut prepared, version| {
                    let encoded = encode_version(version);
                    let raw_size_bytes = encoded.len() as u64;
                    let to_store = compress_and_encrypt(&encoded)?;
                    prepared.add_entry(version.version_id(), to_store, raw_size_bytes);
                    Ok::<_, crate::core::error::Error>(prepared)
                })
                .try_reduce(PreparedVersionBatch::default, |mut left, right| {
                    left.merge(right);
                    Ok::<_, crate::core::error::Error>(left)
                })
        } else {
            let mut prepared = PreparedVersionBatch::with_capacity(versions.len());
            for version in versions {
                let encoded = encode_version(version);
                let raw_size_bytes = encoded.len() as u64;
                let to_store = compress_and_encrypt(&encoded)?;
                prepared.add_entry(version.version_id(), to_store, raw_size_bytes);
            }

            Ok(prepared)
        }
    }

    fn prepare_node_versions_batch(
        &self,
        versions: &[NodeVersion],
    ) -> Result<PreparedVersionBatch> {
        self.prepare_batch(versions, encode_node_version)
    }

    fn prepare_edge_versions_batch(
        &self,
        versions: &[EdgeVersion],
    ) -> Result<PreparedVersionBatch> {
        self.prepare_batch(versions, encode_edge_version)
    }

    /// Set the fault injection flag for write operations.
    ///
    /// When set to `true`, the next write operation (like `store_node_version`) will
    /// immediately return an IO error. This is exclusively used to test database
    /// recovery and failure handling.
    ///
    /// #[doc(hidden)]
    #[cfg(test)]
    pub fn set_fail_writes(&self, fail: bool) {
        self.fail_writes.store(fail, Ordering::SeqCst);
    }

    /// Check if a write operation was attempted while fault injection was active.
    ///
    /// This allows tests to verify that the code under test actually tried to
    /// write to the database during the failure scenario.
    ///
    /// #[doc(hidden)]
    #[cfg(test)]
    pub fn was_write_attempted(&self) -> bool {
        self.writes_attempted.load(Ordering::SeqCst)
    }

    /// Helper to check failure injection
    fn check_fail_writes(&self) -> Result<()> {
        #[cfg(test)]
        {
            self.writes_attempted.store(true, Ordering::SeqCst);
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(StorageError::io_error("Simulated write failure").into());
            }
        }
        Ok(())
    }

    /// Get the Log Sequence Number (LSN) of the last safely flushed transaction.
    ///
    /// The Write-Ahead Log uses this value to determine which segments can be
    /// safely truncated and deleted. If a transaction's LSN is less than or equal
    /// to this `flushed_lsn`, it is durably persisted in cold storage.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("db.redb");
    /// let storage = RedbColdStorage::with_default_config(&path)?;
    ///
    /// if let Some(lsn) = storage.get_flushed_lsn()? {
    ///     println!("Safe to truncate WAL up to LSN: {:?}", lsn);
    /// } else {
    ///     println!("No data flushed yet.");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_flushed_lsn(&self) -> Result<Option<LSN>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;
        let table = read_txn
            .open_table(METADATA_TABLE)
            .map_err(map_table_error("Failed to open metadata table"))?;

        match table.get(FLUSHED_LSN_KEY) {
            Ok(Some(value)) => {
                let bytes: &[u8] = value.value();
                if bytes.len() != 8 {
                    return Err(
                        StorageError::corruption("Invalid flushed_lsn format".to_string()).into(),
                    );
                }
                let lsn_value = u64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                Ok(Some(LSN(lsn_value)))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                Err(StorageError::io_error(format!("Failed to read flushed_lsn: {}", e)).into())
            }
        }
    }

    /// Set the flushed LSN in the metadata table (internal helper).
    fn set_flushed_lsn_internal(
        table: &mut redb::Table<'_, &'static str, &'static [u8]>,
        lsn: LSN,
    ) -> Result<()> {
        // Read current LSN to ensure we only increase it
        let current_lsn = if let Ok(Some(value)) = table.get(FLUSHED_LSN_KEY) {
            let bytes = value.value();
            if bytes.len() == 8 {
                let lsn_value = u64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                Some(LSN(lsn_value))
            } else {
                None
            }
        } else {
            None
        };

        // Only update if new LSN is higher (prevents race condition)
        let final_lsn = match current_lsn {
            Some(current) if lsn.0 <= current.0 => {
                // LSN is not higher, skip update
                return Ok(());
            }
            _ => lsn,
        };

        let lsn_bytes = final_lsn.0.to_le_bytes();
        table
            .insert(FLUSHED_LSN_KEY, lsn_bytes.as_slice())
            .map_err(|e| -> crate::core::error::Error {
                StorageError::io_error(format!("Failed to write flushed_lsn: {}", e)).into()
            })?;
        Ok(())
    }

    /// Read the persisted cold-tier bi-temporal extent bounds (Issue #3389).
    ///
    /// Returns `Ok(None)` when the cold tier has never stored a version (no
    /// extent record yet) or when the persisted record uses an unrecognized
    /// layout version — both mean "no cold bounds to contribute", so callers
    /// leave the hot-tier extent untouched. Otherwise returns the bounds
    /// accumulated across every version ever written to this cold store,
    /// following the same `earliest`/`latest` convention as the in-memory
    /// extent aggregate (the open-interval sentinel never appears).
    ///
    /// This is an O(1) single metadata read; it is the startup counterpart to
    /// the incremental bounds maintained on every cold write, letting
    /// [`temporal_extent`](crate::AletheiaDB::temporal_extent) span history
    /// migrated to cold before the current process started.
    pub fn get_temporal_extent_bounds(&self) -> Result<Option<ColdTemporalExtentBounds>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;
        let table = match read_txn.open_table(METADATA_TABLE) {
            Ok(table) => table,
            // A cold store that never persisted metadata has no such table.
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => {
                return Err(StorageError::io_error(format!(
                    "Failed to open metadata table: {}",
                    e
                ))
                .into());
            }
        };

        match table.get(TEMPORAL_EXTENT_KEY) {
            Ok(Some(value)) => Ok(ColdTemporalExtentBounds::from_bytes(value.value())),
            Ok(None) => Ok(None),
            Err(e) => {
                Err(StorageError::io_error(format!("Failed to read temporal_extent: {}", e)).into())
            }
        }
    }

    /// Merge a batch of freshly-written versions' bounds into the persisted
    /// cold-tier extent, within the caller's write transaction (Issue #3389).
    ///
    /// A no-op when the delta is empty (nothing observed). The read-modify-write
    /// happens on the same `metadata` table the caller already opens, so the
    /// extent update is atomic with the version writes: either the versions and
    /// their widened bounds both commit, or neither does.
    fn merge_extent_into_metadata(
        table: &mut redb::Table<'_, &'static str, &'static [u8]>,
        delta: &ColdTemporalExtentBounds,
    ) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let mut current = match table.get(TEMPORAL_EXTENT_KEY) {
            Ok(Some(value)) => {
                // An existing record with an UNRECOGNIZED version tag decodes to
                // `None` and is clobbered here: `unwrap_or_default()` starts from
                // empty bounds and rebuilds them widen-only from post-write
                // deltas. That makes a downgrade across a future
                // `EXTENT_RECORD_VERSION` bump lossy (bounds a newer binary
                // persisted are dropped, then re-accumulated only from writes the
                // older binary performs) — acceptable because the extent is
                // advisory (under-reporting is safe) and only ever widens.
                ColdTemporalExtentBounds::from_bytes(value.value()).unwrap_or_default()
            }
            Ok(None) => ColdTemporalExtentBounds::default(),
            Err(e) => {
                return Err(StorageError::io_error(format!(
                    "Failed to read temporal_extent: {}",
                    e
                ))
                .into());
            }
        };
        current.merge(delta);
        table
            .insert(TEMPORAL_EXTENT_KEY, current.to_bytes().as_slice())
            .map_err(|e| -> crate::core::error::Error {
                StorageError::io_error(format!("Failed to write temporal_extent: {}", e)).into()
            })?;
        Ok(())
    }

    // ========================================================================
    // Cold-tier key rotation: transactional bulk re-encrypt (Issue #3617 PR3)
    // ========================================================================

    /// Read the durable cold-rotation resume cursor, if a pass is in flight.
    ///
    /// `Ok(None)` when no cursor is present or its record is unrecognized (an
    /// older binary falls back to a full pass). O(1) metadata read.
    fn read_cold_rotation_cursor(&self) -> Result<Option<ColdRotationCursor>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;
        let table = match read_txn.open_table(METADATA_TABLE) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => {
                return Err(
                    StorageError::io_error(format!("Failed to open metadata table: {e}")).into(),
                );
            }
        };
        match table.get(COLD_ROTATION_CURSOR_KEY) {
            Ok(Some(value)) => Ok(ColdRotationCursor::from_bytes(value.value())),
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::io_error(format!(
                "Failed to read cold_rotation cursor: {e}"
            ))
            .into()),
        }
    }

    /// The target `key_version` every cold value has been re-wrapped to, per the
    /// durable [`COLD_VALUE_FORMAT_KEY`] marker (Issue #3617 PR3). `Ok(None)` when
    /// the store has never completed a rotation (values may be legacy or `ACV1`
    /// at the base version). Used by tests and the resume driver to confirm a
    /// finished pass without re-scanning every value.
    pub fn cold_value_format_version(&self) -> Result<Option<u32>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;
        let table = match read_txn.open_table(METADATA_TABLE) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => {
                return Err(
                    StorageError::io_error(format!("Failed to open metadata table: {e}")).into(),
                );
            }
        };
        match table.get(COLD_VALUE_FORMAT_KEY) {
            Ok(Some(value)) => {
                let bytes: &[u8] = value.value();
                if bytes.len() == 5 && bytes[0] == COLD_ROTATION_RECORD_VERSION {
                    Ok(Some(u32::from_le_bytes([
                        bytes[1], bytes[2], bytes[3], bytes[4],
                    ])))
                } else {
                    Ok(None)
                }
            }
            Ok(None) => Ok(None),
            Err(e) => {
                Err(StorageError::io_error(format!("Failed to read cold_value_format: {e}")).into())
            }
        }
    }

    /// Transactionally re-encrypt every stored cold value to the `target_version`
    /// generation (Issue #3617 PR3), decrypting each under its own (old/legacy)
    /// generation and re-encrypting under the target cold DEK, in bounded
    /// per-transaction batches, resumable via a durable redb cursor.
    ///
    /// # Preconditions
    ///
    /// The store must be encrypted and its keyring must hold BOTH the target
    /// generation (installed by the driver) and every source generation needed to
    /// decrypt existing values. New appends interleaved with the pass already
    /// stamp the current (target) generation and are simply skipped as
    /// already-wrapped.
    ///
    /// # Crash-consistency
    ///
    /// Each batch's value rewrites and the cursor advance commit in ONE redb write
    /// transaction, so the cursor never runs ahead of (or behind) the durable
    /// rewrites — a crash resumes from exactly the last committed key. The `ACV1`
    /// wrapper is the idempotency backstop: a value already wrapped at
    /// `target_version` is skipped, so a re-run after a stale cursor never
    /// double-encrypts. Only `node_versions`/`edge_versions` VALUES are touched;
    /// `flushed_lsn` and `temporal_extent` (incl. the #3389 per-dimension bounds)
    /// are never read or rewritten, so they are preserved untouched.
    ///
    /// On completion the durable [`COLD_VALUE_FORMAT_KEY`] marker is set to
    /// `wrapped@target_version` and the resume cursor is deleted.
    ///
    /// # Errors
    ///
    /// * encryption is not configured, or the keyring does not hold
    ///   `target_version`;
    /// * a value fails to decrypt (a genuine wrong/absent key — surfaced loudly,
    ///   never silent wrong data) or a redb transaction fails.
    pub fn reencrypt_cold_values(&self, target_version: u32) -> Result<ColdReencryptStats> {
        let guard = self.keyring.load();
        let ring = guard.as_deref().ok_or_else(|| {
            Into::<crate::core::error::Error>::into(StorageError::InconsistentState {
                reason: "cold storage is not encrypted; cannot rotate cold keys".to_string(),
            })
        })?;
        if !ring.has_version(target_version) {
            return Err(StorageError::InconsistentState {
                reason: "cold keyring does not hold the target generation for rotation".to_string(),
            }
            .into());
        }
        // The cipher every value is re-encrypted UNDER (the target generation).
        let target_cipher = ring.cipher_for_value(Some(target_version)).ok_or_else(|| {
            Into::<crate::core::error::Error>::into(StorageError::InconsistentState {
                reason: "cold keyring target generation missing a cipher".to_string(),
            })
        })?;

        self.migrate_cold_values(target_version, &target_cipher, ColdSourceMode::Rekey)
    }

    /// Wrap every **bare (plaintext, never-encrypted)** cold value into the `ACV1`
    /// format under `target_cipher`, stamping `target_version` (Issue #3616 PR3 —
    /// the plaintext → encrypted *enable* migration).
    ///
    /// This is the cold counterpart the enable engine needs but
    /// [`reencrypt_cold_values`](Self::reencrypt_cold_values) cannot provide: that
    /// pass *decrypts* each source value first (it rekeys an already-encrypted
    /// store old-gen → new-gen), and the cold reader treats a bare value as legacy
    /// **ciphertext** — so feeding a bare plaintext value to the rekey pass would
    /// fail AEAD authentication. This pass instead treats each un-`ACV1` value as
    /// the plaintext to encrypt, so a plaintext cold store becomes a uniformly
    /// `ACV1`-wrapped one whose values the cold reader dispatches correctly (never
    /// mis-reading a bare value as legacy ciphertext once wrapped).
    ///
    /// The cipher is passed explicitly rather than sourced from the keyring, so the
    /// pass runs on a store opened with **no** keyring (the live plaintext handle
    /// during `enable`) as well as one opened under the enable cold DEK (a resuming
    /// `open()`). Only `node_versions`/`edge_versions` VALUES are rewritten;
    /// `flushed_lsn` / `temporal_extent` are never touched.
    ///
    /// **Idempotent / resumable / crash-safe** exactly like
    /// [`reencrypt_cold_values`](Self::reencrypt_cold_values): a value already
    /// wrapped at `target_version` is skipped, and each batch's rewrites + cursor
    /// advance commit in one redb transaction, so a crash mid-pass resumes with no
    /// double-encrypt. On completion the [`COLD_VALUE_FORMAT_KEY`] marker is set to
    /// `wrapped@target_version` and the cursor is cleared.
    ///
    /// # Errors
    ///
    /// A value fails to encrypt, or a redb transaction fails.
    pub fn wrap_plaintext_cold_values(
        &self,
        target_cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
        target_version: u32,
    ) -> Result<ColdReencryptStats> {
        self.migrate_cold_values(target_version, target_cipher, ColdSourceMode::WrapPlaintext)
    }

    /// Unwrap every **`ACV1`-wrapped** cold value back to bare plaintext (the
    /// exact inverse of [`wrap_plaintext_cold_values`]; Issue #3616 PR4 disable).
    ///
    /// Each `ACV1` value is decrypted under its stamped generation (via the live
    /// keyring, exactly as a normal read does) and rewritten BARE — the same
    /// compressed bytes a never-encrypted value carries — so a uniformly-encrypted
    /// cold store becomes a plaintext one the cold reader dispatches as bare. This
    /// is the cold counterpart the disable engine needs: the enable engine wrapped
    /// bare → `ACV1`, so a disable takes each `ACV1` value and produces the
    /// compressed plaintext it wraps. No record is ever decoded (the same
    /// compressed bytes are unwrapped in place), so temporal bounds cannot change;
    /// only `node_versions`/`edge_versions` VALUES are touched, never `flushed_lsn`
    /// / `temporal_extent`. On completion the durable [`COLD_VALUE_FORMAT_KEY`]
    /// marker is cleared (the store is plaintext, `cold_value_format_version`
    /// reports `None`).
    ///
    /// **Idempotent / resumable / crash-safe** WITHOUT a version cursor: each
    /// batch's rewrites commit in one redb transaction, and a re-run after a crash
    /// skips the already-bare values a prior partial run produced (a bare value is
    /// its own completion marker — the mirror of the wrap pass's already-wrapped
    /// skip). A value carrying no `ACV1` wrapper is treated as already-bare
    /// plaintext (the post-enable invariant this pass inverts; the enable wrap
    /// produces uniformly `ACV1`-wrapped values, so no pre-#3617 legacy
    /// bare-ciphertext value is in scope).
    ///
    /// # Errors
    ///
    /// * the store is not encrypted (no keyring to decrypt with);
    /// * a value fails to decrypt (a genuine wrong/absent key — surfaced loudly,
    ///   never silently left encrypted) or a redb transaction fails.
    pub fn unwrap_encrypted_cold_values(&self) -> Result<ColdUnwrapStats> {
        if self.keyring.load().is_none() {
            return Err(StorageError::InconsistentState {
                reason: "cold storage is not encrypted; nothing to unwrap".to_string(),
            }
            .into());
        }

        let mut stats = ColdUnwrapStats::default();
        for table in [ColdTableKind::Node, ColdTableKind::Edge] {
            self.unwrap_one_table(table, &mut stats)?;
        }

        // Terminal marker: the whole store is now plaintext; drop the format
        // marker (so `cold_value_format_version` reports `None`) and any stale
        // rotation cursor, in one transaction.
        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;
        {
            let mut meta = write_txn
                .open_table(METADATA_TABLE)
                .map_err(map_table_error("Failed to open metadata table"))?;
            meta.remove(COLD_VALUE_FORMAT_KEY)
                .map_err(map_storage_error("Failed to clear cold_value_format"))?;
            meta.remove(COLD_ROTATION_CURSOR_KEY)
                .map_err(map_storage_error("Failed to clear cold_rotation cursor"))?;
        }
        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit cold unwrap completion"))?;

        Ok(stats)
    }

    /// Unwrap one value-bearing table's `ACV1` values back to bare, in bounded
    /// per-transaction batches (Issue #3616 PR4 disable). Mirrors
    /// [`reencrypt_one_table`](Self::reencrypt_one_table) but writes the decrypted
    /// compressed bytes BARE (no wrapper) instead of re-wrapping, and relies on
    /// the natural "bare == done" idempotency rather than a version cursor.
    fn unwrap_one_table(
        &self,
        table_kind: ColdTableKind,
        stats: &mut ColdUnwrapStats,
    ) -> Result<()> {
        use std::ops::Bound;
        let table_def = table_kind.table();
        let batch_size = self.config.reencrypt_batch_size.max(1);
        let mut resume_after: Option<u64> = None;
        loop {
            let write_txn = self
                .db
                .begin_write()
                .map_err(map_transaction_error("Failed to begin write transaction"))?;

            let batch_len;
            let mut last_key = resume_after;
            {
                let mut table = write_txn
                    .open_table(table_def)
                    .map_err(map_table_error("Failed to open versions table"))?;

                // Collect a bounded slice into owned buffers FIRST — the redb
                // iterator borrows the table immutably and cannot be held across
                // the `insert` calls below (which need `&mut table`).
                let collected: Vec<(u64, Vec<u8>)> = {
                    let lower = match resume_after {
                        Some(k) => Bound::Excluded(k),
                        None => Bound::Unbounded,
                    };
                    let range = table
                        .range::<u64>((lower, Bound::Unbounded))
                        .map_err(map_storage_error("Failed to range cold table"))?;
                    let mut v = Vec::with_capacity(batch_size);
                    for entry in range.take(batch_size) {
                        let (k, val) =
                            entry.map_err(map_storage_error("Failed to read cold entry"))?;
                        v.push((k.value(), val.value().to_vec()));
                    }
                    v
                };

                batch_len = collected.len();
                if batch_len == 0 {
                    drop(table);
                    drop(write_txn);
                    break;
                }

                for (key, value) in &collected {
                    last_key = Some(*key);
                    // Idempotency backstop: a value with no `ACV1` wrapper is
                    // already bare plaintext → skip (a prior partial run, or the
                    // post-enable invariant this pass inverts).
                    //
                    // FAIL-CLOSED INVARIANT (Issue #3616 PR4 review): treating a
                    // no-wrapper value as bare plaintext is only correct because
                    // this pass runs EXCLUSIVELY over an ENABLE-engine-created
                    // store, where every encrypted cold value carries an `ACV1`
                    // wrapper. That invariant is enforced upstream by construction,
                    // not merely assumed here:
                    //   * the normal encrypted write path (`encrypt_if_needed`)
                    //     ALWAYS `ACV1`-wraps (`wrap_cold_value`) when a keyring is
                    //     present, and
                    //   * `enable`'s `wrap_plaintext_cold_values` converts every
                    //     pre-existing bare value bare → `ACV1` and unconditionally
                    //     stamps the terminal `COLD_VALUE_FORMAT_KEY` marker (even
                    //     for an empty store).
                    // So a disable-able store's encrypted cold values are uniformly
                    // `ACV1`-wrapped, and the ONLY no-wrapper values reaching here
                    // are genuinely bare (a completed prior batch/run). LEGACY
                    // pre-#3617 bare-ciphertext (encrypted, no `ACV1` wrapper), which
                    // `decrypt_if_needed` still supports on the read path, is OUT OF
                    // SCOPE: it predates both the `ACV1` scheme and the durable
                    // encryption authority the disable engine requires, so it never
                    // reaches this pass.
                    //
                    // A marker-based fail-closed guard was evaluated and deliberately
                    // NOT added: the `COLD_VALUE_FORMAT_KEY` marker cannot represent
                    // the legacy-encrypted state distinctly — it is ABSENT for a
                    // legacy store, a never-encrypted plaintext store, AND a store
                    // whose disable already cleared it and is being re-driven by the
                    // post-clear resume window (a crash between
                    // `unwrap_encrypted_cold_values` clearing the marker and
                    // `mark_cold_complete` legitimately re-runs this pass over an
                    // all-bare, marker-absent, keyring-present store). Refusing on
                    // "marker absent" would therefore break that legitimate
                    // idempotent resume rather than catch a real defect, and refusing
                    // for enable-engine stores is impossible (their marker is always
                    // set). The safe guard is the upstream construction invariant
                    // above, so this skip stays.
                    if parse_cold_wrapper(value).is_none() {
                        stats.values_skipped += 1;
                        continue;
                    }
                    // Decrypt the wrapped value under its stamped generation,
                    // yielding the compressed plaintext bytes a bare value holds.
                    let bare = self.decrypt_if_needed(value)?;
                    table
                        .insert(*key, bare.as_slice())
                        .map_err(map_storage_error("Failed to rewrite cold value"))?;
                    stats.values_unwrapped += 1;
                }
            }

            write_txn
                .commit()
                .map_err(map_commit_error("Failed to commit cold unwrap batch"))?;
            stats.batches_committed += 1;

            resume_after = last_key;
            if batch_len < batch_size {
                break;
            }
        }
        Ok(())
    }

    /// Shared driver for the cold value-migration passes (Issue #3617 PR3 rekey /
    /// Issue #3616 PR3 enable-wrap): resume from the durable cursor, process the
    /// Node then Edge value tables in bounded batches, and write the terminal
    /// format marker. The `source_mode` selects how each source value's plaintext
    /// is obtained (decrypt-under-own-generation vs treat-bare-as-plaintext).
    fn migrate_cold_values(
        &self,
        target_version: u32,
        target_cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
        source_mode: ColdSourceMode,
    ) -> Result<ColdReencryptStats> {
        let mut stats = ColdReencryptStats::default();

        // Resume point: a cursor for THIS target resumes it; any other cursor
        // (stale, from an aborted earlier rotation) is ignored and the pass
        // restarts from the node table (idempotent — already-wrapped values are
        // skipped). Tables are always processed Node then Edge.
        let (start_table, start_after) = match self.read_cold_rotation_cursor()? {
            Some(c) if c.target_version == target_version => (c.table, Some(c.last_completed_key)),
            _ => (ColdTableKind::Node, None),
        };

        let tables = [ColdTableKind::Node, ColdTableKind::Edge];
        let start_idx = tables.iter().position(|t| *t == start_table).unwrap_or(0);
        for (i, &table) in tables.iter().enumerate() {
            if i < start_idx {
                // A table before the cursor's table is already fully processed.
                continue;
            }
            let resume_after = if i == start_idx { start_after } else { None };
            self.reencrypt_one_table(
                table,
                target_version,
                target_cipher,
                source_mode,
                resume_after,
                &mut stats,
            )?;
        }

        // Terminal marker: whole store is wrapped at the target; drop the cursor.
        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;
        {
            let mut meta = write_txn
                .open_table(METADATA_TABLE)
                .map_err(map_table_error("Failed to open metadata table"))?;
            let mut format_bytes = [0u8; 5];
            format_bytes[0] = COLD_ROTATION_RECORD_VERSION;
            format_bytes[1..5].copy_from_slice(&target_version.to_le_bytes());
            meta.insert(COLD_VALUE_FORMAT_KEY, format_bytes.as_slice())
                .map_err(map_storage_error("Failed to write cold_value_format"))?;
            meta.remove(COLD_ROTATION_CURSOR_KEY)
                .map_err(map_storage_error("Failed to clear cold_rotation cursor"))?;
        }
        write_txn.commit().map_err(map_commit_error(
            "Failed to commit cold rotation completion",
        ))?;

        Ok(stats)
    }

    /// Re-encrypt one value-bearing table in bounded, cursor-advancing batches.
    fn reencrypt_one_table(
        &self,
        table_kind: ColdTableKind,
        target_version: u32,
        target_cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
        source_mode: ColdSourceMode,
        mut resume_after: Option<u64>,
        stats: &mut ColdReencryptStats,
    ) -> Result<()> {
        use std::ops::Bound;
        let table_def = table_kind.table();
        // Configurable per-transaction batch size (Issue #3617 PR3). Floored to
        // 1 so a misconfigured 0 (e.g. via a direct/serde-deserialized field
        // write that bypasses `with_reencrypt_batch_size`) still makes progress.
        let batch_size = self.config.reencrypt_batch_size.max(1);
        loop {
            let write_txn = self
                .db
                .begin_write()
                .map_err(map_transaction_error("Failed to begin write transaction"))?;

            let mut last_key = resume_after;
            let batch_len;
            {
                let mut table = write_txn
                    .open_table(table_def)
                    .map_err(map_table_error("Failed to open versions table"))?;

                // Collect a bounded slice of (key, value) into owned buffers FIRST
                // — a redb iterator borrows the table immutably and cannot be held
                // across the `insert` calls below (which need `&mut table`).
                let collected: Vec<(u64, Vec<u8>)> = {
                    let lower = match resume_after {
                        Some(k) => Bound::Excluded(k),
                        None => Bound::Unbounded,
                    };
                    let range = table
                        .range::<u64>((lower, Bound::Unbounded))
                        .map_err(map_storage_error("Failed to range cold table"))?;
                    let mut v = Vec::with_capacity(batch_size);
                    for entry in range.take(batch_size) {
                        let (k, val) =
                            entry.map_err(map_storage_error("Failed to read cold entry"))?;
                        v.push((k.value(), val.value().to_vec()));
                    }
                    v
                };

                batch_len = collected.len();
                if batch_len == 0 {
                    // Table exhausted — nothing to commit; abort the empty txn.
                    drop(table);
                    drop(write_txn);
                    break;
                }

                for (key, value) in &collected {
                    // Idempotency backstop: already wrapped at the target → skip.
                    if let Some((kv, _)) = parse_cold_wrapper(value)
                        && kv == target_version
                    {
                        stats.values_skipped += 1;
                        last_key = Some(*key);
                        continue;
                    }
                    // Obtain the plaintext to encrypt under the target DEK. In
                    // `Rekey` mode the source value is decrypted under its own
                    // (old/legacy) generation; in `WrapPlaintext` mode (enable) the
                    // bare value already IS the plaintext (never encrypted), so it
                    // must NOT be decrypted (the cold reader would mis-read it as
                    // legacy ciphertext). Either way the SAME compressed bytes are
                    // re-wrapped: no record is decoded, so temporal bounds cannot
                    // change.
                    // `Rekey` owns a freshly decrypted buffer; `WrapPlaintext` (enable)
                    // encrypts the bare value in place with no extra allocation (the
                    // bare value already IS the plaintext — CLAUDE.md "avoid
                    // allocations": no per-value clone on the enable path).
                    let decrypted;
                    let compressed: &[u8] = match source_mode {
                        ColdSourceMode::Rekey => {
                            decrypted = self.decrypt_if_needed(value)?;
                            &decrypted
                        }
                        ColdSourceMode::WrapPlaintext => value.as_slice(),
                    };
                    let ciphertext = target_cipher.encrypt(compressed, &[]).map_err(
                        |e| -> crate::core::error::Error {
                            StorageError::Encryption(format!(
                                "Cold storage re-encryption failed: {e}"
                            ))
                            .into()
                        },
                    )?;
                    let wrapped = wrap_cold_value(target_version, &ciphertext);
                    table
                        .insert(*key, wrapped.as_slice())
                        .map_err(map_storage_error("Failed to rewrite cold value"))?;
                    stats.values_rewrapped += 1;
                    last_key = Some(*key);
                }
            }

            // Advance the durable cursor in the SAME transaction as the rewrites,
            // so a crash resumes from exactly the last committed key.
            {
                let cursor = ColdRotationCursor {
                    target_version,
                    table: table_kind,
                    last_completed_key: last_key.unwrap_or(0),
                };
                let mut meta = write_txn
                    .open_table(METADATA_TABLE)
                    .map_err(map_table_error("Failed to open metadata table"))?;
                meta.insert(COLD_ROTATION_CURSOR_KEY, cursor.to_bytes().as_slice())
                    .map_err(map_storage_error("Failed to write cold_rotation cursor"))?;
            }

            write_txn
                .commit()
                .map_err(map_commit_error("Failed to commit cold rotation batch"))?;
            stats.batches_committed += 1;

            resume_after = last_key;
            if batch_len < batch_size {
                // Fewer than a full batch means the table is exhausted.
                break;
            }
        }
        Ok(())
    }

    // ========================================================================
    // Logic moved from ColdStorage trait impl
    // ========================================================================

    fn store_entry_internal<V, F>(
        &self,
        version: &V,
        encode_fn: F,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
        stats_counter: &AtomicU64,
    ) -> Result<()>
    where
        V: EntityVersion,
        F: Fn(&V) -> Vec<u8>,
    {
        self.check_fail_writes()?;

        let encoded = encode_fn(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let to_store = self.encrypt_if_needed(compressed)?;
        let stored_size = to_store.len();

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        {
            let mut table =
                write_txn
                    .open_table(table_def)
                    .map_err(|e| -> crate::core::error::Error {
                        StorageError::io_error(format!(
                            "Failed to open table '{}': {}",
                            table_def.name(),
                            e
                        ))
                        .into()
                    })?;

            table
                .insert(version.version_id().as_u64(), to_store.as_slice())
                .map_err(map_storage_error("Failed to store version"))?;
        }

        // Widen the persisted cold-tier extent atomically with the version
        // write, so it survives restarts (Issue #3389).
        {
            let mut delta = ColdTemporalExtentBounds::default();
            delta.observe_interval(version.temporal());
            let mut meta = write_txn
                .open_table(METADATA_TABLE)
                .map_err(map_table_error("Failed to open metadata table"))?;
            Self::merge_extent_into_metadata(&mut meta, &delta)?;
        }

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit"))?;

        stats_counter.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(raw_size as u64, Ordering::Relaxed);
        self.stats
            .bytes_written_compressed
            .fetch_add(stored_size as u64, Ordering::Relaxed);

        Ok(())
    }

    fn get_entry_internal<V, F>(
        &self,
        id: VersionId,
        decode_fn: F,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
        stats_counter: &AtomicU64,
    ) -> Result<Option<V>>
    where
        F: Fn(&[u8]) -> Result<V>,
    {
        stats_counter.fetch_add(1, Ordering::Relaxed);

        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = read_txn
            .open_table(table_def)
            .map_err(|e| -> crate::core::error::Error {
                StorageError::io_error(format!(
                    "Failed to open table '{}': {}",
                    table_def.name(),
                    e
                ))
                .into()
            })?;

        match table.get(id.as_u64()) {
            Ok(Some(value)) => {
                let raw: &[u8] = value.value();
                self.stats
                    .bytes_read_compressed
                    .fetch_add(raw.len() as u64, Ordering::Relaxed);

                let compressed = self.decrypt_if_needed(raw)?;
                let decompressed = self.decompress(&compressed)?;
                self.stats
                    .bytes_read_decompressed
                    .fetch_add(decompressed.len() as u64, Ordering::Relaxed);

                let version = decode_fn(&decompressed)?;
                Ok(Some(version))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::io_error(format!("Failed to read version: {}", e)).into()),
        }
    }

    /// Decode every entry in a versions table.
    ///
    /// Full-table scan used by the temporal changefeed so that versions migrated out of the hot
    /// tier are still discoverable. Cold storage is keyed by `VersionId`, so there is no
    /// transaction-time index to narrow the scan — every entry is decoded and the caller filters.
    fn scan_entries_internal<V, F>(
        &self,
        decode_fn: F,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
    ) -> Result<Vec<V>>
    where
        F: Fn(&[u8]) -> Result<V>,
    {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = match read_txn.open_table(table_def) {
            Ok(table) => table,
            // A cold store that has never persisted this kind of version has no such table yet.
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => {
                return Err(StorageError::io_error(format!(
                    "Failed to open table '{}': {}",
                    table_def.name(),
                    e
                ))
                .into());
            }
        };

        let mut out = Vec::new();
        let iter = table
            .iter()
            .map_err(|e| StorageError::io_error(format!("Failed to iterate cold table: {}", e)))?;
        for entry in iter {
            let (_key, value) = entry
                .map_err(|e| StorageError::io_error(format!("Failed to read cold entry: {}", e)))?;
            let raw: &[u8] = value.value();
            let compressed = self.decrypt_if_needed(raw)?;
            let decompressed = self.decompress(&compressed)?;
            out.push(decode_fn(&decompressed)?);
        }
        Ok(out)
    }

    /// Decode every node version currently held in cold storage.
    ///
    /// This is an O(N) full scan + decode of the cold node-version table; prefer the by-id
    /// lookups for point reads. Used by the changefeed to include migrated history.
    pub fn scan_node_versions(&self) -> Result<Vec<NodeVersion>> {
        self.scan_entries_internal(decode_node_version, NODE_VERSIONS_TABLE)
    }

    /// Decode every edge version currently held in cold storage.
    ///
    /// This is an O(N) full scan + decode of the cold edge-version table; prefer the by-id
    /// lookups for point reads. Used by the changefeed to include migrated history.
    pub fn scan_edge_versions(&self) -> Result<Vec<EdgeVersion>> {
        self.scan_entries_internal(decode_edge_version, EDGE_VERSIONS_TABLE)
    }

    /// Stream-decode a versions table, invoking `emit` on each decoded version.
    ///
    /// Like [`scan_entries_internal`](Self::scan_entries_internal) but never accumulates the
    /// decoded versions into a `Vec` — the caller's `emit` closure decides what (if anything) to
    /// keep. This is what lets the filtered changefeed scan hold only `O(bound)` survivors in
    /// memory instead of the whole table.
    fn scan_versions_into<V, D, E>(
        &self,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
        decode_fn: D,
        mut emit: E,
    ) -> Result<()>
    where
        D: Fn(&[u8]) -> Result<V>,
        E: FnMut(V),
    {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = match read_txn.open_table(table_def) {
            Ok(table) => table,
            // A cold store that has never persisted this kind of version has no such table yet.
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(()),
            Err(e) => {
                return Err(StorageError::io_error(format!(
                    "Failed to open table '{}': {}",
                    table_def.name(),
                    e
                ))
                .into());
            }
        };

        let iter = table
            .iter()
            .map_err(|e| StorageError::io_error(format!("Failed to iterate cold table: {}", e)))?;
        for entry in iter {
            let (_key, value) = entry
                .map_err(|e| StorageError::io_error(format!("Failed to read cold entry: {}", e)))?;
            let raw: &[u8] = value.value();
            let compressed = self.decrypt_if_needed(raw)?;
            let decompressed = self.decompress(&compressed)?;
            emit(decode_fn(&decompressed)?);
        }
        Ok(())
    }

    /// Filter-during-decode changefeed scan of the cold tier (Issue #3216, PR 2).
    ///
    /// Runs the shared [`consider_version`] predicate (transaction-time window, optional
    /// valid-time window, optional label filter, and the strict `> resume_after` cursor) inline
    /// as each node then edge version is decoded, retaining only the `bound`-smallest survivors
    /// by [`ChangeCursor`] order. It returns `Vec<RawChange>` directly — the caller no longer
    /// materializes a full `Vec<NodeVersion>` / `Vec<EdgeVersion>` and re-filters it.
    ///
    /// # Correctness note — no `version_id` early-stop
    ///
    /// Cold storage is keyed by `VersionId`, but a version's id is allocated at transaction-build
    /// time while its commit timestamp is assigned later, so `version_id` is **not** monotonic
    /// with transaction time — two concurrent commits can invert the two orders. The changefeed
    /// is ordered by transaction time (`ChangeCursor`), so an early-stop / `range()` on the
    /// ascending `version_id` key would silently drop or misorder rows. This scan therefore
    /// **decodes every entry** (I/O stays O(N)) and only the in-memory retention is bounded. A
    /// true sub-linear cold pushdown needs a transaction-time-ordered directory (tracked
    /// follow-up), not a `version_id` range.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn collect_changes_filtered(
        &self,
        tx_window: &TimeRange,
        valid_window: Option<&TimeRange>,
        label_filter: Option<&str>,
        resume_after: Option<ChangeCursor>,
        bound: usize,
    ) -> Result<Vec<RawChange>> {
        let mut acc = BoundedChanges::new(bound);

        // Namespace derivation for the cold tier (Issue #3349, PR3c). The immutable
        // ride-along namespace key is carried only on **anchor** versions (a delta
        // never diffs the immutable key). Cold storage cannot cheaply reconstruct a
        // delta's property chain during a scan, but it does not need to *when the
        // covering anchor is also cold*: the table is keyed by `VersionId` and
        // iterated ascending, and an entity's create version (its lowest `VersionId`)
        // is always an anchor carrying the key — so an in-cold anchor is decoded
        // *before* any of its deltas. We record **every** anchor's namespace
        // (default included) in a running map as it is decoded and read a delta's
        // namespace back from it (fast path).
        //
        // # LRU anchor-split (the fail-closed miss)
        //
        // The "anchor always precedes delta" invariant only holds *within* the cold
        // scan. Under `MigrationPolicy::aggressive()` / `enable_lru`, a
        // frequently-accessed anchor can stay HOT while an older delta migrates COLD
        // — so a cold delta's covering anchor is absent from this scan. A map MISS is
        // therefore **not** a `default` entity; it is an unresolvable-here delta. We
        // stamp such a delta with the reserved
        // [`crate::core::namespace::UNRESOLVED_NAMESPACE`] sentinel — **never**
        // `default` — and the [`HistoricalStorage`] layer, which sees both tiers,
        // re-derives its real namespace via tier-aware reconstruction (see
        // `resolve_unresolved_namespaces`). Failing open to `default` here would leak
        // the change to `default`-scoped subscribers and hide it from its own.
        let unresolved_ns_id = unresolved_namespace_id();
        let mut node_ns: std::collections::HashMap<u64, NamespaceId> =
            std::collections::HashMap::new();
        let mut edge_ns: std::collections::HashMap<u64, NamespaceId> =
            std::collections::HashMap::new();

        self.scan_versions_into(
            NODE_VERSIONS_TABLE,
            decode_node_version,
            |v: NodeVersion| {
                let ns_id = Self::version_namespace_id(
                    &v.data,
                    v.node_id.as_u64(),
                    &mut node_ns,
                    unresolved_ns_id,
                );
                consider_version(
                    &mut acc,
                    resume_after,
                    v.id.as_u64(),
                    v.node_id.as_u64(),
                    EntityKind::Node,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                    tx_window,
                    valid_window,
                    label_filter,
                    move || ns_id,
                );
            },
        )?;

        self.scan_versions_into(
            EDGE_VERSIONS_TABLE,
            decode_edge_version,
            |v: EdgeVersion| {
                let ns_id = Self::version_namespace_id(
                    &v.data,
                    v.edge_id.as_u64(),
                    &mut edge_ns,
                    unresolved_ns_id,
                );
                consider_version(
                    &mut acc,
                    resume_after,
                    v.id.as_u64(),
                    v.edge_id.as_u64(),
                    EntityKind::Edge,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                    tx_window,
                    valid_window,
                    label_filter,
                    move || ns_id,
                );
            },
        )?;

        Ok(acc.into_vec())
    }

    /// Derive an entity version's interned [`NamespaceId`] during a cold-tier scan
    /// (Issue #3349, PR3c), maintaining the running anchor→namespace map keyed by
    /// entity id. An anchor's namespace is read directly from its properties and
    /// recorded; a delta reads its entity's namespace back from the map (its
    /// covering anchor, if also cold, was decoded earlier — see
    /// [`collect_changes_filtered`]).
    ///
    /// **Every** anchor is recorded — `default` entities included — so a map MISS
    /// for a delta is unambiguous: it means the delta's covering anchor is **not in
    /// this cold scan** (the LRU anchor-split case), not merely that the entity is
    /// `default`. Such a miss returns `unresolved_ns_id` (the fail-closed
    /// [`crate::core::namespace::UNRESOLVED_NAMESPACE`] sentinel) rather than
    /// `default`; the [`HistoricalStorage`](crate::storage::historical) layer
    /// re-derives the real namespace tier-aware afterward. Returning `default` here
    /// would leak the change to `default`-scoped subscribers.
    fn version_namespace_id(
        data: &VersionData,
        entity_id: u64,
        seen: &mut std::collections::HashMap<u64, NamespaceId>,
        unresolved_ns_id: NamespaceId,
    ) -> NamespaceId {
        match data {
            VersionData::Anchor { properties, .. } => {
                let ns_id = intern_namespace(&namespace_of(properties));
                // Record every anchor (default included) so a later delta MISS is a
                // genuine "anchor not in this scan" signal, not a default entity.
                seen.insert(entity_id, ns_id);
                ns_id
            }
            VersionData::Delta { .. } => seen.get(&entity_id).copied().unwrap_or(unresolved_ns_id),
        }
    }

    /// Store a single node version.
    ///
    /// Encodes and compresses the version before writing it to the `node_versions` table.
    ///
    /// # Performance
    ///
    /// Storing versions one-by-one is slower than batching. Prefer using
    /// [`store_batch_with_lsn`](Self::store_batch_with_lsn) or
    /// [`store_node_versions_batch`](Self::store_node_versions_batch) for bulk data.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::NodeVersion;
    /// # use aletheiadb::core::id::{VersionId, NodeId};
    /// # use aletheiadb::core::temporal::BiTemporalInterval;
    /// # use aletheiadb::core::property::PropertyMap;
    /// # use aletheiadb::core::interning::GLOBAL_INTERNER;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    ///
    /// let version = NodeVersion::new_anchor(
    ///     VersionId::new(1)?,
    ///     NodeId::new(100)?,
    ///     BiTemporalInterval::current(1000.into()),
    ///     GLOBAL_INTERNER.intern("Person")?,
    ///     PropertyMap::new(),
    /// );
    ///
    /// storage.store_node_version(&version)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        self.store_entry_internal(
            version,
            encode_node_version,
            NODE_VERSIONS_TABLE,
            &self.stats.node_versions_stored,
        )
    }

    /// Retrieve a node version by its specific `VersionId`.
    ///
    /// Decompresses and deserializes the payload back into a `NodeVersion`.
    /// Returns `Ok(None)` if the version does not exist in the cold storage tier.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// if let Some(version) = storage.get_node_version(id)? {
    ///     println!("Found historical version from transaction {}",
    ///              version.temporal.transaction_time().start().wallclock());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>> {
        self.get_entry_internal(
            id,
            decode_node_version,
            NODE_VERSIONS_TABLE,
            &self.stats.node_version_reads,
        )
    }

    /// Test-only: read the RAW stored bytes for a node version (the exact value in
    /// the `node_versions` table, before any decrypt/decompress), so a test can
    /// assert the on-disk wire shape (e.g. the `ACV1` wrapper after an enable wrap
    /// pass, or its absence for a bare plaintext value).
    #[cfg(test)]
    pub(crate) fn raw_node_value_for_test(&self, version_id: u64) -> Option<Vec<u8>> {
        let read_txn = self.db.begin_read().ok()?;
        let table = read_txn.open_table(NODE_VERSIONS_TABLE).ok()?;
        let guard = table.get(version_id).ok()??;
        Some(guard.value().to_vec())
    }

    /// Test-only: is the raw stored value for `version_id` an `ACV1`-wrapped value?
    #[cfg(test)]
    pub(crate) fn raw_node_value_is_acv1_for_test(&self, version_id: u64) -> bool {
        self.raw_node_value_for_test(version_id)
            .and_then(|v| parse_cold_wrapper(&v).map(|_| ()))
            .is_some()
    }

    /// Retrieve multiple node versions in a single call.
    ///
    /// Currently, this performs iterative reads, but provides an API surface
    /// for future optimizations (e.g., parallel reads or read-ahead caching).
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let ids = vec![VersionId::new(1)?, VersionId::new(2)?];
    /// let versions = storage.get_node_versions_batch(&ids)?;
    ///
    /// assert_eq!(versions.len(), 2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_node_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<NodeVersion>>> {
        ids.iter().map(|id| self.get_node_version(*id)).collect()
    }

    /// Store a single historical edge version.
    ///
    /// Encodes and compresses the edge version before writing it to the `edge_versions` table.
    /// Prefer batch operations for heavy write workloads.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::EdgeVersion;
    /// # use aletheiadb::core::id::{VersionId, EdgeId, NodeId};
    /// # use aletheiadb::core::temporal::BiTemporalInterval;
    /// # use aletheiadb::core::property::PropertyMap;
    /// # use aletheiadb::core::interning::GLOBAL_INTERNER;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let version = EdgeVersion::new_anchor(
    ///     VersionId::new(2)?,
    ///     EdgeId::new(500)?,
    ///     BiTemporalInterval::current(1000.into()),
    ///     GLOBAL_INTERNER.intern("KNOWS")?,
    ///     NodeId::new(1)?,
    ///     NodeId::new(2)?,
    ///     PropertyMap::new(),
    /// );
    ///
    /// storage.store_edge_version(&version)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        self.store_entry_internal(
            version,
            encode_edge_version,
            EDGE_VERSIONS_TABLE,
            &self.stats.edge_versions_stored,
        )
    }

    /// Retrieve an edge version by its specific `VersionId`.
    ///
    /// Returns `Ok(None)` if the historical edge version has not been flushed
    /// to cold storage or does not exist.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// let edge_opt = storage.get_edge_version(id)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>> {
        self.get_entry_internal(
            id,
            decode_edge_version,
            EDGE_VERSIONS_TABLE,
            &self.stats.edge_version_reads,
        )
    }

    /// Retrieve multiple edge versions in a single API call.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let ids = vec![VersionId::new(1)?];
    /// let versions = storage.get_edge_versions_batch(&ids)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_edge_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<EdgeVersion>>> {
        ids.iter().map(|id| self.get_edge_version(*id)).collect()
    }

    fn contains_entry_internal(
        &self,
        id: VersionId,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
    ) -> Result<bool> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = read_txn
            .open_table(table_def)
            .map_err(|e| -> crate::core::error::Error {
                StorageError::io_error(format!("Failed to open table: {}", e)).into()
            })?;

        match table.get(id.as_u64()) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(StorageError::io_error(format!("Failed to check version: {}", e)).into()),
        }
    }

    fn delete_entry_internal(
        &self,
        id: VersionId,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
    ) -> Result<bool> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        let deleted = {
            let mut table =
                write_txn
                    .open_table(table_def)
                    .map_err(|e| -> crate::core::error::Error {
                        StorageError::io_error(format!(
                            "Failed to open table '{}': {}",
                            table_def.name(),
                            e
                        ))
                        .into()
                    })?;

            match table.remove(id.as_u64()) {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(e) => {
                    return Err(map_storage_error("Failed to delete version")(e));
                }
            }
        };

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit"))?;

        Ok(deleted)
    }

    /// Quickly check if a node version exists without fetching its payload.
    ///
    /// This is significantly faster than calling [`get_node_version`](Self::get_node_version)
    /// because it skips reading and decompressing the potentially large property payload.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// if storage.contains_node_version(id)? {
    ///     println!("Version exists, but we didn't waste time loading its properties!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        self.contains_entry_internal(id, NODE_VERSIONS_TABLE)
    }

    /// Quickly check if an edge version exists without fetching its payload.
    ///
    /// Avoids decompression overhead if you only need to verify existence.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// assert_eq!(storage.contains_edge_version(id)?, false);
    /// # Ok(())
    /// # }
    /// ```
    pub fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        self.contains_entry_internal(id, EDGE_VERSIONS_TABLE)
    }

    /// Permanently delete a node version from cold storage.
    ///
    /// Returns `true` if the version existed and was deleted, `false` if it
    /// was not found. Space won't be recovered immediately; use [`compact`](Self::compact)
    /// to shrink the database file later if needed.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// let did_delete = storage.delete_node_version(id)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete_node_version(&self, id: VersionId) -> Result<bool> {
        self.delete_entry_internal(id, NODE_VERSIONS_TABLE)
    }

    /// Permanently delete an edge version from cold storage.
    ///
    /// Returns `true` if the version existed and was deleted, `false` otherwise.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// let did_delete = storage.delete_edge_version(id)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete_edge_version(&self, id: VersionId) -> Result<bool> {
        self.delete_entry_internal(id, EDGE_VERSIONS_TABLE)
    }

    fn write_prepared_batch_to_table(
        txn: &redb::WriteTransaction,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
        batch: &PreparedVersionBatch,
    ) -> Result<()> {
        let mut table = txn
            .open_table(table_def)
            .map_err(|e| -> crate::core::error::Error {
                StorageError::io_error(format!(
                    "Failed to open table '{}': {}",
                    table_def.name(),
                    e
                ))
                .into()
            })?;

        for (id, compressed) in &batch.entries {
            table
                .insert(*id, compressed.as_slice())
                .map_err(map_storage_error("Failed to store version"))?;
        }
        Ok(())
    }

    fn store_entries_batch_internal<V, PrepareFn>(
        &self,
        versions: &[V],
        prepare_fn: PrepareFn,
        table_def: redb::TableDefinition<'static, u64, &'static [u8]>,
        stats_counter: &AtomicU64,
    ) -> Result<()>
    where
        V: EntityVersion,
        PrepareFn: Fn(&[V]) -> Result<PreparedVersionBatch>,
    {
        self.check_fail_writes()?;

        if versions.is_empty() {
            return Ok(());
        }

        let prepared = prepare_fn(versions)?;
        let version_count = prepared.entries.len() as u64;
        let extent_delta = ColdTemporalExtentBounds::from_versions(versions);

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        Self::write_prepared_batch_to_table(&write_txn, table_def, &prepared)?;

        // Widen the persisted cold-tier extent atomically with the batch so it
        // survives restarts (Issue #3389).
        {
            let mut meta = write_txn
                .open_table(METADATA_TABLE)
                .map_err(map_table_error("Failed to open metadata table"))?;
            Self::merge_extent_into_metadata(&mut meta, &extent_delta)?;
        }

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit batch"))?;

        stats_counter.fetch_add(version_count, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(prepared.raw_size_bytes, Ordering::Relaxed);
        self.stats
            .bytes_written_compressed
            .fetch_add(prepared.compressed_size_bytes, Ordering::Relaxed);

        Ok(())
    }

    /// Store a batch of node versions efficiently.
    ///
    /// This method optimizes serialization and compression. If the batch size is
    /// large enough (e.g., > 1024), it uses Rayon to compress payloads in parallel.
    /// It then commits the entire batch in a single atomic database transaction.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::NodeVersion;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let versions: Vec<NodeVersion> = vec![]; // populate from WAL
    /// storage.store_node_versions_batch(&versions)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
        self.store_entries_batch_internal(
            versions,
            |v| self.prepare_node_versions_batch(v),
            NODE_VERSIONS_TABLE,
            &self.stats.node_versions_stored,
        )
    }

    /// Store a batch of edge versions efficiently.
    ///
    /// Parallels `store_node_versions_batch` but for edges. Uses a single
    /// transaction for maximum throughput.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::EdgeVersion;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let edges: Vec<EdgeVersion> = vec![]; // populate from WAL
    /// storage.store_edge_versions_batch(&edges)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
        self.store_entries_batch_internal(
            versions,
            |v| self.prepare_edge_versions_batch(v),
            EDGE_VERSIONS_TABLE,
            &self.stats.edge_versions_stored,
        )
    }

    /// Get a snapshot of internal usage statistics.
    ///
    /// Provides metrics on bytes read/written and error counts.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let stats = storage.stats();
    /// println!("Written raw bytes: {}", stats.bytes_written_raw);
    /// # Ok(())
    /// # }
    /// ```
    pub fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    /// Flush uncommitted data to disk.
    ///
    /// For `RedbColdStorage`, this is a fast no-op. Redb guarantees that once
    /// a write transaction is committed (which we do eagerly in batch operations),
    /// it is durable on disk.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// storage.flush()?; // Does nothing, returns Ok
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush(&self) -> Result<()> {
        // Redb automatically flushes on commit, nothing extra needed
        Ok(())
    }

    /// Close the database connection.
    ///
    /// This gives an explicit way to release the file lock before the object
    /// goes out of scope. For Redb, dropping the database instance achieves this.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// storage.close()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn close(&self) -> Result<()> {
        // Redb handles cleanup on drop
        Ok(())
    }

    /// Store a batch of versions with LSN tracking.
    ///
    /// This operation is **atomic** - either all versions are stored AND the LSN is updated,
    /// or nothing changes. This atomicity is crucial for safe WAL truncation.
    ///
    /// # Arguments
    ///
    /// * `nodes` - A slice of node versions to store.
    /// * `edges` - A slice of edge versions to store.
    /// * `lsn` - The Log Sequence Number (LSN) associated with this batch.
    ///
    /// # LSN Monotonicity
    ///
    /// The `flushed_lsn` in the metadata table will only be updated if the provided `lsn`
    /// is *greater than* the current stored value. This prevents race conditions where
    /// an out-of-order older batch might accidentally revert the flush progress marker.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aletheiadb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
    /// # use aletheiadb::storage::wal::LSN;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let storage = RedbColdStorage::with_default_config("data.redb")?;
    /// let nodes = vec![]; // ... populate
    /// let edges = vec![]; // ... populate
    /// let lsn = LSN(500);
    ///
    /// // Atomic commit
    /// storage.store_batch_with_lsn(&nodes, &edges, lsn)?;
    ///
    /// // Now safe to truncate WAL < 500
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_batch_with_lsn(
        &self,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        lsn: LSN,
    ) -> Result<()> {
        self.check_fail_writes()?;

        let prepared_nodes = self.prepare_node_versions_batch(nodes)?;
        let prepared_edges = self.prepare_edge_versions_batch(edges)?;
        let node_count = prepared_nodes.entries.len() as u64;
        let edge_count = prepared_edges.entries.len() as u64;

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        // Store node versions
        Self::write_prepared_batch_to_table(&write_txn, NODE_VERSIONS_TABLE, &prepared_nodes)?;

        // Store edge versions
        Self::write_prepared_batch_to_table(&write_txn, EDGE_VERSIONS_TABLE, &prepared_edges)?;

        // Update flushed_lsn and widen the persisted cold-tier extent
        // atomically with the batch (Issue #3389).
        {
            let mut table = write_txn
                .open_table(METADATA_TABLE)
                .map_err(map_table_error("Failed to open metadata table"))?;
            Self::set_flushed_lsn_internal(&mut table, lsn)?;

            let mut extent_delta = ColdTemporalExtentBounds::from_versions(nodes);
            extent_delta.merge(&ColdTemporalExtentBounds::from_versions(edges));
            Self::merge_extent_into_metadata(&mut table, &extent_delta)?;
        }

        // Commit atomically
        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit batch"))?;

        self.stats
            .node_versions_stored
            .fetch_add(node_count, Ordering::Relaxed);
        self.stats
            .edge_versions_stored
            .fetch_add(edge_count, Ordering::Relaxed);
        self.stats.bytes_written_raw.fetch_add(
            prepared_nodes.raw_size_bytes + prepared_edges.raw_size_bytes,
            Ordering::Relaxed,
        );
        self.stats.bytes_written_compressed.fetch_add(
            prepared_nodes.compressed_size_bytes + prepared_edges.compressed_size_bytes,
            Ordering::Relaxed,
        );

        Ok(())
    }

    /// Compact the database file to reclaim free space.
    ///
    /// As records are overwritten or deleted, Redb accumulates fragmented free space.
    /// This method copies all active data into a fresh, tightly-packed file, swapping
    /// it atomically with the old one.
    ///
    /// # Performance
    ///
    /// This operation is I/O heavy and blocks. It requires an exclusive `&mut self`
    /// reference, ensuring no other threads can access the storage during compaction.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let mut storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// // ... after many deletes ...
    /// storage.compact()?; // Space is now reclaimed
    /// # Ok(())
    /// # }
    /// ```
    pub fn compact(&mut self) -> Result<()> {
        self.db
            .compact()
            .map_err(map_compaction_error("Failed to compact database"))?;
        Ok(())
    }
}

// Serialization helpers using bitcode
// ============================================================================

/// Serializable wrapper for write-time provenance (Issues #3224 + #3350).
///
/// Mirrors [`crate::core::provenance::Provenance`]'s fields exactly.
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableProvenance {
    source: Option<String>,
    confidence: Option<f64>,
    note: Option<String>,
    correlation_id: Option<String>,
    principal: Option<String>,
}

/// Serializable wrapper for NodeVersion.
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableNodeVersion {
    id: u64,
    node_id: u64,
    temporal_valid_start: i64,
    temporal_valid_end: i64,
    temporal_tx_start: i64,
    temporal_tx_end: i64,
    label: String,
    data: SerializableVersionData,
    next_version: Option<u64>,
    prev_version: Option<u64>,
    provenance: Option<SerializableProvenance>,
}

/// Serializable wrapper for EdgeVersion.
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableEdgeVersion {
    id: u64,
    edge_id: u64,
    temporal_valid_start: i64,
    temporal_valid_end: i64,
    temporal_tx_start: i64,
    temporal_tx_end: i64,
    label: String,
    source: u64,
    target: u64,
    data: SerializableVersionData,
    next_version: Option<u64>,
    prev_version: Option<u64>,
    provenance: Option<SerializableProvenance>,
}

/// Pre-principal (Issue #3350) shape of the provenance bundle -- the
/// Issue #3224 shape without the `principal` field. Only referenced by the
/// frozen `..V2` record shapes below.
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableProvenanceV2 {
    source: Option<String>,
    confidence: Option<f64>,
    note: Option<String>,
    correlation_id: Option<String>,
}

/// Pre-principal (Issue #3350) shape of [`SerializableNodeVersion`]: written
/// with the [`COLD_RECORD_MAGIC_V2`] prefix by Issue-#3224-era binaries.
/// Identical to the current shape except its provenance bundle lacks
/// `principal`.
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableNodeVersionV2 {
    id: u64,
    node_id: u64,
    temporal_valid_start: i64,
    temporal_valid_end: i64,
    temporal_tx_start: i64,
    temporal_tx_end: i64,
    label: String,
    data: SerializableVersionData,
    next_version: Option<u64>,
    prev_version: Option<u64>,
    provenance: Option<SerializableProvenanceV2>,
}

/// Pre-principal (Issue #3350) shape of [`SerializableEdgeVersion`].
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableEdgeVersionV2 {
    id: u64,
    edge_id: u64,
    temporal_valid_start: i64,
    temporal_valid_end: i64,
    temporal_tx_start: i64,
    temporal_tx_end: i64,
    label: String,
    source: u64,
    target: u64,
    data: SerializableVersionData,
    next_version: Option<u64>,
    prev_version: Option<u64>,
    provenance: Option<SerializableProvenanceV2>,
}

/// Pre-provenance (Issue #3224) shape of [`SerializableNodeVersion`].
///
/// Cold-storage records have no version tag at all, so a legacy record is
/// indistinguishable from a current one except by trying to decode it. This
/// frozen shape is the fallback `decode_node_version` tries when the
/// tag-prefixed decodes don't apply (see [`COLD_RECORD_MAGIC_V2`] /
/// [`COLD_RECORD_MAGIC_V3`]).
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableNodeVersionV1 {
    id: u64,
    node_id: u64,
    temporal_valid_start: i64,
    temporal_valid_end: i64,
    temporal_tx_start: i64,
    temporal_tx_end: i64,
    label: String,
    data: SerializableVersionData,
    next_version: Option<u64>,
    prev_version: Option<u64>,
}

/// Pre-provenance (Issue #3224) shape of [`SerializableEdgeVersion`].
#[derive(bitcode::Encode, bitcode::Decode)]
struct SerializableEdgeVersionV1 {
    id: u64,
    edge_id: u64,
    temporal_valid_start: i64,
    temporal_valid_end: i64,
    temporal_tx_start: i64,
    temporal_tx_end: i64,
    label: String,
    source: u64,
    target: u64,
    data: SerializableVersionData,
    next_version: Option<u64>,
    prev_version: Option<u64>,
}

/// Magic sequence prepended to cold-storage records written with provenance
/// support (Issue #3224). Legacy (pre-#3224) records have no tag at all --
/// `decode_node_version`/`decode_edge_version` fall back to the untagged
/// [`SerializableNodeVersionV1`]/[`SerializableEdgeVersionV1`] shape when the
/// leading bytes don't match this magic (or the tagged decode fails).
///
/// This is a 4-byte sequence rather than a single tag byte specifically to
/// make an accidental collision with a legacy record's leading bytes (which
/// start with a bitcode-encoded `id: u64`) astronomically unlikely rather
/// than merely unlikely: a 1-in-256 chance (single byte) is a real risk over
/// the lifetime of a large cold-storage table, a 1-in-4-billion chance is
/// not. The value itself is arbitrary but deliberately not a small integer
/// (to avoid any structural resemblance to a plausible bitcode-encoded id).
const COLD_RECORD_MAGIC_V2: [u8; 4] = [0xA1, 0x37, 0xC0, 0xDE];

/// Magic sequence prepended to cold-storage records written with the
/// authenticated-principal provenance field (Issue #3350). Same collision
/// rationale as [`COLD_RECORD_MAGIC_V2`]; a distinct value so decoders can
/// tell the two tagged generations apart (the V2 provenance bundle lacks
/// the `principal` field).
const COLD_RECORD_MAGIC_V3: [u8; 4] = [0xB2, 0x48, 0xD1, 0xEF];

/// Serializable wrapper for VersionData.
#[derive(bitcode::Encode, bitcode::Decode)]
enum SerializableVersionData {
    Anchor {
        properties: Vec<(String, SerializablePropertyValue)>,
        vector_snapshot_id: Option<u64>,
    },
    Delta {
        changed: Vec<(String, SerializablePropertyValue)>,
        removed: Vec<String>,
    },
}

/// Serializable property value.
///
/// Note: Array is stored as a serialized `Vec<u8>` to avoid recursive type issues with bitcode.
#[derive(bitcode::Encode, bitcode::Decode)]
enum SerializablePropertyValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    /// Array is stored as bitcode-encoded bytes to avoid recursive type issues.
    Array(Vec<u8>),
    Vector(Vec<f32>),
    SparseVector {
        dimension: usize,
        indices: Vec<u32>,
        values: Vec<f32>,
    },
}

/// Encode a `NodeVersion` into a byte payload for storage.
///
/// Uses `bitcode` for extremely fast serialization. The resulting byte array
/// is deterministic, compact, and optimized for compression.
///
/// #[doc(hidden)]
pub fn encode_node_version(version: &NodeVersion) -> Vec<u8> {
    use crate::core::interning::GLOBAL_INTERNER;

    let serializable = SerializableNodeVersion {
        id: version.id.as_u64(),
        node_id: version.node_id.as_u64(),
        temporal_valid_start: version.temporal.valid_time().start().wallclock(),
        temporal_valid_end: version.temporal.valid_time().end().wallclock(),
        temporal_tx_start: version.temporal.transaction_time().start().wallclock(),
        temporal_tx_end: version.temporal.transaction_time().end().wallclock(),
        label: GLOBAL_INTERNER
            .resolve_with(version.label, |s| s.to_string())
            .unwrap_or_default(),
        data: encode_version_data(&version.data),
        next_version: version.next_version.map(|v| v.as_u64()),
        prev_version: version.prev_version.map(|v| v.as_u64()),
        provenance: encode_provenance(version.provenance.as_deref()),
    };

    let mut out = Vec::with_capacity(COLD_RECORD_MAGIC_V3.len());
    out.extend_from_slice(&COLD_RECORD_MAGIC_V3);
    out.extend_from_slice(&bitcode::encode(&serializable));
    out
}

/// Convert an in-memory [`Provenance`](crate::core::provenance::Provenance)
/// into its cold-storage representation.
fn encode_provenance(
    provenance: Option<&crate::core::provenance::Provenance>,
) -> Option<SerializableProvenance> {
    provenance.map(|p| SerializableProvenance {
        source: p.source().map(String::from),
        confidence: p.confidence(),
        note: p.note().map(String::from),
        correlation_id: p.correlation_id().map(String::from),
        principal: p.principal().map(String::from),
    })
}

/// Restore a cold-storage provenance bundle back into an in-memory
/// [`Provenance`](crate::core::provenance::Provenance).
fn decode_provenance(
    persisted: Option<SerializableProvenance>,
) -> Result<Option<std::sync::Arc<crate::core::provenance::Provenance>>> {
    let Some(p) = persisted else {
        return Ok(None);
    };
    use crate::core::provenance::Provenance;
    let provenance = Provenance::from_parts(
        p.source,
        p.confidence,
        p.note,
        p.correlation_id,
        p.principal,
    )
    .map_err(|e| StorageError::corruption(format!("Invalid persisted provenance: {}", e)))?;
    Ok(Some(std::sync::Arc::new(provenance)))
}

/// Restore a pre-principal (Issue #3350) cold-storage provenance bundle:
/// same as [`decode_provenance`] with `principal: None`.
fn decode_provenance_v2(
    persisted: Option<SerializableProvenanceV2>,
) -> Result<Option<std::sync::Arc<crate::core::provenance::Provenance>>> {
    decode_provenance(persisted.map(|p| SerializableProvenance {
        source: p.source,
        confidence: p.confidence,
        note: p.note,
        correlation_id: p.correlation_id,
        principal: None,
    }))
}

/// Decode a `NodeVersion` from a previously serialized byte payload.
///
/// Reconstructs the internal references (like `InternedString`) alongside
/// the core node temporal data.
///
/// #[doc(hidden)]
pub fn decode_node_version(data: &[u8]) -> Result<NodeVersion> {
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::NodeId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::temporal::{BiTemporalInterval, TimeRange};

    // Tagged records carry provenance: V3 (Issue #3350) bundles include the
    // authenticated principal, V2 (Issue #3224) bundles do not. Untagged
    // records are pre-provenance and are decoded via the frozen `..V1`
    // shape instead. There is no version marker on legacy records at all,
    // so the magic sequences themselves are the only signal -- see
    // `COLD_RECORD_MAGIC_V2` / `COLD_RECORD_MAGIC_V3`.
    //
    // A magic-byte match means this is unambiguously a record of that tagged
    // generation (legacy records never carry either prefix), so a decode
    // failure past the magic indicates real corruption -- it must be
    // surfaced directly rather than falling through to an older decoder,
    // which would otherwise misinterpret the remaining bytes
    // (magic-matched-but-corrupt is not "maybe legacy").
    let (
        label,
        temporal_valid_start,
        temporal_valid_end,
        temporal_tx_start,
        temporal_tx_end,
        id,
        node_id,
        data_field,
        next_version,
        prev_version,
        provenance,
    ) = if data.starts_with(&COLD_RECORD_MAGIC_V3) {
        let s: SerializableNodeVersion = bitcode::decode(&data[COLD_RECORD_MAGIC_V3.len()..])
            .map_err(|e| {
                StorageError::corruption(format!("Failed to decode node version V3: {}", e))
            })?;
        (
            s.label,
            s.temporal_valid_start,
            s.temporal_valid_end,
            s.temporal_tx_start,
            s.temporal_tx_end,
            s.id,
            s.node_id,
            s.data,
            s.next_version,
            s.prev_version,
            decode_provenance(s.provenance)?,
        )
    } else if data.starts_with(&COLD_RECORD_MAGIC_V2) {
        let s: SerializableNodeVersionV2 = bitcode::decode(&data[COLD_RECORD_MAGIC_V2.len()..])
            .map_err(|e| {
                StorageError::corruption(format!("Failed to decode node version V2: {}", e))
            })?;
        (
            s.label,
            s.temporal_valid_start,
            s.temporal_valid_end,
            s.temporal_tx_start,
            s.temporal_tx_end,
            s.id,
            s.node_id,
            s.data,
            s.next_version,
            s.prev_version,
            decode_provenance_v2(s.provenance)?,
        )
    } else {
        let s: SerializableNodeVersionV1 = bitcode::decode(data).map_err(|e| {
            StorageError::corruption(format!("Failed to decode node version V1: {}", e))
        })?;
        (
            s.label,
            s.temporal_valid_start,
            s.temporal_valid_end,
            s.temporal_tx_start,
            s.temporal_tx_end,
            s.id,
            s.node_id,
            s.data,
            s.next_version,
            s.prev_version,
            None,
        )
    };

    let valid_time = TimeRange::new(
        HybridTimestamp::new_unchecked(temporal_valid_start, 0),
        HybridTimestamp::new_unchecked(temporal_valid_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid valid time range: {}", e)))?;
    let tx_time = TimeRange::new(
        HybridTimestamp::new_unchecked(temporal_tx_start, 0),
        HybridTimestamp::new_unchecked(temporal_tx_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid transaction time range: {}", e)))?;

    Ok(NodeVersion {
        id: VersionId::new(id)
            .map_err(|e| StorageError::corruption(format!("Invalid version ID: {}", e)))?,
        node_id: NodeId::new(node_id)
            .map_err(|e| StorageError::corruption(format!("Invalid node ID: {}", e)))?,
        commit_timestamp: tx_time.start(),
        temporal: BiTemporalInterval::new(valid_time, tx_time),
        label: GLOBAL_INTERNER
            .intern(&label)
            .map_err(|e| StorageError::corruption(format!("Failed to intern label: {}", e)))?,
        data: decode_version_data(data_field)?,
        next_version: next_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid next version ID: {}", e)))?,
        prev_version: prev_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid prev version ID: {}", e)))?,
        provenance,
    })
}

/// Encode an `EdgeVersion` into a byte payload.
///
/// Converts the complex `EdgeVersion` (including source and target pointers)
/// into a flat, binary `bitcode` representation.
///
/// #[doc(hidden)]
pub fn encode_edge_version(version: &EdgeVersion) -> Vec<u8> {
    use crate::core::interning::GLOBAL_INTERNER;

    let serializable = SerializableEdgeVersion {
        id: version.id.as_u64(),
        edge_id: version.edge_id.as_u64(),
        temporal_valid_start: version.temporal.valid_time().start().wallclock(),
        temporal_valid_end: version.temporal.valid_time().end().wallclock(),
        temporal_tx_start: version.temporal.transaction_time().start().wallclock(),
        temporal_tx_end: version.temporal.transaction_time().end().wallclock(),
        label: GLOBAL_INTERNER
            .resolve_with(version.label, |s| s.to_string())
            .unwrap_or_default(),
        source: version.source.as_u64(),
        target: version.target.as_u64(),
        data: encode_version_data(&version.data),
        next_version: version.next_version.map(|v| v.as_u64()),
        prev_version: version.prev_version.map(|v| v.as_u64()),
        provenance: encode_provenance(version.provenance.as_deref()),
    };

    let mut out = Vec::with_capacity(COLD_RECORD_MAGIC_V3.len());
    out.extend_from_slice(&COLD_RECORD_MAGIC_V3);
    out.extend_from_slice(&bitcode::encode(&serializable));
    out
}

/// Decode an `EdgeVersion` from a byte payload.
///
/// Reconstructs all internal metadata, including `InternedString` labels
/// and associated endpoints.
///
/// #[doc(hidden)]
pub fn decode_edge_version(data: &[u8]) -> Result<EdgeVersion> {
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::temporal::{BiTemporalInterval, TimeRange};

    // See `decode_node_version` for why a magic-byte match means a decode
    // failure past the magic must be surfaced immediately rather than
    // falling back to the untagged legacy shape.
    #[allow(clippy::type_complexity)]
    let (
        label,
        temporal_valid_start,
        temporal_valid_end,
        temporal_tx_start,
        temporal_tx_end,
        id,
        edge_id,
        source,
        target,
        data_field,
        next_version,
        prev_version,
        provenance,
    ) = if data.starts_with(&COLD_RECORD_MAGIC_V3) {
        let s: SerializableEdgeVersion = bitcode::decode(&data[COLD_RECORD_MAGIC_V3.len()..])
            .map_err(|e| {
                StorageError::corruption(format!("Failed to decode edge version V3: {}", e))
            })?;
        (
            s.label,
            s.temporal_valid_start,
            s.temporal_valid_end,
            s.temporal_tx_start,
            s.temporal_tx_end,
            s.id,
            s.edge_id,
            s.source,
            s.target,
            s.data,
            s.next_version,
            s.prev_version,
            decode_provenance(s.provenance)?,
        )
    } else if data.starts_with(&COLD_RECORD_MAGIC_V2) {
        let s: SerializableEdgeVersionV2 = bitcode::decode(&data[COLD_RECORD_MAGIC_V2.len()..])
            .map_err(|e| {
                StorageError::corruption(format!("Failed to decode edge version V2: {}", e))
            })?;
        (
            s.label,
            s.temporal_valid_start,
            s.temporal_valid_end,
            s.temporal_tx_start,
            s.temporal_tx_end,
            s.id,
            s.edge_id,
            s.source,
            s.target,
            s.data,
            s.next_version,
            s.prev_version,
            decode_provenance_v2(s.provenance)?,
        )
    } else {
        let s: SerializableEdgeVersionV1 = bitcode::decode(data).map_err(|e| {
            StorageError::corruption(format!("Failed to decode edge version V1: {}", e))
        })?;
        (
            s.label,
            s.temporal_valid_start,
            s.temporal_valid_end,
            s.temporal_tx_start,
            s.temporal_tx_end,
            s.id,
            s.edge_id,
            s.source,
            s.target,
            s.data,
            s.next_version,
            s.prev_version,
            None,
        )
    };

    let valid_time = TimeRange::new(
        HybridTimestamp::new_unchecked(temporal_valid_start, 0),
        HybridTimestamp::new_unchecked(temporal_valid_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid valid time range: {}", e)))?;
    let tx_time = TimeRange::new(
        HybridTimestamp::new_unchecked(temporal_tx_start, 0),
        HybridTimestamp::new_unchecked(temporal_tx_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid transaction time range: {}", e)))?;

    Ok(EdgeVersion {
        id: VersionId::new(id)
            .map_err(|e| StorageError::corruption(format!("Invalid version ID: {}", e)))?,
        edge_id: EdgeId::new(edge_id)
            .map_err(|e| StorageError::corruption(format!("Invalid edge ID: {}", e)))?,
        commit_timestamp: tx_time.start(),
        temporal: BiTemporalInterval::new(valid_time, tx_time),
        label: GLOBAL_INTERNER
            .intern(&label)
            .map_err(|e| StorageError::corruption(format!("Failed to intern label: {}", e)))?,
        source: NodeId::new(source)
            .map_err(|e| StorageError::corruption(format!("Invalid source ID: {}", e)))?,
        target: NodeId::new(target)
            .map_err(|e| StorageError::corruption(format!("Invalid target ID: {}", e)))?,
        data: decode_version_data(data_field)?,
        next_version: next_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid next version ID: {}", e)))?,
        prev_version: prev_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid prev version ID: {}", e)))?,
        provenance,
    })
}

fn encode_version_data(data: &crate::storage::version::VersionData) -> SerializableVersionData {
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::storage::version::VersionData;

    match data {
        VersionData::Anchor {
            properties,
            vector_snapshot_id,
        } => SerializableVersionData::Anchor {
            properties: properties
                .iter()
                .map(|(k, v)| {
                    (
                        GLOBAL_INTERNER
                            .resolve_with(*k, |s| s.to_string())
                            .unwrap_or_default(),
                        encode_property_value(v),
                    )
                })
                .collect(),
            vector_snapshot_id: vector_snapshot_id.map(|id| id as u64),
        },
        VersionData::Delta { delta } => SerializableVersionData::Delta {
            changed: delta
                .changed
                .iter()
                .map(|(k, v)| {
                    (
                        GLOBAL_INTERNER
                            .resolve_with(*k, |s| s.to_string())
                            .unwrap_or_default(),
                        encode_property_value(v),
                    )
                })
                .collect(),
            removed: delta
                .removed
                .iter()
                .map(|k| {
                    GLOBAL_INTERNER
                        .resolve_with(*k, |s| s.to_string())
                        .unwrap_or_default()
                })
                .collect(),
        },
    }
}

fn decode_version_data(
    data: SerializableVersionData,
) -> Result<crate::storage::version::VersionData> {
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::version::{PropertyDelta, VersionData};

    match data {
        SerializableVersionData::Anchor {
            properties,
            vector_snapshot_id,
        } => {
            let mut builder = PropertyMapBuilder::new();
            for (key, value) in properties {
                builder = builder.insert(&key, decode_property_value(value)?);
            }
            Ok(VersionData::Anchor {
                properties: builder.build(),
                vector_snapshot_id: vector_snapshot_id.map(|id| id as usize),
            })
        }
        SerializableVersionData::Delta { changed, removed } => {
            let mut delta = PropertyDelta::new();
            for (key, value) in changed {
                let interned_key = GLOBAL_INTERNER.intern(&key).map_err(|e| {
                    StorageError::corruption(format!("Failed to intern key: {}", e))
                })?;
                delta
                    .changed
                    .insert(interned_key, decode_property_value(value)?);
            }
            for key in removed {
                let interned_key = GLOBAL_INTERNER.intern(&key).map_err(|e| {
                    StorageError::corruption(format!("Failed to intern key: {}", e))
                })?;
                delta.removed.insert(interned_key);
            }
            Ok(VersionData::Delta { delta })
        }
    }
}

/// Simple array element for encoding (non-recursive).
#[derive(bitcode::Encode, bitcode::Decode)]
enum SimpleArrayElement {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Vector(Vec<f32>),
}

fn encode_property_value(
    value: &crate::core::property::PropertyValue,
) -> SerializablePropertyValue {
    use crate::core::property::PropertyValue;

    match value {
        PropertyValue::Null => SerializablePropertyValue::Null,
        PropertyValue::Bool(b) => SerializablePropertyValue::Bool(*b),
        PropertyValue::Int(i) => SerializablePropertyValue::Int(*i),
        PropertyValue::Float(f) => SerializablePropertyValue::Float(*f),
        PropertyValue::String(s) => SerializablePropertyValue::String(s.to_string()),
        PropertyValue::Bytes(b) => SerializablePropertyValue::Bytes(b.to_vec()),
        PropertyValue::Array(arr) => {
            // Encode array elements as simple types (no nested arrays supported)
            let elements: Vec<SimpleArrayElement> = arr
                .iter()
                .map(|v| match v {
                    PropertyValue::Null => SimpleArrayElement::Null,
                    PropertyValue::Bool(b) => SimpleArrayElement::Bool(*b),
                    PropertyValue::Int(i) => SimpleArrayElement::Int(*i),
                    PropertyValue::Float(f) => SimpleArrayElement::Float(*f),
                    PropertyValue::String(s) => SimpleArrayElement::String(s.to_string()),
                    PropertyValue::Bytes(b) => SimpleArrayElement::Bytes(b.to_vec()),
                    PropertyValue::Vector(v) => SimpleArrayElement::Vector(v.to_vec()),
                    // Nested arrays and sparse vectors are not supported in cold storage arrays
                    _ => SimpleArrayElement::Null,
                })
                .collect();
            SerializablePropertyValue::Array(bitcode::encode(&elements))
        }
        PropertyValue::Vector(v) => SerializablePropertyValue::Vector(v.to_vec()),
        PropertyValue::SparseVector(sv) => SerializablePropertyValue::SparseVector {
            dimension: sv.dimension(),
            indices: sv.indices().to_vec(),
            values: sv.values().to_vec(),
        },
    }
}

fn decode_property_value(
    value: SerializablePropertyValue,
) -> Result<crate::core::property::PropertyValue> {
    use crate::core::property::PropertyValue;
    use crate::core::vector::SparseVec;

    Ok(match value {
        SerializablePropertyValue::Null => PropertyValue::Null,
        SerializablePropertyValue::Bool(b) => PropertyValue::Bool(b),
        SerializablePropertyValue::Int(i) => PropertyValue::Int(i),
        SerializablePropertyValue::Float(f) => PropertyValue::Float(f),
        SerializablePropertyValue::String(s) => PropertyValue::string(&s),
        SerializablePropertyValue::Bytes(b) => PropertyValue::bytes(&b),
        SerializablePropertyValue::Array(encoded) => {
            let elements: Vec<SimpleArrayElement> = bitcode::decode(&encoded)
                .map_err(|e| StorageError::corruption(format!("Failed to decode array: {}", e)))?;
            let values: Vec<PropertyValue> = elements
                .into_iter()
                .map(|e| match e {
                    SimpleArrayElement::Null => PropertyValue::Null,
                    SimpleArrayElement::Bool(b) => PropertyValue::Bool(b),
                    SimpleArrayElement::Int(i) => PropertyValue::Int(i),
                    SimpleArrayElement::Float(f) => PropertyValue::Float(f),
                    SimpleArrayElement::String(s) => PropertyValue::string(&s),
                    SimpleArrayElement::Bytes(b) => PropertyValue::bytes(&b),
                    SimpleArrayElement::Vector(v) => PropertyValue::vector(&v),
                })
                .collect();
            PropertyValue::array(values)
        }
        SerializablePropertyValue::Vector(v) => PropertyValue::vector(&v),
        SerializablePropertyValue::SparseVector {
            dimension,
            indices,
            values,
        } => {
            let sparse = SparseVec::new(indices, values, dimension as u32)
                .map_err(|e| StorageError::corruption(format!("Invalid sparse vector: {}", e)))?;
            PropertyValue::sparse_vector(sparse)
        }
    })
}

#[cfg(test)]
mod tests;
