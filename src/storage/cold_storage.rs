//! Cold storage backend for tiered storage architecture.
//!
//! This module implements the cold tier of the tiered storage system, providing
//! disk-based storage for historical versions that have been migrated from the
//! hot tier (in-memory).
//!
//! # Architecture
//!
//! The cold storage system uses Redb, a pure Rust embedded database, to implement
//! ADR-0025 (Redb Cold Storage and LSN-Based WAL Truncation):
//! - **node_versions table**: Stores compressed NodeVersion data keyed by VersionId
//! - **edge_versions table**: Stores compressed EdgeVersion data keyed by VersionId
//! - **metadata table**: Stores flushed_lsn for WAL truncation coordination
//!
//! # Performance Targets (Issue #120)
//!
//! - Read latency: <1ms (p50), <10ms (p99)
//! - Write throughput: >10k versions/sec
//! - Compression ratio: >3x
//!
//! # Compression
//!
//! Supports two compression algorithms:
//! - **Zstd**: Higher compression ratio (ratio-optimized)
//! - **LZ4**: Faster compression/decompression (speed-optimized)

use crate::core::id::VersionId;
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::storage::wal::LSN;
use crate::utils::error::{Result, StorageError};
use redb::{ReadableDatabase, ReadableTable};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::atomic::AtomicBool;

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

// Table definitions with static lifetimes
const NODE_VERSIONS_TABLE: redb::TableDefinition<'static, u64, &'static [u8]> =
    redb::TableDefinition::new("node_versions");
const EDGE_VERSIONS_TABLE: redb::TableDefinition<'static, u64, &'static [u8]> =
    redb::TableDefinition::new("edge_versions");
const METADATA_TABLE: redb::TableDefinition<'static, &'static str, &'static [u8]> =
    redb::TableDefinition::new("metadata");

/// Metadata keys stored in the metadata table.
const FLUSHED_LSN_KEY: &str = "flushed_lsn";

// ============================================================================
// Error Handling Helpers
// ============================================================================

#[inline]
fn map_io_error(context: &str) -> impl Fn(std::io::Error) -> crate::utils::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_db_error(context: &str) -> impl Fn(redb::DatabaseError) -> crate::utils::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_table_error(context: &str) -> impl Fn(redb::TableError) -> crate::utils::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_commit_error(
    context: &str,
) -> impl Fn(redb::CommitError) -> crate::utils::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_transaction_error(
    context: &str,
) -> impl Fn(redb::TransactionError) -> crate::utils::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_storage_error(
    context: &str,
) -> impl Fn(redb::StorageError) -> crate::utils::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

#[inline]
fn map_compaction_error(
    context: &str,
) -> impl Fn(redb::CompactionError) -> crate::utils::error::Error + '_ {
    move |e| StorageError::io_error(format!("{}: {}", context, e)).into()
}

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
    /// Note: Redb handles durability, so this flag might be advisory or unused by Redb itself
    /// but kept for compatibility.
    pub sync_writes: bool,

    /// Maximum number of versions to batch in a single write operation.
    /// Higher values improve throughput but increase memory usage.
    pub batch_size: usize,

    /// Enable CRC32 checksums for data integrity verification.
    pub enable_checksums: bool,

    /// Cache size in bytes for Redb (0 = use default).
    pub cache_size_bytes: usize,
}

impl Default for ColdStorageConfig {
    fn default() -> Self {
        Self {
            compression: CompressionAlgorithm::Zstd,
            sync_writes: true,
            batch_size: 1000,
            enable_checksums: true,
            cache_size_bytes: 0,
        }
    }
}

impl ColdStorageConfig {
    /// Create a new configuration.
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
}

/// Alias for backwards compatibility during refactor.
pub type RedbConfig = ColdStorageConfig;

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
    /// Total number of node versions stored (atomic).
    pub node_versions_stored: AtomicU64,
    /// Total number of edge versions stored (atomic).
    pub edge_versions_stored: AtomicU64,
    /// Total number of node version reads (atomic).
    pub node_version_reads: AtomicU64,
    /// Total number of edge version reads (atomic).
    pub edge_version_reads: AtomicU64,
    /// Total bytes written (before compression, atomic).
    pub bytes_written_raw: AtomicU64,
    /// Total bytes written (after compression, atomic).
    pub bytes_written_compressed: AtomicU64,
    /// Total bytes read (compressed size from storage, atomic).
    pub bytes_read_compressed: AtomicU64,
    /// Total bytes read (after decompression, atomic).
    pub bytes_read_decompressed: AtomicU64,
    /// Number of read errors (atomic).
    pub read_errors: AtomicU64,
    /// Number of write errors (atomic).
    pub write_errors: AtomicU64,
}

impl AtomicColdStorageStats {
    /// Create a new atomic stats tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert to non-atomic stats for reporting.
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

/// Redb-based cold storage implementation.
///
/// This implementation uses Redb for durable, crash-safe storage of historical
/// versions. It supports LSN tracking for WAL truncation coordination.
pub struct RedbColdStorage {
    /// Path to the database file.
    path: PathBuf,
    /// Redb database instance.
    db: redb::Database,
    /// Configuration.
    config: ColdStorageConfig,
    /// Statistics tracker.
    stats: AtomicColdStorageStats,
    /// Test-only flag to simulate write failures.
    #[cfg(test)]
    fail_writes: AtomicBool,
}

impl RedbColdStorage {
    /// Create a new Redb cold storage at the given path.
    pub fn new<P: AsRef<Path>>(path: P, config: ColdStorageConfig) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(map_io_error("Failed to create directory"))?;
        }

        // Open or create the database
        let db =
            redb::Database::create(&path).map_err(map_db_error("Failed to open Redb database"))?;

        // Initialize tables
        let write_txn = db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

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
        })
    }

    /// Create with default configuration.
    pub fn with_default_config<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::new(path, ColdStorageConfig::default())
    }

    /// Get the database file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Compress data using the configured algorithm.
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        crate::storage::compression::compress(data, &self.config)
    }

    /// Decompress data using the configured algorithm.
    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        crate::storage::compression::decompress(data, &self.config)
    }

    // ========================================================================
    // Testing Helpers
    // ========================================================================

    /// Set a flag to simulate write failures (tests only).
    #[cfg(test)]
    pub fn set_fail_writes(&self, fail: bool) {
        self.fail_writes.store(fail, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn check_fail_writes(&self) -> Result<()> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(StorageError::io_error("Simulated write failure").into());
        }
        Ok(())
    }

    /// Create a new Redb cold storage backed by a temporary file (tests only).
    #[cfg(test)]
    pub fn new_temp() -> Result<(Self, tempfile::TempDir)> {
        let temp_dir = tempfile::tempdir().map_err(map_io_error("Failed to create temp dir"))?;
        let db_path = temp_dir.path().join("test.redb");
        let storage = Self::with_default_config(&db_path)?;
        Ok((storage, temp_dir))
    }

    // ========================================================================
    // CRUD Operations (Formerly ColdStorage Trait)
    // ========================================================================

    /// Store a node version to cold storage.
    pub fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        #[cfg(test)]
        self.check_fail_writes()?;

        let encoded = encode_node_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        {
            let mut table = write_txn.open_table(NODE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open node_versions table: {}", e))
                        .into()
                },
            )?;

            table
                .insert(version.id.as_u64(), compressed.as_slice())
                .map_err(map_storage_error("Failed to store node version"))?;
        }

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit"))?;

        self.stats
            .node_versions_stored
            .fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(raw_size as u64, Ordering::Relaxed);
        self.stats
            .bytes_written_compressed
            .fetch_add(compressed_size as u64, Ordering::Relaxed);

        Ok(())
    }

    /// Retrieve a node version from cold storage.
    pub fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>> {
        self.stats
            .node_version_reads
            .fetch_add(1, Ordering::Relaxed);

        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = read_txn.open_table(NODE_VERSIONS_TABLE).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to open node_versions table: {}", e)).into()
            },
        )?;

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

                let version = decode_node_version(&decompressed)?;
                Ok(Some(version))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                Err(StorageError::io_error(format!("Failed to read node version: {}", e)).into())
            }
        }
    }

    /// Retrieve multiple node versions in a batch.
    pub fn get_node_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<NodeVersion>>> {
        // Simple implementation - can be optimized if needed
        ids.iter().map(|id| self.get_node_version(*id)).collect()
    }

    /// Store an edge version to cold storage.
    pub fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        #[cfg(test)]
        self.check_fail_writes()?;

        let encoded = encode_edge_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        {
            let mut table = write_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open edge_versions table: {}", e))
                        .into()
                },
            )?;

            table
                .insert(version.id.as_u64(), compressed.as_slice())
                .map_err(map_storage_error("Failed to store edge version"))?;
        }

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit"))?;

        self.stats
            .edge_versions_stored
            .fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(raw_size as u64, Ordering::Relaxed);
        self.stats
            .bytes_written_compressed
            .fetch_add(compressed_size as u64, Ordering::Relaxed);

        Ok(())
    }

    /// Retrieve an edge version from cold storage.
    pub fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>> {
        self.stats
            .edge_version_reads
            .fetch_add(1, Ordering::Relaxed);

        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = read_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to open edge_versions table: {}", e)).into()
            },
        )?;

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

                let version = decode_edge_version(&decompressed)?;
                Ok(Some(version))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                Err(StorageError::io_error(format!("Failed to read edge version: {}", e)).into())
            }
        }
    }

    /// Retrieve multiple edge versions in a batch.
    pub fn get_edge_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<EdgeVersion>>> {
        ids.iter().map(|id| self.get_edge_version(*id)).collect()
    }

    /// Check if a node version exists in cold storage.
    pub fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = read_txn.open_table(NODE_VERSIONS_TABLE).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to open node_versions table: {}", e)).into()
            },
        )?;

        match table.get(id.as_u64()) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => {
                Err(StorageError::io_error(format!("Failed to check node version: {}", e)).into())
            }
        }
    }

    /// Check if an edge version exists in cold storage.
    pub fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(map_transaction_error("Failed to begin read transaction"))?;

        let table = read_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to open edge_versions table: {}", e)).into()
            },
        )?;

        match table.get(id.as_u64()) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => {
                Err(StorageError::io_error(format!("Failed to check edge version: {}", e)).into())
            }
        }
    }

    /// Delete a node version from cold storage.
    pub fn delete_node_version(&self, id: VersionId) -> Result<bool> {
        #[cfg(test)]
        self.check_fail_writes()?;

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        let deleted = {
            let mut table = write_txn.open_table(NODE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open node_versions table: {}", e))
                        .into()
                },
            )?;

            match table.remove(id.as_u64()) {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(e) => {
                    return Err(map_storage_error("Failed to delete node version")(e));
                }
            }
        };

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit"))?;

        Ok(deleted)
    }

    /// Delete an edge version from cold storage.
    pub fn delete_edge_version(&self, id: VersionId) -> Result<bool> {
        #[cfg(test)]
        self.check_fail_writes()?;

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        let deleted = {
            let mut table = write_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open edge_versions table: {}", e))
                        .into()
                },
            )?;

            match table.remove(id.as_u64()) {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(e) => {
                    return Err(map_storage_error("Failed to delete edge version")(e));
                }
            }
        };

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit"))?;

        Ok(deleted)
    }

    /// Store multiple node versions in a batch.
    pub fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
        #[cfg(test)]
        self.check_fail_writes()?;

        if versions.is_empty() {
            return Ok(());
        }

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        {
            let mut table = write_txn.open_table(NODE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open node_versions table: {}", e))
                        .into()
                },
            )?;

            for version in versions {
                let encoded = encode_node_version(version);
                let raw_size = encoded.len();
                let compressed = self.compress(&encoded)?;
                let compressed_size = compressed.len();

                table
                    .insert(version.id.as_u64(), compressed.as_slice())
                    .map_err(map_storage_error("Failed to store node version"))?;

                self.stats
                    .node_versions_stored
                    .fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written_raw
                    .fetch_add(raw_size as u64, Ordering::Relaxed);
                self.stats
                    .bytes_written_compressed
                    .fetch_add(compressed_size as u64, Ordering::Relaxed);
            }
        }

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit batch"))?;

        Ok(())
    }

    /// Store multiple edge versions in a batch.
    pub fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
        #[cfg(test)]
        self.check_fail_writes()?;

        if versions.is_empty() {
            return Ok(());
        }

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        {
            let mut table = write_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open edge_versions table: {}", e))
                        .into()
                },
            )?;

            for version in versions {
                let encoded = encode_edge_version(version);
                let raw_size = encoded.len();
                let compressed = self.compress(&encoded)?;
                let compressed_size = compressed.len();

                table
                    .insert(version.id.as_u64(), compressed.as_slice())
                    .map_err(map_storage_error("Failed to store edge version"))?;

                self.stats
                    .edge_versions_stored
                    .fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written_raw
                    .fetch_add(raw_size as u64, Ordering::Relaxed);
                self.stats
                    .bytes_written_compressed
                    .fetch_add(compressed_size as u64, Ordering::Relaxed);
            }
        }

        write_txn
            .commit()
            .map_err(map_commit_error("Failed to commit batch"))?;

        Ok(())
    }

    /// Get statistics about cold storage.
    pub fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    /// Flush any buffered writes to disk.
    pub fn flush(&self) -> Result<()> {
        // Redb automatically flushes on commit, nothing extra needed
        Ok(())
    }

    /// Close the cold storage, flushing any pending writes.
    pub fn close(&self) -> Result<()> {
        // Redb handles cleanup on drop
        Ok(())
    }

    // ========================================================================
    // LSN Tracking (ADR-0025)
    // ========================================================================

    /// Get the highest LSN that has been durably flushed to cold storage.
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
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to write flushed_lsn: {}", e)).into()
            })?;
        Ok(())
    }

    /// Store a batch of versions with LSN tracking.
    pub fn store_batch_with_lsn(
        &self,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        lsn: LSN,
    ) -> Result<()> {
        #[cfg(test)]
        self.check_fail_writes()?;

        let write_txn = self
            .db
            .begin_write()
            .map_err(map_transaction_error("Failed to begin write transaction"))?;

        // Store node versions
        {
            let mut table = write_txn
                .open_table(NODE_VERSIONS_TABLE)
                .map_err(map_table_error("Failed to open node_versions table"))?;

            for version in nodes {
                let encoded = encode_node_version(version);
                let raw_size = encoded.len();
                let compressed = self.compress(&encoded)?;
                let compressed_size = compressed.len();

                table
                    .insert(version.id.as_u64(), compressed.as_slice())
                    .map_err(map_storage_error("Failed to store node version"))?;

                self.stats
                    .node_versions_stored
                    .fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written_raw
                    .fetch_add(raw_size as u64, Ordering::Relaxed);
                self.stats
                    .bytes_written_compressed
                    .fetch_add(compressed_size as u64, Ordering::Relaxed);
            }
        }

        // Store edge versions
        {
            let mut table = write_txn
                .open_table(EDGE_VERSIONS_TABLE)
                .map_err(map_table_error("Failed to open edge_versions table"))?;

            for version in edges {
                let encoded = encode_edge_version(version);
                let raw_size = encoded.len();
                let compressed = self.compress(&encoded)?;
                let compressed_size = compressed.len();

                table
                    .insert(version.id.as_u64(), compressed.as_slice())
                    .map_err(map_storage_error("Failed to store edge version"))?;

                self.stats
                    .edge_versions_stored
                    .fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_written_raw
                    .fetch_add(raw_size as u64, Ordering::Relaxed);
                self.stats
                    .bytes_written_compressed
                    .fetch_add(compressed_size as u64, Ordering::Relaxed);
            }
        }

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

        Ok(())
    }

    /// Compact the database to reclaim space.
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
#[derive(bitcode::Encode, bitcode::Decode)]
enum SerializablePropertyValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<u8>),
    Vector(Vec<f32>),
    SparseVector {
        dimension: usize,
        indices: Vec<u32>,
        values: Vec<f32>,
    },
}

/// Encode a NodeVersion to bytes for storage.
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
            .resolve(version.label)
            .unwrap_or_default()
            .to_string(),
        data: encode_version_data(&version.data),
        next_version: version.next_version.map(|v| v.as_u64()),
        prev_version: version.prev_version.map(|v| v.as_u64()),
    };

    bitcode::encode(&serializable)
}

/// Decode a NodeVersion from bytes.
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
            .resolve(version.label)
            .unwrap_or_default()
            .to_string(),
        source: version.source.as_u64(),
        target: version.target.as_u64(),
        data: encode_version_data(&version.data),
        next_version: version.next_version.map(|v| v.as_u64()),
        prev_version: version.prev_version.map(|v| v.as_u64()),
    };

    bitcode::encode(&serializable)
}

/// Decode an EdgeVersion from bytes.
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
                        GLOBAL_INTERNER.resolve(*k).unwrap_or_default().to_string(),
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
                        GLOBAL_INTERNER.resolve(*k).unwrap_or_default().to_string(),
                        encode_property_value(v),
                    )
                })
                .collect(),
            removed: delta
                .removed
                .iter()
                .map(|k| GLOBAL_INTERNER.resolve(*k).unwrap_or_default().to_string())
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
mod tests {
    use super::*;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::BiTemporalInterval;

    fn create_test_node_version(id: u64) -> NodeVersion {
        let properties = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        NodeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
        )
    }

    fn create_test_edge_version(id: u64) -> EdgeVersion {
        let properties = PropertyMapBuilder::new().insert("weight", 1.5f64).build();

        EdgeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            EdgeId::new(200).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            properties,
        )
    }

    #[test]
    fn test_redb_store_and_retrieve_node_version() {
        let (storage, _temp_dir) = RedbColdStorage::new_temp().unwrap();
        let version = create_test_node_version(1);

        storage.store_node_version(&version).unwrap();
        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();

        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.node_id, version.node_id);
    }

    #[test]
    fn test_redb_store_and_retrieve_edge_version() {
        let (storage, _temp_dir) = RedbColdStorage::new_temp().unwrap();
        let version = create_test_edge_version(1);

        storage.store_edge_version(&version).unwrap();
        let retrieved = storage.get_edge_version(version.id).unwrap().unwrap();

        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.edge_id, version.edge_id);
        assert_eq!(retrieved.source, version.source);
        assert_eq!(retrieved.target, version.target);
    }

    #[test]
    fn test_redb_get_nonexistent_returns_none() {
        let (storage, _temp_dir) = RedbColdStorage::new_temp().unwrap();

        let result = storage
            .get_node_version(VersionId::new(999).unwrap())
            .unwrap();
        assert!(result.is_none());

        let result = storage
            .get_edge_version(VersionId::new(999).unwrap())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_redb_delete_node_version() {
        let (storage, _temp_dir) = RedbColdStorage::new_temp().unwrap();
        let version = create_test_node_version(1);

        storage.store_node_version(&version).unwrap();
        assert!(storage.contains_node_version(version.id).unwrap());

        let deleted = storage.delete_node_version(version.id).unwrap();
        assert!(deleted);

        assert!(!storage.contains_node_version(version.id).unwrap());

        // Deleting again should return false
        let deleted_again = storage.delete_node_version(version.id).unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn test_redb_batch_store_atomic() {
        let (storage, _temp_dir) = RedbColdStorage::new_temp().unwrap();

        let node_versions: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();
        let edge_versions: Vec<EdgeVersion> = (1..=5).map(create_test_edge_version).collect();

        // Store batch with LSN
        storage
            .store_batch_with_lsn(&node_versions, &edge_versions, LSN(100))
            .unwrap();

        // Verify all nodes stored
        for version in &node_versions {
            let retrieved = storage.get_node_version(version.id).unwrap();
            assert!(retrieved.is_some());
        }

        // Verify LSN was stored
        let flushed_lsn = storage.get_flushed_lsn().unwrap();
        assert_eq!(flushed_lsn, Some(LSN(100)));
    }

    #[test]
    fn test_fail_writes() {
        let (storage, _temp_dir) = RedbColdStorage::new_temp().unwrap();
        storage.set_fail_writes(true);

        let version = create_test_node_version(1);
        let result = storage.store_node_version(&version);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Simulated write failure"));
    }
}
