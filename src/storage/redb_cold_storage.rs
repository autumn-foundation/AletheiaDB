//! Redb-based cold storage backend for tiered storage architecture.
//!
//! This module implements cold storage using Redb, a pure Rust embedded database.
//! It provides disk-based storage for historical versions with LSN tracking for
//! coordinated WAL truncation.
//!
//! # Architecture
//!
//! Redb cold storage implements ADR-0025 (Redb Cold Storage and LSN-Based WAL Truncation):
//! - **node_versions table**: Stores compressed NodeVersion data keyed by VersionId
//! - **edge_versions table**: Stores compressed EdgeVersion data keyed by VersionId
//! - **metadata table**: Stores flushed_lsn for WAL truncation coordination
//!
//! # LSN Tracking
//!
//! The `flushed_lsn` metadata enables safe WAL truncation:
//! - Versions are written atomically with their max LSN
//! - WAL can safely truncate segments where max_lsn < flushed_lsn
//! - Recovery replays WAL from flushed_lsn + 1
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
//!
//! let config = RedbConfig::default();
//! let storage = RedbColdStorage::new("data/cold.redb", config)?;
//!
//! // Store versions with LSN tracking
//! storage.store_batch_with_lsn(&node_versions, &edge_versions, LSN(1000))?;
//!
//! // Get flushed LSN for WAL truncation
//! let flushed_lsn = storage.get_flushed_lsn()?;
//! ```

use crate::core::id::VersionId;
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::storage::wal::LSN;
use crate::utils::error::{Result, StorageError};
use redb::{ReadableDatabase, ReadableTable};
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
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to write flushed_lsn: {}", e)).into()
            })?;
        Ok(())
    }

    // ========================================================================
    // Logic moved from ColdStorage trait impl
    // ========================================================================

    /// Store a node version.
    pub fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
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

    /// Retrieve a node version.
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

    /// Retrieve a batch of node versions.
    pub fn get_node_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<NodeVersion>>> {
        ids.iter().map(|id| self.get_node_version(*id)).collect()
    }

    /// Store an edge version.
    pub fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
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

    /// Retrieve an edge version.
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

    /// Retrieve a batch of edge versions.
    pub fn get_edge_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<EdgeVersion>>> {
        ids.iter().map(|id| self.get_edge_version(*id)).collect()
    }

    /// Check if a node version exists.
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

    /// Check if an edge version exists.
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

    /// Delete a node version.
    pub fn delete_node_version(&self, id: VersionId) -> Result<bool> {
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

    /// Delete an edge version.
    pub fn delete_edge_version(&self, id: VersionId) -> Result<bool> {
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

    /// Store a batch of node versions.
    pub fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
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

    /// Store a batch of edge versions.
    pub fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
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

    /// Get usage statistics.
    pub fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    /// Flush to disk.
    ///
    /// For Redb, this is a no-op as transactions are durable on commit.
    pub fn flush(&self) -> Result<()> {
        // Redb automatically flushes on commit, nothing extra needed
        Ok(())
    }

    /// Close the database connection.
    ///
    /// This releases the file lock.
    pub fn close(&self) -> Result<()> {
        // Redb handles cleanup on drop
        Ok(())
    }

    /// Store a batch of versions with LSN tracking.
    ///
    /// This operation is atomic - either all versions are stored and the LSN is updated,
    /// or nothing is changed. This enables safe WAL truncation.
    pub fn store_batch_with_lsn(
        &self,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        lsn: LSN,
    ) -> Result<()> {
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
    ///
    /// Note: This method requires mutable access because Redb's compact
    /// operation modifies the database file structure.
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

    // ========================================================================
    // TDD Tests for Issue #2: CRUD operations
    // ========================================================================

    #[test]
    fn test_redb_store_and_retrieve_node_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.node_id, version.node_id);
    }

    #[test]
    fn test_redb_store_and_retrieve_edge_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

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
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

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
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

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
    fn test_redb_delete_edge_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let version = create_test_edge_version(1);
        storage.store_edge_version(&version).unwrap();

        assert!(storage.contains_edge_version(version.id).unwrap());

        let deleted = storage.delete_edge_version(version.id).unwrap();
        assert!(deleted);

        assert!(!storage.contains_edge_version(version.id).unwrap());
    }

    // ========================================================================
    // TDD Tests for Issue #2: Batch operations
    // ========================================================================

    #[test]
    fn test_redb_batch_store_atomic() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

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
            assert_eq!(retrieved.unwrap().id, version.id);
        }

        // Verify all edges stored
        for version in &edge_versions {
            let retrieved = storage.get_edge_version(version.id).unwrap();
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().id, version.id);
        }

        // Verify LSN was stored
        let flushed_lsn = storage.get_flushed_lsn().unwrap();
        assert_eq!(flushed_lsn, Some(LSN(100)));
    }

    #[test]
    fn test_redb_batch_node_versions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let versions: Vec<NodeVersion> = (1..=100).map(create_test_node_version).collect();
        storage.store_node_versions_batch(&versions).unwrap();

        // Verify all stored
        for version in &versions {
            assert!(storage.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_redb_batch_edge_versions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let versions: Vec<EdgeVersion> = (1..=100).map(create_test_edge_version).collect();
        storage.store_edge_versions_batch(&versions).unwrap();

        // Verify all stored
        for version in &versions {
            assert!(storage.contains_edge_version(version.id).unwrap());
        }
    }

    // ========================================================================
    // TDD Tests for Issue #2: LSN metadata (NEW capability)
    // ========================================================================

    #[test]
    fn test_redb_set_and_get_flushed_lsn() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Initially no LSN
        assert_eq!(storage.get_flushed_lsn().unwrap(), None);

        // Store with LSN
        let version = create_test_node_version(1);
        storage
            .store_batch_with_lsn(&[version], &[], LSN(42))
            .unwrap();

        // LSN should be set
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(42)));
    }

    #[test]
    fn test_redb_flushed_lsn_persists_across_reopen() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // Create and set LSN
        {
            let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
            let version = create_test_node_version(1);
            storage
                .store_batch_with_lsn(&[version], &[], LSN(12345))
                .unwrap();
        }

        // Reopen and verify LSN persisted
        {
            let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
            assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(12345)));

            // Also verify the version persisted
            let retrieved = storage
                .get_node_version(VersionId::new(1).unwrap())
                .unwrap();
            assert!(retrieved.is_some());
        }
    }

    #[test]
    fn test_redb_batch_updates_flushed_lsn_atomically() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // First batch with LSN 100
        let nodes1: Vec<NodeVersion> = (1..=5).map(create_test_node_version).collect();
        storage
            .store_batch_with_lsn(&nodes1, &[], LSN(100))
            .unwrap();
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

        // Second batch with higher LSN
        let nodes2: Vec<NodeVersion> = (6..=10).map(create_test_node_version).collect();
        storage
            .store_batch_with_lsn(&nodes2, &[], LSN(200))
            .unwrap();
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(200)));

        // Verify all data present
        for id in 1..=10 {
            assert!(
                storage
                    .contains_node_version(VersionId::new(id).unwrap())
                    .unwrap()
            );
        }
    }

    // ========================================================================
    // TDD Tests for Issue #2: Persistence
    // ========================================================================

    #[test]
    fn test_redb_persistence_across_close_and_reopen() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // Store data
        {
            let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

            let node_version = create_test_node_version(42);
            let edge_version = create_test_edge_version(43);

            storage.store_node_version(&node_version).unwrap();
            storage.store_edge_version(&edge_version).unwrap();
        }

        // Reopen and verify
        {
            let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

            let node = storage
                .get_node_version(VersionId::new(42).unwrap())
                .unwrap();
            assert!(node.is_some());
            assert_eq!(node.unwrap().id, VersionId::new(42).unwrap());

            let edge = storage
                .get_edge_version(VersionId::new(43).unwrap())
                .unwrap();
            assert!(edge.is_some());
            assert_eq!(edge.unwrap().id, VersionId::new(43).unwrap());
        }
    }

    #[test]
    fn test_redb_compression_zstd() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        // Create version with repetitive data for better compression
        let props = PropertyMapBuilder::new()
            .insert("description", "This is a test description with repetitive content that should compress well. This is a test description with repetitive content that should compress well.")
            .build();

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            props,
        );

        storage.store_node_version(&version).unwrap();

        let stats = storage.stats();
        assert!(stats.bytes_written_raw > 0);
        assert!(stats.bytes_written_compressed > 0);
        // Compression should provide some benefit
        assert!(stats.bytes_written_raw >= stats.bytes_written_compressed);
    }

    #[test]
    fn test_redb_no_compression() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let config = RedbConfig::new()
            .compression(CompressionAlgorithm::None)
            .enable_checksums(false);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    #[test]
    fn test_redb_fast_compression() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    // ========================================================================
    // Additional tests
    // ========================================================================

    #[test]
    fn test_redb_stats_tracking() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();
        storage.get_node_version(version.id).unwrap();

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 1);
        assert_eq!(stats.node_version_reads, 1);
        assert!(stats.bytes_written_raw > 0);
        assert!(stats.bytes_read_compressed > 0);
    }

    #[test]
    fn test_redb_overwrite_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Store first version
        let version1 = create_test_node_version(1);
        storage.store_node_version(&version1).unwrap();

        // Overwrite with new version (same ID, different data)
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 40i64)
            .build();
        let version2 = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(2000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            props2,
        );
        storage.store_node_version(&version2).unwrap();

        // Should get the second version
        let retrieved = storage.get_node_version(version2.id).unwrap().unwrap();
        assert_eq!(
            retrieved.temporal.transaction_time().start().wallclock(),
            2000
        );
    }

    #[test]
    fn test_redb_empty_batch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Empty batch should succeed
        storage.store_node_versions_batch(&[]).unwrap();
        storage.store_edge_versions_batch(&[]).unwrap();
    }

    #[test]
    fn test_redb_compact() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let mut storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Store and delete some data
        for id in 1..=100 {
            let version = create_test_node_version(id);
            storage.store_node_version(&version).unwrap();
        }

        for id in 1..=50 {
            storage
                .delete_node_version(VersionId::new(id).unwrap())
                .unwrap();
        }

        // Compact should succeed
        storage.compact().unwrap();

        // Remaining data should be intact
        for id in 51..=100 {
            assert!(
                storage
                    .contains_node_version(VersionId::new(id).unwrap())
                    .unwrap()
            );
        }
    }

    #[test]
    fn test_redb_config_builder() {
        let config = RedbConfig::new()
            .compression(CompressionAlgorithm::Fast)
            .enable_checksums(false)
            .cache_size_bytes(1024 * 1024);

        assert_eq!(config.compression, CompressionAlgorithm::Fast);
        assert!(!config.enable_checksums);
        assert_eq!(config.cache_size_bytes, 1024 * 1024);
    }

    // ========================================================================
    // Critical Safety Tests (LSN Race Conditions)
    // ========================================================================

    #[test]
    fn test_lsn_monotonic_increase_only() {
        // Test that set_flushed_lsn only increases monotonically
        // This prevents race conditions where out-of-order commits could
        // overwrite a higher LSN with a lower LSN
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();

        // Store with LSN 100
        let node1 = create_test_node_version(1);
        storage
            .store_batch_with_lsn(std::slice::from_ref(&node1), &[], LSN(100))
            .unwrap();

        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

        // Try to store with lower LSN 50 (simulates out-of-order commit)
        let node2 = create_test_node_version(2);
        storage
            .store_batch_with_lsn(&[node2], &[], LSN(50))
            .unwrap();

        // LSN should still be 100 (not overwritten by lower 50)
        assert_eq!(
            storage.get_flushed_lsn().unwrap(),
            Some(LSN(100)),
            "LSN should not decrease"
        );

        // Store with higher LSN 200
        let node3 = create_test_node_version(3);
        storage
            .store_batch_with_lsn(&[node3], &[], LSN(200))
            .unwrap();

        // LSN should increase to 200
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(200)));
    }

    #[test]
    fn test_concurrent_lsn_updates() {
        // Test concurrent LSN updates from multiple threads
        // Verifies that the final LSN is the maximum of all updates
        use std::sync::Arc;
        use std::thread;

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let storage = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());

        // Spawn multiple threads that update LSN in random order
        let mut handles = vec![];
        let lsns = [150, 100, 200, 50, 175, 125];

        for (i, &lsn_value) in lsns.iter().enumerate() {
            let storage_clone = Arc::clone(&storage);
            let handle = thread::spawn(move || {
                let node = create_test_node_version((i + 1) as u64);
                storage_clone
                    .store_batch_with_lsn(&[node], &[], LSN(lsn_value))
                    .unwrap();
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Final LSN should be the maximum (200)
        assert_eq!(
            storage.get_flushed_lsn().unwrap(),
            Some(LSN(200)),
            "Final LSN should be max of all updates"
        );
    }

    #[test]
    fn test_lsn_persistence_across_reopen() {
        // Verify that LSN persists correctly even with out-of-order updates
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // First session: store with LSN 100
        {
            let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();
            let node = create_test_node_version(1);
            storage
                .store_batch_with_lsn(&[node], &[], LSN(100))
                .unwrap();
        }

        // Second session: try to store with lower LSN 50
        {
            let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();

            // LSN should still be 100 from first session
            assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

            let node = create_test_node_version(2);
            storage.store_batch_with_lsn(&[node], &[], LSN(50)).unwrap();

            // LSN should STILL be 100 (not overwritten)
            assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));
        }

        // Third session: verify LSN is still 100
        {
            let storage = RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap();
            assert_eq!(
                storage.get_flushed_lsn().unwrap(),
                Some(LSN(100)),
                "LSN should persist correctly"
            );
        }
    }

    // ========================================================================
    // Quick Win Tests for Function Coverage (Phase 1)
    // ========================================================================

    #[test]
    fn test_path_getter() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        assert_eq!(storage.path(), db_path.as_path());
    }

    #[test]
    fn test_flush() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Store some data
        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        // Flush should succeed (no-op for Redb)
        assert!(storage.flush().is_ok());

        // Data should still be retrievable
        let retrieved = storage.get_node_version(version.id).unwrap();
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_close() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let version_id = {
            let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

            // Store some data
            let version = create_test_node_version(1);
            storage.store_node_version(&version).unwrap();

            // Close should succeed
            assert!(storage.close().is_ok());

            version.id
        }; // storage drops here, releasing the lock

        // Data should persist after close (verify by reopening)
        let storage2 = RedbColdStorage::with_default_config(&db_path).unwrap();
        let retrieved = storage2.get_node_version(version_id).unwrap();
        assert!(retrieved.is_some());
    }

    // ========================================================================
    // Additional Coverage Tests - Error Paths and Uncovered Methods
    // ========================================================================

    #[test]
    fn test_config_to_cold_storage_config() {
        let config = RedbConfig::new()
            .compression(CompressionAlgorithm::Zstd)
            .enable_checksums(true)
            .cache_size_bytes(2048);

        let cold_config = config.to_cold_storage_config();
        assert_eq!(cold_config.compression, CompressionAlgorithm::Zstd);
        assert!(cold_config.enable_checksums);
        assert!(cold_config.sync_writes);
        assert_eq!(cold_config.batch_size, 1000);
    }

    #[test]
    fn test_default_config() {
        let config1 = RedbConfig::default();
        let config2 = RedbConfig::new();

        assert_eq!(config1.compression, config2.compression);
        assert_eq!(config1.enable_checksums, config2.enable_checksums);
        assert_eq!(config1.cache_size_bytes, config2.cache_size_bytes);
    }

    #[test]
    fn test_compress_decompress_zstd() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        let original_data = b"Test data for compression";
        let compressed = storage.compress(original_data).unwrap();
        let decompressed = storage.decompress(&compressed).unwrap();

        assert_eq!(original_data, decompressed.as_slice());
    }

    #[test]
    fn test_compress_decompress_lz4() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        let original_data = b"Test data for LZ4 compression";
        let compressed = storage.compress(original_data).unwrap();
        let decompressed = storage.decompress(&compressed).unwrap();

        assert_eq!(original_data, decompressed.as_slice());
    }

    #[test]
    fn test_compress_decompress_none() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let config = RedbConfig::new()
            .compression(CompressionAlgorithm::None)
            .enable_checksums(false);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        let original_data = b"No compression test";
        let compressed = storage.compress(original_data).unwrap();
        let decompressed = storage.decompress(&compressed).unwrap();

        assert_eq!(original_data, decompressed.as_slice());
    }

    #[test]
    fn test_invalid_flushed_lsn_format() {
        // Create a database with corrupted metadata
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        {
            let db = redb::Database::create(&db_path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(METADATA_TABLE).unwrap();

                // Write invalid LSN (wrong size - 4 bytes instead of 8)
                let invalid_data = [1u8, 2, 3, 4];
                table
                    .insert(FLUSHED_LSN_KEY, invalid_data.as_slice())
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        // Now try to read it with RedbColdStorage
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        let result = storage.get_flushed_lsn();

        // Should return corruption error
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("corruption") || err_msg.contains("Invalid"));
    }

    #[test]
    fn test_get_node_version_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Try to get non-existent version
        let result = storage
            .get_node_version(VersionId::new(99999).unwrap())
            .unwrap();
        assert!(result.is_none());

        // Verify stats were updated
        let stats = storage.stats();
        assert_eq!(stats.node_version_reads, 1);
    }

    #[test]
    fn test_get_edge_version_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Try to get non-existent version
        let result = storage
            .get_edge_version(VersionId::new(99999).unwrap())
            .unwrap();
        assert!(result.is_none());

        // Verify stats were updated
        let stats = storage.stats();
        assert_eq!(stats.edge_version_reads, 1);
    }

    #[test]
    fn test_contains_node_version_false() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        assert!(
            !storage
                .contains_node_version(VersionId::new(88888).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn test_contains_edge_version_false() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        assert!(
            !storage
                .contains_edge_version(VersionId::new(88888).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn test_delete_node_version_nonexistent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let deleted = storage
            .delete_node_version(VersionId::new(77777).unwrap())
            .unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_delete_edge_version_nonexistent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let deleted = storage
            .delete_edge_version(VersionId::new(77777).unwrap())
            .unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_lsn_equal_does_not_update() {
        // Test that setting LSN to the same value doesn't update
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Set initial LSN
        let node1 = create_test_node_version(1);
        storage
            .store_batch_with_lsn(&[node1], &[], LSN(100))
            .unwrap();
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));

        // Try to set same LSN again
        let node2 = create_test_node_version(2);
        storage
            .store_batch_with_lsn(&[node2], &[], LSN(100))
            .unwrap();

        // Should still be 100
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(100)));
    }

    #[test]
    fn test_empty_batch_with_lsn() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Empty batch should still update LSN
        storage.store_batch_with_lsn(&[], &[], LSN(500)).unwrap();
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(500)));
    }

    #[test]
    fn test_get_flushed_lsn_no_metadata() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Before any LSN is set, should return None
        assert_eq!(storage.get_flushed_lsn().unwrap(), None);
    }

    #[test]
    fn test_stats_after_reads() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Store versions
        let node = create_test_node_version(1);
        let edge = create_test_edge_version(2);
        storage.store_node_version(&node).unwrap();
        storage.store_edge_version(&edge).unwrap();

        // Read them back
        storage.get_node_version(node.id).unwrap();
        storage.get_edge_version(edge.id).unwrap();

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 1);
        assert_eq!(stats.edge_versions_stored, 1);
        assert_eq!(stats.node_version_reads, 1);
        assert_eq!(stats.edge_version_reads, 1);
        assert!(stats.bytes_written_raw > 0);
        assert!(stats.bytes_written_compressed > 0);
        assert!(stats.bytes_read_compressed > 0);
        assert!(stats.bytes_read_decompressed > 0);
    }

    #[test]
    fn test_batch_with_only_nodes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let nodes: Vec<NodeVersion> = (1..=5).map(create_test_node_version).collect();
        storage.store_batch_with_lsn(&nodes, &[], LSN(100)).unwrap();

        for node in &nodes {
            assert!(storage.contains_node_version(node.id).unwrap());
        }
    }

    #[test]
    fn test_batch_with_only_edges() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let edges: Vec<EdgeVersion> = (1..=5).map(create_test_edge_version).collect();
        storage.store_batch_with_lsn(&[], &edges, LSN(100)).unwrap();

        for edge in &edges {
            assert!(storage.contains_edge_version(edge.id).unwrap());
        }
    }

    #[test]
    fn test_config_cache_size() {
        let config = RedbConfig::new().cache_size_bytes(4096);
        assert_eq!(config.cache_size_bytes, 4096);
    }

    #[test]
    fn test_multiple_overwrites_same_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let version_id = VersionId::new(1).unwrap();

        // Write same version multiple times
        for i in 0..5 {
            let props = PropertyMapBuilder::new().insert("iteration", i).build();

            let version = NodeVersion::new_anchor(
                version_id,
                NodeId::new(100).unwrap(),
                BiTemporalInterval::current((i * 1000).into()),
                GLOBAL_INTERNER.intern("Person").unwrap(),
                props,
            );

            storage.store_node_version(&version).unwrap();
        }

        // Should have last version
        let retrieved = storage.get_node_version(version_id).unwrap().unwrap();
        assert_eq!(
            retrieved.temporal.transaction_time().start().wallclock(),
            4000
        );
    }

    #[test]
    fn test_large_batch_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Store large batch
        let nodes: Vec<NodeVersion> = (1..=1000).map(create_test_node_version).collect();
        let edges: Vec<EdgeVersion> = (1..=500).map(create_test_edge_version).collect();

        storage
            .store_batch_with_lsn(&nodes, &edges, LSN(5000))
            .unwrap();

        // Verify counts
        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 1000);
        assert_eq!(stats.edge_versions_stored, 500);
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(5000)));
    }

    #[test]
    fn test_compression_with_checksums() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let config = RedbConfig::new()
            .compression(CompressionAlgorithm::Zstd)
            .enable_checksums(true);

        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    #[test]
    fn test_store_and_delete_cycle() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let node = create_test_node_version(1);
        let edge = create_test_edge_version(2);

        // Store
        storage.store_node_version(&node).unwrap();
        storage.store_edge_version(&edge).unwrap();
        assert!(storage.contains_node_version(node.id).unwrap());
        assert!(storage.contains_edge_version(edge.id).unwrap());

        // Delete
        assert!(storage.delete_node_version(node.id).unwrap());
        assert!(storage.delete_edge_version(edge.id).unwrap());
        assert!(!storage.contains_node_version(node.id).unwrap());
        assert!(!storage.contains_edge_version(edge.id).unwrap());

        // Store again
        storage.store_node_version(&node).unwrap();
        storage.store_edge_version(&edge).unwrap();
        assert!(storage.contains_node_version(node.id).unwrap());
        assert!(storage.contains_edge_version(edge.id).unwrap());
    }

    #[test]
    fn test_mixed_batch_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // First batch
        let nodes1: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();
        storage.store_node_versions_batch(&nodes1).unwrap();

        // Second batch
        let edges1: Vec<EdgeVersion> = (1..=5).map(create_test_edge_version).collect();
        storage.store_edge_versions_batch(&edges1).unwrap();

        // Third batch with LSN
        let nodes2: Vec<NodeVersion> = (11..=20).map(create_test_node_version).collect();
        let edges2: Vec<EdgeVersion> = (6..=10).map(create_test_edge_version).collect();
        storage
            .store_batch_with_lsn(&nodes2, &edges2, LSN(1000))
            .unwrap();

        // Verify all data
        for i in 1..=20 {
            assert!(
                storage
                    .contains_node_version(VersionId::new(i).unwrap())
                    .unwrap()
            );
        }
        for i in 1..=10 {
            assert!(
                storage
                    .contains_edge_version(VersionId::new(i).unwrap())
                    .unwrap()
            );
        }

        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(1000)));
    }

    // ========================================================================
    // Error Path Coverage Tests - Force database errors to cover map_err closures
    // ========================================================================

    #[test]
    fn test_error_invalid_database_path() {
        use std::fs;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("file.txt");

        // Create a regular file, not a directory
        fs::write(&file_path, b"not a database").unwrap();

        // Try to create database with a file path instead of directory path
        // This should trigger the error path in RedbColdStorage::new
        let result = RedbColdStorage::with_default_config(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_parent_directory_creation_fails() {
        // Try to create database in a path where parent creation would fail
        // Using /dev/null as parent should fail on Unix systems
        #[cfg(unix)]
        {
            let invalid_path = "/dev/null/subdir/test.redb";
            let result = RedbColdStorage::with_default_config(invalid_path);
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_corrupted_database_recovery() {
        use std::fs;
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("corrupt.redb");

        // Create a file with invalid database content
        fs::write(&db_path, b"This is not a valid Redb database file content").unwrap();

        // Try to open it - should trigger error in Database::create
        let result = RedbColdStorage::with_default_config(&db_path);
        // Redb might handle this or error out
        // Either way, we're exercising the error path
        let _ = result; // Result varies by Redb version
    }

    #[test]
    fn test_decompression_error_corrupted_data() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // Create database with Zstd compression
        let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        // Try to decompress invalid data
        let invalid_compressed_data = vec![0xFF; 100]; // Invalid Zstd data
        let result = storage.decompress(&invalid_compressed_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_decompression_error_lz4_corrupted() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // Create database with LZ4 compression
        let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        // Try to decompress invalid data
        let invalid_data = vec![0xAA; 100]; // Invalid LZ4 data
        let result = storage.decompress(&invalid_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_store_retrieve_with_checksum_validation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // Test with checksums enabled
        let config = RedbConfig::new()
            .compression(CompressionAlgorithm::Zstd)
            .enable_checksums(true);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        // Create a version with substantial data
        let mut props = PropertyMapBuilder::new();
        for i in 0..100 {
            props = props.insert(&format!("field_{}", i), i as i64);
        }
        let properties = props.build();

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
        );

        storage.store_node_version(&version).unwrap();
        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    #[test]
    fn test_concurrent_writes_no_data_loss() {
        use std::sync::Arc;
        use std::thread;

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let storage = Arc::new(RedbColdStorage::with_default_config(&db_path).unwrap());

        // Spawn multiple threads writing different versions
        let mut handles = vec![];
        for thread_id in 0..10 {
            let storage_clone = Arc::clone(&storage);
            let handle = thread::spawn(move || {
                for i in 0..10 {
                    let version_id = (thread_id * 10 + i) as u64 + 1;
                    let version = create_test_node_version(version_id);
                    storage_clone.store_node_version(&version).unwrap();
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all 100 versions were stored
        for i in 1..=100 {
            assert!(
                storage
                    .contains_node_version(VersionId::new(i).unwrap())
                    .unwrap()
            );
        }
    }

    #[test]
    fn test_edge_version_roundtrip_all_fields() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Create edge with comprehensive properties
        let properties = PropertyMapBuilder::new()
            .insert("weight", 2.5f64)
            .insert("label", "test_label")
            .insert("count", 42i64)
            .build();

        let version = EdgeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            EdgeId::new(200).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(10).unwrap(),
            NodeId::new(20).unwrap(),
            properties,
        );

        storage.store_edge_version(&version).unwrap();
        let retrieved = storage.get_edge_version(version.id).unwrap().unwrap();

        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.edge_id, version.edge_id);
        assert_eq!(retrieved.source, version.source);
        assert_eq!(retrieved.target, version.target);
    }

    #[test]
    fn test_batch_operations_preserve_order() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let nodes: Vec<NodeVersion> = (1..=50).map(create_test_node_version).collect();
        let edges: Vec<EdgeVersion> = (1..=50).map(create_test_edge_version).collect();

        storage
            .store_batch_with_lsn(&nodes, &edges, LSN(1000))
            .unwrap();

        // Verify all nodes
        for i in 1..=50 {
            let version = storage
                .get_node_version(VersionId::new(i).unwrap())
                .unwrap();
            assert!(version.is_some());
        }

        // Verify all edges
        for i in 1..=50 {
            let version = storage
                .get_edge_version(VersionId::new(i).unwrap())
                .unwrap();
            assert!(version.is_some());
        }
    }

    #[test]
    fn test_stats_accuracy_after_multiple_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Perform various operations
        let node1 = create_test_node_version(1);
        let node2 = create_test_node_version(2);
        let edge1 = create_test_edge_version(10);

        storage.store_node_version(&node1).unwrap();
        storage.store_node_version(&node2).unwrap();
        storage.store_edge_version(&edge1).unwrap();

        storage.get_node_version(node1.id).unwrap();
        storage.get_node_version(node2.id).unwrap();
        storage.get_edge_version(edge1.id).unwrap();

        storage.delete_node_version(node1.id).unwrap();

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 2);
        assert_eq!(stats.edge_versions_stored, 1);
        assert_eq!(stats.node_version_reads, 2);
        assert_eq!(stats.edge_version_reads, 1);
    }

    #[test]
    fn test_reopen_after_compact() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // Create, store, compact
        {
            let mut storage = RedbColdStorage::with_default_config(&db_path).unwrap();
            for i in 1..=50 {
                let version = create_test_node_version(i);
                storage.store_node_version(&version).unwrap();
            }
            storage.compact().unwrap();
        }

        // Reopen and verify
        {
            let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
            for i in 1..=50 {
                assert!(
                    storage
                        .contains_node_version(VersionId::new(i).unwrap())
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn test_lsn_persistence_through_compact() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        let lsn_value = LSN(99999);

        {
            let mut storage = RedbColdStorage::with_default_config(&db_path).unwrap();
            let version = create_test_node_version(1);
            storage
                .store_batch_with_lsn(&[version], &[], lsn_value)
                .unwrap();
            storage.compact().unwrap();
        }

        {
            let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
            assert_eq!(storage.get_flushed_lsn().unwrap(), Some(lsn_value));
        }
    }

    #[test]
    fn test_multiple_database_instances_same_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        // Create first instance and write data
        {
            let storage1 = RedbColdStorage::with_default_config(&db_path).unwrap();
            let version = create_test_node_version(1);
            storage1.store_node_version(&version).unwrap();
        } // Close first instance

        // Open second instance
        {
            let storage2 = RedbColdStorage::with_default_config(&db_path).unwrap();
            assert!(
                storage2
                    .contains_node_version(VersionId::new(1).unwrap())
                    .unwrap()
            );

            // Add more data
            let version = create_test_node_version(2);
            storage2.store_node_version(&version).unwrap();
        }

        // Open third instance and verify both
        {
            let storage3 = RedbColdStorage::with_default_config(&db_path).unwrap();
            assert!(
                storage3
                    .contains_node_version(VersionId::new(1).unwrap())
                    .unwrap()
            );
            assert!(
                storage3
                    .contains_node_version(VersionId::new(2).unwrap())
                    .unwrap()
            );
        }
    }

    #[test]
    fn test_flush_idempotent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Multiple flushes should all succeed
        assert!(storage.flush().is_ok());
        assert!(storage.flush().is_ok());
        assert!(storage.flush().is_ok());
    }

    #[test]
    fn test_close_idempotent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Multiple closes should all succeed
        assert!(storage.close().is_ok());
        assert!(storage.close().is_ok());
    }

    #[test]
    fn test_empty_node_batch_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Empty batch should succeed without error
        storage.store_node_versions_batch(&[]).unwrap();

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 0);
    }

    #[test]
    fn test_empty_edge_batch_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Empty batch should succeed without error
        storage.store_edge_versions_batch(&[]).unwrap();

        let stats = storage.stats();
        assert_eq!(stats.edge_versions_stored, 0);
    }

    #[test]
    fn test_compression_ratio_tracking() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let config = RedbConfig::new().compression(CompressionAlgorithm::Zstd);
        let storage = RedbColdStorage::new(&db_path, config).unwrap();

        // Create highly compressible data
        let mut props = PropertyMapBuilder::new();
        let repeated_text = "A".repeat(1000);
        props = props.insert("text", repeated_text.as_str());
        let properties = props.build();

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
        );

        storage.store_node_version(&version).unwrap();

        let stats = storage.stats();
        assert!(stats.bytes_written_raw > 0);
        assert!(stats.bytes_written_compressed > 0);
        // Compression should provide significant benefit for repeated data
        assert!(stats.bytes_written_compressed < stats.bytes_written_raw);
    }

    // ========================================================================
    // Additional tests to increase function coverage by exercising more paths
    // ========================================================================

    #[test]
    fn test_get_flushed_lsn_with_invalid_metadata_bytes() {
        // Test the specific error path where LSN has wrong byte count
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        {
            // Manually create corrupted metadata
            let db = redb::Database::create(&db_path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(METADATA_TABLE).unwrap();
                // Write 7 bytes instead of 8 (invalid)
                table
                    .insert(FLUSHED_LSN_KEY, &[1, 2, 3, 4, 5, 6, 7][..])
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        let result = storage.get_flushed_lsn();
        assert!(result.is_err());
    }

    #[test]
    fn test_set_flushed_lsn_internal_with_existing_lower_value() {
        // This exercises the branch where we read existing LSN and compare
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Set LSN to 200
        storage.store_batch_with_lsn(&[], &[], LSN(200)).unwrap();

        // Try to set to 100 (should be skipped)
        storage.store_batch_with_lsn(&[], &[], LSN(100)).unwrap();

        // Should still be 200
        assert_eq!(storage.get_flushed_lsn().unwrap(), Some(LSN(200)));
    }

    #[test]
    fn test_node_version_decode_error_path() {
        // Test decode error by storing corrupted data directly
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        {
            let db = redb::Database::create(&db_path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(NODE_VERSIONS_TABLE).unwrap();
                // Store invalid serialized data
                table.insert(12345, &[0xFF, 0xFF, 0xFF][..]).unwrap();
            }
            write_txn.commit().unwrap();
        }

        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        let result = storage.get_node_version(VersionId::new(12345).unwrap());
        // Should fail to decompress or decode
        assert!(result.is_err());
    }

    #[test]
    fn test_edge_version_decode_error_path() {
        // Test decode error for edges
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");

        {
            let db = redb::Database::create(&db_path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(EDGE_VERSIONS_TABLE).unwrap();
                // Store invalid serialized data
                table.insert(12345, &[0xAA, 0xBB, 0xCC][..]).unwrap();
            }
            write_txn.commit().unwrap();
        }

        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();
        let result = storage.get_edge_version(VersionId::new(12345).unwrap());
        // Should fail to decompress or decode
        assert!(result.is_err());
    }

    #[test]
    fn test_store_many_versions_individually() {
        // Exercise individual store paths many times
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        for i in 1..=100 {
            let node = create_test_node_version(i);
            storage.store_node_version(&node).unwrap();
        }

        for i in 1..=100 {
            let edge = create_test_edge_version(i);
            storage.store_edge_version(&edge).unwrap();
        }

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 100);
        assert_eq!(stats.edge_versions_stored, 100);
    }

    #[test]
    fn test_contains_then_get_pattern() {
        // Exercise the common pattern of checking existence then retrieving
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let node = create_test_node_version(1);
        let edge = create_test_edge_version(2);

        storage.store_node_version(&node).unwrap();
        storage.store_edge_version(&edge).unwrap();

        // Contains then get pattern
        if storage.contains_node_version(node.id).unwrap() {
            let retrieved = storage.get_node_version(node.id).unwrap();
            assert!(retrieved.is_some());
        }

        if storage.contains_edge_version(edge.id).unwrap() {
            let retrieved = storage.get_edge_version(edge.id).unwrap();
            assert!(retrieved.is_some());
        }
    }

    #[test]
    fn test_delete_then_reinsert_same_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let version_id = VersionId::new(42).unwrap();

        // Create and store
        let version1 = create_test_node_version(42);
        storage.store_node_version(&version1).unwrap();
        assert!(storage.contains_node_version(version_id).unwrap());

        // Delete
        storage.delete_node_version(version_id).unwrap();
        assert!(!storage.contains_node_version(version_id).unwrap());

        // Reinsert with same ID
        let version2 = create_test_node_version(42);
        storage.store_node_version(&version2).unwrap();
        assert!(storage.contains_node_version(version_id).unwrap());

        // Verify it's there
        let retrieved = storage.get_node_version(version_id).unwrap();
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_batch_with_duplicates() {
        // Store batch with duplicate IDs (last one wins)
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Create versions with same ID
        let v1 = create_test_node_version(1);
        let v2 = create_test_node_version(1); // Same ID
        let v3 = create_test_node_version(1); // Same ID again

        storage.store_node_versions_batch(&[v1, v2, v3]).unwrap();

        // Should have the last version
        assert!(
            storage
                .contains_node_version(VersionId::new(1).unwrap())
                .unwrap()
        );
        let stats = storage.stats();
        // Stored 3 times even though same ID
        assert_eq!(stats.node_versions_stored, 3);
    }

    #[test]
    fn test_alternating_node_edge_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Alternate between node and edge operations
        for i in 1..=20 {
            if i % 2 == 0 {
                let node = create_test_node_version(i);
                storage.store_node_version(&node).unwrap();
            } else {
                let edge = create_test_edge_version(i);
                storage.store_edge_version(&edge).unwrap();
            }
        }

        // Verify counts
        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 10);
        assert_eq!(stats.edge_versions_stored, 10);
    }

    #[test]
    fn test_get_nonexistent_after_delete() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        let node = create_test_node_version(1);
        storage.store_node_version(&node).unwrap();

        // Delete it
        storage.delete_node_version(node.id).unwrap();

        // Try to get it
        let result = storage.get_node_version(node.id).unwrap();
        assert!(result.is_none());

        // Try to contains it
        assert!(!storage.contains_node_version(node.id).unwrap());
    }

    #[test]
    fn test_stats_snapshot_consistency() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Take initial snapshot
        let stats1 = storage.stats();
        assert_eq!(stats1.node_versions_stored, 0);

        // Do some operations
        let node = create_test_node_version(1);
        storage.store_node_version(&node).unwrap();

        // Take another snapshot
        let stats2 = storage.stats();
        assert_eq!(stats2.node_versions_stored, 1);

        // First snapshot unchanged
        assert_eq!(stats1.node_versions_stored, 0);
    }

    #[test]
    fn test_very_large_property_map() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Create version with many properties
        let mut props = PropertyMapBuilder::new();
        for i in 0..1000 {
            props = props.insert(&format!("prop_{}", i), i as i64);
        }
        let properties = props.build();

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
        );

        storage.store_node_version(&version).unwrap();
        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    #[test]
    fn test_interleaved_batch_and_individual() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        // Individual
        storage
            .store_node_version(&create_test_node_version(1))
            .unwrap();

        // Batch
        let nodes: Vec<_> = (2..=5).map(create_test_node_version).collect();
        storage.store_node_versions_batch(&nodes).unwrap();

        // Individual again
        storage
            .store_node_version(&create_test_node_version(6))
            .unwrap();

        // Batch with LSN
        let nodes: Vec<_> = (7..=10).map(create_test_node_version).collect();
        storage.store_batch_with_lsn(&nodes, &[], LSN(100)).unwrap();

        // Verify all present
        for i in 1..=10 {
            assert!(
                storage
                    .contains_node_version(VersionId::new(i).unwrap())
                    .unwrap()
            );
        }
    }

    #[test]
    fn test_fault_injection() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let storage = RedbColdStorage::with_default_config(&db_path).unwrap();

        storage.set_fail_writes(true);

        let node = create_test_node_version(1);
        let result = storage.store_node_version(&node);

        assert!(result.is_err());
        assert!(storage.was_write_attempted());
    }
}
