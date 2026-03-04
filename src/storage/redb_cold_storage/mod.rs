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

use crate::core::error::{Result, StorageError};
use crate::core::id::VersionId;
use crate::core::version::{EdgeVersion, EntityVersion, NodeVersion};
use crate::storage::wal::LSN;
use rayon::prelude::*;
use redb::{ReadableDatabase, ReadableTable, TableHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

#[cfg(test)]
use std::sync::atomic::AtomicBool;

// Table definitions with static lifetimes
const NODE_VERSIONS_TABLE: redb::TableDefinition<'static, u64, &'static [u8]> =
    redb::TableDefinition::new("node_versions");
const EDGE_VERSIONS_TABLE: redb::TableDefinition<'static, u64, &'static [u8]> =
    redb::TableDefinition::new("edge_versions");
const METADATA_TABLE: redb::TableDefinition<'static, &'static str, &'static [u8]> =
    redb::TableDefinition::new("metadata");

/// Metadata keys stored in the metadata table.
const FLUSHED_LSN_KEY: &str = "flushed_lsn";

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
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
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
    /// Get the zstd compression level for this algorithm.
    pub fn zstd_level(&self) -> Option<i32> {
        match self {
            CompressionAlgorithm::None => None,
            CompressionAlgorithm::Zstd => Some(3), // Balanced ratio/speed
            CompressionAlgorithm::Fast => Some(1), // Speed-optimized
        }
    }
}

/// Configuration for cold storage.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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
#[derive(Debug, Clone, Default)]
pub struct ColdStorageStats {
    /// Total number of node versions stored.
    pub node_versions_stored: u64,
    /// Total number of edge versions stored.
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
    /// Calculate the compression ratio (raw/compressed).
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
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a snapshot of the current statistics.
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
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
pub struct RedbConfig {
    /// Compression algorithm for stored values.
    pub compression: CompressionAlgorithm,

    /// Enable CRC32 checksums for data integrity (in addition to Redb's built-in checksums).
    pub enable_checksums: bool,

    /// Cache size in bytes for Redb (0 = use default).
    pub cache_size_bytes: usize,
}

impl Default for RedbConfig {
    fn default() -> Self {
        Self {
            compression: CompressionAlgorithm::Zstd,
            enable_checksums: true,
            cache_size_bytes: 0,
        }
    }
}

impl RedbConfig {
    /// Create a new Redb configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the compression algorithm.
    pub fn compression(mut self, compression: CompressionAlgorithm) -> Self {
        self.compression = compression;
        self
    }

    /// Enable or disable checksums.
    pub fn enable_checksums(mut self, enable: bool) -> Self {
        self.enable_checksums = enable;
        self
    }

    /// Set the cache size in bytes.
    pub fn cache_size_bytes(mut self, size: usize) -> Self {
        self.cache_size_bytes = size;
        self
    }

    /// Convert to ColdStorageConfig for compression/checksum handling.
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
    /// Fault injection flag for testing.
    #[cfg(test)]
    fail_writes: AtomicBool,
    #[cfg(test)]
    writes_attempted: AtomicBool,
}

impl RedbColdStorage {
    /// Create a new Redb cold storage at the given path.
    ///
    /// # Arguments
    ///
    /// * `path` - Directory or file path for the Redb database file.
    /// * `config` - Configuration for compression and caching.
    ///
    /// # Errors
    ///
    /// Returns an error if the database file cannot be created or opened.
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

        // Open tables to create them
        write_txn
            .open_table(NODE_VERSIONS_TABLE)
            .map_err(map_table_error("Failed to create node_versions table"))?;
        write_txn
            .open_table(EDGE_VERSIONS_TABLE)
            .map_err(map_table_error("Failed to create edge_versions table"))?;
        write_txn
            .open_table(METADATA_TABLE)
            .map_err(map_table_error("Failed to create metadata table"))?;

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit table creation"))?;

        Ok(Self {
            path,
            db,
            config,
            stats: AtomicColdStorageStats::new(),
            #[cfg(test)]
            fail_writes: AtomicBool::new(false),
            #[cfg(test)]
            writes_attempted: AtomicBool::new(false),
        })
    }

    /// Create with default configuration.
    ///
    /// Equivalent to `RedbColdStorage::new(path, RedbConfig::default())`.
    pub fn with_default_config<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::new(path, RedbConfig::default())
    }

    /// Get the database file path.
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

        if self.should_parallel_compress(versions.len()) {
            versions
                .par_iter()
                .try_fold(PreparedVersionBatch::default, |mut prepared, version| {
                    let encoded = encode_version(version);
                    let raw_size_bytes = encoded.len() as u64;
                    let compressed = crate::storage::compression::compress(&encoded, &cold_config)?;
                    prepared.add_entry(version.version_id(), compressed, raw_size_bytes);
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
                let compressed = crate::storage::compression::compress(&encoded, &cold_config)?;
                prepared.add_entry(version.version_id(), compressed, raw_size_bytes);
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

    /// Set fault injection flag for write operations (Test only).
    #[cfg(test)]
    pub fn set_fail_writes(&self, fail: bool) {
        self.fail_writes.store(fail, Ordering::SeqCst);
    }

    /// Check if a write failure was injected and attempted (Test only).
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

    /// Get the flushed LSN from the metadata table.
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
        let compressed_size = compressed.len();

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
                .insert(version.version_id().as_u64(), compressed.as_slice())
                .map_err(map_storage_error("Failed to store version"))?;
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
            .fetch_add(compressed_size as u64, Ordering::Relaxed);

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
                let compressed: &[u8] = value.value();
                self.stats
                    .bytes_read_compressed
                    .fetch_add(compressed.len() as u64, Ordering::Relaxed);

                let decompressed = self.decompress(compressed)?;
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

    /// Store a single node version.
    ///
    /// Encodes and compresses the version before writing it to the `node_versions` table.
    pub fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        self.store_entry_internal(
            version,
            encode_node_version,
            NODE_VERSIONS_TABLE,
            &self.stats.node_versions_stored,
        )
    }

    /// Retrieve a node version by its ID.
    ///
    /// Returns `Ok(None)` if the version does not exist.
    pub fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>> {
        self.get_entry_internal(
            id,
            decode_node_version,
            NODE_VERSIONS_TABLE,
            &self.stats.node_version_reads,
        )
    }

    /// Retrieve multiple node versions efficiently.
    pub fn get_node_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<NodeVersion>>> {
        ids.iter().map(|id| self.get_node_version(*id)).collect()
    }

    /// Store a single edge version.
    pub fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        self.store_entry_internal(
            version,
            encode_edge_version,
            EDGE_VERSIONS_TABLE,
            &self.stats.edge_versions_stored,
        )
    }

    /// Retrieve an edge version by its ID.
    pub fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>> {
        self.get_entry_internal(
            id,
            decode_edge_version,
            EDGE_VERSIONS_TABLE,
            &self.stats.edge_version_reads,
        )
    }

    /// Retrieve multiple edge versions efficiently.
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

    /// Check if a node version exists in cold storage.
    pub fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        self.contains_entry_internal(id, NODE_VERSIONS_TABLE)
    }

    /// Check if an edge version exists in cold storage.
    pub fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        self.contains_entry_internal(id, EDGE_VERSIONS_TABLE)
    }

    /// Delete a node version.
    ///
    /// Returns `true` if the version existed and was deleted, `false` otherwise.
    pub fn delete_node_version(&self, id: VersionId) -> Result<bool> {
        self.delete_entry_internal(id, NODE_VERSIONS_TABLE)
    }

    /// Delete an edge version.
    ///
    /// Returns `true` if the version existed and was deleted, `false` otherwise.
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

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        Self::write_prepared_batch_to_table(&write_txn, table_def, &prepared)?;

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

    /// Store a batch of node versions.
    ///
    /// Encodes and compresses versions in parallel (if batch size exceeds threshold),
    /// then writes them in a single transaction.
    pub fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
        self.store_entries_batch_internal(
            versions,
            |v| self.prepare_node_versions_batch(v),
            NODE_VERSIONS_TABLE,
            &self.stats.node_versions_stored,
        )
    }

    /// Store a batch of edge versions.
    ///
    /// Encodes and compresses versions in parallel (if batch size exceeds threshold),
    /// then writes them in a single transaction.
    pub fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
        self.store_entries_batch_internal(
            versions,
            |v| self.prepare_edge_versions_batch(v),
            EDGE_VERSIONS_TABLE,
            &self.stats.edge_versions_stored,
        )
    }

    /// Get usage statistics.
    pub fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    /// Flush to disk.
    ///
    /// For Redb, this is a no-op as transactions are durable on commit.
    /// This method exists to satisfy the storage trait interface.
    pub fn flush(&self) -> Result<()> {
        // Redb automatically flushes on commit, nothing extra needed
        Ok(())
    }

    /// Close the database connection.
    ///
    /// This releases the file lock and resources.
    /// Note: The actual file lock is released when the inner `redb::Database` is dropped.
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

        // Update flushed_lsn atomically with the batch
        {
            let mut table = write_txn
                .open_table(METADATA_TABLE)
                .map_err(map_table_error("Failed to open metadata table"))?;
            Self::set_flushed_lsn_internal(&mut table, lsn)?;
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

    /// Compact the database to reclaim space.
    ///
    /// Performs a database-wide compaction. This copies all live data to a new file
    /// and replaces the old file, removing wasted space from deleted or overwritten records.
    ///
    /// # Locking
    ///
    /// This requires a mutable reference (`&mut self`) and will block other operations
    /// until compaction is complete.
    pub fn compact(&mut self) -> Result<()> {
        self.db
            .compact()
            .map_err(map_compaction_error("Failed to compact database"))?;
        Ok(())
    }
}

// Serialization helpers using bitcode
// ============================================================================

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
}

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

/// Encode a NodeVersion to bytes for storage.
///
/// This function is public for use by all cold storage implementations.
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
    };

    bitcode::encode(&serializable)
}

/// Decode a NodeVersion from bytes.
///
/// This function is public for use by all cold storage implementations.
pub fn decode_node_version(data: &[u8]) -> Result<NodeVersion> {
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::NodeId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::temporal::{BiTemporalInterval, TimeRange};

    let serializable: SerializableNodeVersion = bitcode::decode(data)
        .map_err(|e| StorageError::corruption(format!("Failed to decode node version: {}", e)))?;

    let valid_time = TimeRange::new(
        HybridTimestamp::new_unchecked(serializable.temporal_valid_start, 0),
        HybridTimestamp::new_unchecked(serializable.temporal_valid_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid valid time range: {}", e)))?;
    let tx_time = TimeRange::new(
        HybridTimestamp::new_unchecked(serializable.temporal_tx_start, 0),
        HybridTimestamp::new_unchecked(serializable.temporal_tx_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid transaction time range: {}", e)))?;

    Ok(NodeVersion {
        id: VersionId::new(serializable.id)
            .map_err(|e| StorageError::corruption(format!("Invalid version ID: {}", e)))?,
        node_id: NodeId::new(serializable.node_id)
            .map_err(|e| StorageError::corruption(format!("Invalid node ID: {}", e)))?,
        temporal: BiTemporalInterval::new(valid_time, tx_time),
        label: GLOBAL_INTERNER
            .intern(&serializable.label)
            .map_err(|e| StorageError::corruption(format!("Failed to intern label: {}", e)))?,
        data: decode_version_data(serializable.data)?,
        next_version: serializable
            .next_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid next version ID: {}", e)))?,
        prev_version: serializable
            .prev_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid prev version ID: {}", e)))?,
    })
}

/// Encode an EdgeVersion to bytes for storage.
///
/// This function is public for use by all cold storage implementations.
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
    };

    bitcode::encode(&serializable)
}

/// Decode an EdgeVersion from bytes.
///
/// This function is public for use by all cold storage implementations.
pub fn decode_edge_version(data: &[u8]) -> Result<EdgeVersion> {
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::temporal::{BiTemporalInterval, TimeRange};

    let serializable: SerializableEdgeVersion = bitcode::decode(data)
        .map_err(|e| StorageError::corruption(format!("Failed to decode edge version: {}", e)))?;

    let valid_time = TimeRange::new(
        HybridTimestamp::new_unchecked(serializable.temporal_valid_start, 0),
        HybridTimestamp::new_unchecked(serializable.temporal_valid_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid valid time range: {}", e)))?;
    let tx_time = TimeRange::new(
        HybridTimestamp::new_unchecked(serializable.temporal_tx_start, 0),
        HybridTimestamp::new_unchecked(serializable.temporal_tx_end, 0),
    )
    .map_err(|e| StorageError::corruption(format!("Invalid transaction time range: {}", e)))?;

    Ok(EdgeVersion {
        id: VersionId::new(serializable.id)
            .map_err(|e| StorageError::corruption(format!("Invalid version ID: {}", e)))?,
        edge_id: EdgeId::new(serializable.edge_id)
            .map_err(|e| StorageError::corruption(format!("Invalid edge ID: {}", e)))?,
        temporal: BiTemporalInterval::new(valid_time, tx_time),
        label: GLOBAL_INTERNER
            .intern(&serializable.label)
            .map_err(|e| StorageError::corruption(format!("Failed to intern label: {}", e)))?,
        source: NodeId::new(serializable.source)
            .map_err(|e| StorageError::corruption(format!("Invalid source ID: {}", e)))?,
        target: NodeId::new(serializable.target)
            .map_err(|e| StorageError::corruption(format!("Invalid target ID: {}", e)))?,
        data: decode_version_data(serializable.data)?,
        next_version: serializable
            .next_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid next version ID: {}", e)))?,
        prev_version: serializable
            .prev_version
            .map(VersionId::new)
            .transpose()
            .map_err(|e| StorageError::corruption(format!("Invalid prev version ID: {}", e)))?,
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
