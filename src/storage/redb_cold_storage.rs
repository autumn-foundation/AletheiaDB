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
//! use gallifreydb::storage::ColdStorage;
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
use crate::storage::cold_storage::{
    AtomicColdStorageStats, ColdStorage, ColdStorageConfig, ColdStorageStats, CompressionAlgorithm,
    decode_edge_version, decode_node_version, encode_edge_version, encode_node_version,
};
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::storage::wal::LSN;
use crate::utils::error::{Result, StorageError};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

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
/// This implementation uses Redb for durable, crash-safe storage of historical
/// versions. It supports LSN tracking for WAL truncation coordination.
pub struct RedbColdStorage {
    /// Path to the database file.
    path: PathBuf,
    /// Redb database instance.
    db: redb::Database,
    /// Configuration.
    config: RedbConfig,
    /// Statistics tracker.
    stats: AtomicColdStorageStats,
}

impl RedbColdStorage {
    /// Create a new Redb cold storage at the given path.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the database file
    /// * `config` - Configuration options
    ///
    /// # Returns
    ///
    /// Returns the storage instance or an error if database creation fails.
    pub fn new<P: AsRef<Path>>(path: P, config: RedbConfig) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to create directory: {}", e)).into()
            })?;
        }

        // Open or create the database
        let db = redb::Database::create(&path).map_err(|e| -> crate::utils::error::Error {
            StorageError::io_error(format!("Failed to open Redb database: {}", e)).into()
        })?;

        // Initialize tables by creating them if they don't exist
        let write_txn = db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

        // Open tables to create them
        write_txn
            .open_table(NODE_VERSIONS_TABLE)
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to create node_versions table: {}", e))
                    .into()
            })?;
        write_txn
            .open_table(EDGE_VERSIONS_TABLE)
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to create edge_versions table: {}", e))
                    .into()
            })?;
        write_txn
            .open_table(METADATA_TABLE)
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to create metadata table: {}", e)).into()
            })?;

        write_txn
            .commit()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit table creation: {}", e)).into()
            })?;

        Ok(Self {
            path,
            db,
            config,
            stats: AtomicColdStorageStats::new(),
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

    /// Get the flushed LSN from the metadata table.
    pub fn get_flushed_lsn(&self) -> Result<Option<LSN>> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin read transaction: {}", e)).into()
            })?;

        let table =
            read_txn
                .open_table(METADATA_TABLE)
                .map_err(|e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open metadata table: {}", e)).into()
                })?;

        match table.get(FLUSHED_LSN_KEY) {
            Ok(Some(value)) => {
                let bytes = value.value();
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
        let lsn_bytes = lsn.0.to_le_bytes();
        table
            .insert(FLUSHED_LSN_KEY, lsn_bytes.as_slice())
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to write flushed_lsn: {}", e)).into()
            })?;
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
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

        // Store node versions
        {
            let mut table = write_txn.open_table(NODE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open node_versions table: {}", e))
                        .into()
                },
            )?;

            for version in nodes {
                let encoded = encode_node_version(version);
                let raw_size = encoded.len();
                let compressed = self.compress(&encoded)?;
                let compressed_size = compressed.len();

                table
                    .insert(version.id.as_u64(), compressed.as_slice())
                    .map_err(|e| -> crate::utils::error::Error {
                        StorageError::io_error(format!("Failed to store node version: {}", e))
                            .into()
                    })?;

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
            let mut table = write_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open edge_versions table: {}", e))
                        .into()
                },
            )?;

            for version in edges {
                let encoded = encode_edge_version(version);
                let raw_size = encoded.len();
                let compressed = self.compress(&encoded)?;
                let compressed_size = compressed.len();

                table
                    .insert(version.id.as_u64(), compressed.as_slice())
                    .map_err(|e| -> crate::utils::error::Error {
                        StorageError::io_error(format!("Failed to store edge version: {}", e))
                            .into()
                    })?;

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
            let mut table = write_txn.open_table(METADATA_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open metadata table: {}", e)).into()
                },
            )?;
            Self::set_flushed_lsn_internal(&mut table, lsn)?;
        }

        // Commit atomically
        write_txn
            .commit()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit batch: {}", e)).into()
            })?;

        Ok(())
    }

    /// Compact the database to reclaim space.
    ///
    /// Note: This method requires mutable access because Redb's compact
    /// operation modifies the database file structure.
    pub fn compact(&mut self) -> Result<()> {
        self.db
            .compact()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to compact database: {}", e)).into()
            })?;
        Ok(())
    }
}

impl ColdStorage for RedbColdStorage {
    fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        let encoded = encode_node_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

        {
            let mut table = write_txn.open_table(NODE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open node_versions table: {}", e))
                        .into()
                },
            )?;

            table
                .insert(version.id.as_u64(), compressed.as_slice())
                .map_err(|e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to store node version: {}", e)).into()
                })?;
        }

        write_txn
            .commit()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit: {}", e)).into()
            })?;

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

    fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>> {
        self.stats
            .node_version_reads
            .fetch_add(1, Ordering::Relaxed);

        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin read transaction: {}", e)).into()
            })?;

        let table = read_txn.open_table(NODE_VERSIONS_TABLE).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to open node_versions table: {}", e)).into()
            },
        )?;

        match table.get(id.as_u64()) {
            Ok(Some(value)) => {
                let compressed = value.value();
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

    fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        let encoded = encode_edge_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

        {
            let mut table = write_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
                |e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open edge_versions table: {}", e))
                        .into()
                },
            )?;

            table
                .insert(version.id.as_u64(), compressed.as_slice())
                .map_err(|e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to store edge version: {}", e)).into()
                })?;
        }

        write_txn
            .commit()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit: {}", e)).into()
            })?;

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

    fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>> {
        self.stats
            .edge_version_reads
            .fetch_add(1, Ordering::Relaxed);

        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin read transaction: {}", e)).into()
            })?;

        let table = read_txn.open_table(EDGE_VERSIONS_TABLE).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to open edge_versions table: {}", e)).into()
            },
        )?;

        match table.get(id.as_u64()) {
            Ok(Some(value)) => {
                let compressed = value.value();
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

    fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin read transaction: {}", e)).into()
            })?;

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

    fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin read transaction: {}", e)).into()
            })?;

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

    fn delete_node_version(&self, id: VersionId) -> Result<bool> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

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
                    return Err(StorageError::io_error(format!(
                        "Failed to delete node version: {}",
                        e
                    ))
                    .into());
                }
            }
        };

        write_txn
            .commit()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit: {}", e)).into()
            })?;

        Ok(deleted)
    }

    fn delete_edge_version(&self, id: VersionId) -> Result<bool> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

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
                    return Err(StorageError::io_error(format!(
                        "Failed to delete edge version: {}",
                        e
                    ))
                    .into());
                }
            }
        };

        write_txn
            .commit()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit: {}", e)).into()
            })?;

        Ok(deleted)
    }

    fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
        if versions.is_empty() {
            return Ok(());
        }

        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

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
                    .map_err(|e| -> crate::utils::error::Error {
                        StorageError::io_error(format!("Failed to store node version: {}", e))
                            .into()
                    })?;

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
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit batch: {}", e)).into()
            })?;

        Ok(())
    }

    fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
        if versions.is_empty() {
            return Ok(());
        }

        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to begin write transaction: {}", e)).into()
            })?;

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
                    .map_err(|e| -> crate::utils::error::Error {
                        StorageError::io_error(format!("Failed to store edge version: {}", e))
                            .into()
                    })?;

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
            .map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to commit batch: {}", e)).into()
            })?;

        Ok(())
    }

    fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    fn flush(&self) -> Result<()> {
        // Redb automatically flushes on commit, nothing extra needed
        Ok(())
    }

    fn close(&self) -> Result<()> {
        // Redb handles cleanup on drop
        Ok(())
    }

    fn get_flushed_lsn(&self) -> Result<Option<LSN>> {
        RedbColdStorage::get_flushed_lsn(self)
    }

    fn store_batch_with_lsn(
        &self,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        lsn: LSN,
    ) -> Result<()> {
        RedbColdStorage::store_batch_with_lsn(self, nodes, edges, lsn)
    }
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
}
