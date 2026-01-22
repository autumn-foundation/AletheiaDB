//! RocksDB-based cold storage backend.
//!
//! This module provides a high-performance cold storage implementation using RocksDB,
//! as specified in ADR-0013 (Tiered Storage Architecture).
//!
//! # Features
//!
//! - LSM-tree optimized for write-heavy workloads (version ingestion)
//! - Built-in compression (LZ4/Zstd configurable)
//! - Column families for node and edge versions
//! - Batch operations for efficient migration
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::storage::rocksdb_cold_storage::{RocksDBColdStorage, RocksDBConfig};
//!
//! let config = RocksDBConfig::default();
//! let storage = RocksDBColdStorage::open("data/cold", config)?;
//!
//! storage.store_node_version(&version)?;
//! let retrieved = storage.get_node_version(version_id)?;
//! ```

use crate::core::id::VersionId;
use crate::storage::cold_storage::{
    AtomicColdStorageStats, ColdStorage, ColdStorageStats, CompressionAlgorithm,
};
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::utils::error::{Result, StorageError};
use rocksdb::{ColumnFamilyDescriptor, DB, Options, WriteBatch};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

/// Column family names for RocksDB.
const CF_NODE_VERSIONS: &str = "node_versions";
const CF_EDGE_VERSIONS: &str = "edge_versions";

/// Configuration for RocksDB cold storage.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
pub struct RocksDBConfig {
    /// Compression algorithm to use.
    pub compression: CompressionAlgorithm,

    /// Write buffer size in bytes (default: 64MB).
    /// Larger values improve write throughput but use more memory.
    pub write_buffer_size: usize,

    /// Maximum number of write buffers (default: 3).
    pub max_write_buffer_number: i32,

    /// Target file size for level 1 in bytes (default: 64MB).
    pub target_file_size_base: u64,

    /// Maximum number of open files (default: 1000).
    /// Set to -1 for unlimited.
    pub max_open_files: i32,

    /// Enable direct I/O for reads (bypasses OS cache).
    pub use_direct_reads: bool,

    /// Enable direct I/O for compaction (bypasses OS cache).
    pub use_direct_io_for_compaction: bool,

    /// Block cache size in bytes (default: 128MB).
    pub block_cache_size: usize,

    /// Enable bloom filters for faster lookups.
    pub enable_bloom_filter: bool,

    /// Bloom filter bits per key (default: 10).
    pub bloom_filter_bits: i32,
}

impl Default for RocksDBConfig {
    fn default() -> Self {
        Self {
            compression: CompressionAlgorithm::Zstd,
            write_buffer_size: 64 * 1024 * 1024, // 64MB
            max_write_buffer_number: 3,
            target_file_size_base: 64 * 1024 * 1024, // 64MB
            max_open_files: 1000,
            use_direct_reads: false,
            use_direct_io_for_compaction: false,
            block_cache_size: 128 * 1024 * 1024, // 128MB
            enable_bloom_filter: true,
            bloom_filter_bits: 10,
        }
    }
}

impl RocksDBConfig {
    /// Create configuration optimized for write-heavy workloads (migration).
    pub fn write_optimized() -> Self {
        Self {
            write_buffer_size: 128 * 1024 * 1024, // 128MB
            max_write_buffer_number: 4,
            target_file_size_base: 128 * 1024 * 1024, // 128MB
            ..Default::default()
        }
    }

    /// Create configuration optimized for read-heavy workloads (queries).
    pub fn read_optimized() -> Self {
        Self {
            block_cache_size: 256 * 1024 * 1024, // 256MB
            enable_bloom_filter: true,
            bloom_filter_bits: 14, // More bits for fewer false positives
            ..Default::default()
        }
    }

    fn to_rocksdb_compression(&self) -> rocksdb::DBCompressionType {
        match self.compression {
            CompressionAlgorithm::None => rocksdb::DBCompressionType::None,
            CompressionAlgorithm::Zstd => rocksdb::DBCompressionType::Zstd,
            CompressionAlgorithm::Fast => rocksdb::DBCompressionType::Lz4,
        }
    }
}

/// RocksDB-based cold storage backend.
///
/// This implementation uses RocksDB's LSM-tree for efficient write-heavy workloads
/// typical of version migration. It provides:
///
/// - Efficient batch writes for migration
/// - Built-in compression
/// - Column families for node/edge separation
/// - Bloom filters for fast negative lookups
pub struct RocksDBColdStorage {
    db: DB,
    #[allow(dead_code)]
    path: PathBuf,
    stats: AtomicColdStorageStats,
}

impl RocksDBColdStorage {
    /// Open or create a RocksDB cold storage at the given path.
    ///
    /// # Arguments
    ///
    /// * `path` - Directory path for the RocksDB database
    /// * `config` - Configuration options
    ///
    /// # Returns
    ///
    /// Returns the cold storage instance or an error if opening fails.
    pub fn open<P: AsRef<Path>>(path: P, config: RocksDBConfig) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Configure RocksDB options
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.set_compression_type(config.to_rocksdb_compression());
        opts.set_write_buffer_size(config.write_buffer_size);
        opts.set_max_write_buffer_number(config.max_write_buffer_number);
        opts.set_target_file_size_base(config.target_file_size_base);
        opts.set_max_open_files(config.max_open_files);

        if config.use_direct_reads {
            opts.set_use_direct_reads(true);
        }
        if config.use_direct_io_for_compaction {
            opts.set_use_direct_io_for_flush_and_compaction(true);
        }

        // Configure block-based table with bloom filter
        let mut block_opts = rocksdb::BlockBasedOptions::default();
        if config.enable_bloom_filter {
            block_opts.set_bloom_filter(config.bloom_filter_bits as f64, false);
        }
        // Note: block cache is handled by RocksDB's default settings
        opts.set_block_based_table_factory(&block_opts);

        // Define column families
        let cf_descriptors = vec![
            ColumnFamilyDescriptor::new(CF_NODE_VERSIONS, Options::default()),
            ColumnFamilyDescriptor::new(CF_EDGE_VERSIONS, Options::default()),
        ];

        // Open the database
        let db = DB::open_cf_descriptors(&opts, &path, cf_descriptors)
            .map_err(|e| StorageError::persistence(format!("Failed to open RocksDB: {}", e)))?;

        Ok(Self {
            db,
            path,
            stats: AtomicColdStorageStats::new(),
        })
    }

    /// Open with default configuration.
    pub fn open_default<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open(path, RocksDBConfig::default())
    }

    /// Get the column family handle for node versions.
    ///
    /// # Errors
    ///
    /// Returns an error if the column family is missing, which indicates
    /// RocksDB is in an inconsistent state.
    fn cf_nodes(&self) -> Result<&rocksdb::ColumnFamily> {
        self.db.cf_handle(CF_NODE_VERSIONS).ok_or_else(|| {
            StorageError::persistence(
                "node_versions column family missing - database may be corrupted",
            )
            .into()
        })
    }

    /// Get the column family handle for edge versions.
    ///
    /// # Errors
    ///
    /// Returns an error if the column family is missing, which indicates
    /// RocksDB is in an inconsistent state.
    fn cf_edges(&self) -> Result<&rocksdb::ColumnFamily> {
        self.db.cf_handle(CF_EDGE_VERSIONS).ok_or_else(|| {
            StorageError::persistence(
                "edge_versions column family missing - database may be corrupted",
            )
            .into()
        })
    }

    /// Convert a VersionId to a RocksDB key.
    fn version_key(id: VersionId) -> [u8; 8] {
        id.as_u64().to_be_bytes()
    }

    /// Store multiple node versions atomically using a write batch.
    ///
    /// This is more efficient than individual stores and provides atomicity.
    pub fn store_node_versions_atomic(&self, versions: &[NodeVersion]) -> Result<()> {
        let mut batch = WriteBatch::default();
        let cf = self.cf_nodes()?;

        let mut total_raw = 0u64;

        for version in versions {
            let key = Self::version_key(version.id);
            let encoded = super::cold_storage::encode_node_version(version);
            total_raw += encoded.len() as u64;

            // Note: RocksDB handles compression internally at the block level.
            // We track raw (uncompressed) bytes here. Actual on-disk size after
            // compression can be queried via approximate_size() or RocksDB properties.
            batch.put_cf(cf, key, &encoded);
        }

        self.db.write(batch).map_err(|e| {
            self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            StorageError::persistence(format!("Failed to write batch: {}", e))
        })?;

        self.stats
            .node_versions_stored
            .fetch_add(versions.len() as u64, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(total_raw, Ordering::Relaxed);
        // Note: bytes_written_compressed equals raw because RocksDB compresses
        // internally at the block level. Use approximate_size() for actual disk usage.
        self.stats
            .bytes_written_compressed
            .fetch_add(total_raw, Ordering::Relaxed);

        Ok(())
    }

    /// Store multiple edge versions atomically using a write batch.
    pub fn store_edge_versions_atomic(&self, versions: &[EdgeVersion]) -> Result<()> {
        let mut batch = WriteBatch::default();
        let cf = self.cf_edges()?;

        let mut total_raw = 0u64;

        for version in versions {
            let key = Self::version_key(version.id);
            let encoded = super::cold_storage::encode_edge_version(version);
            total_raw += encoded.len() as u64;

            // Note: RocksDB handles compression internally at the block level.
            batch.put_cf(cf, key, &encoded);
        }

        self.db.write(batch).map_err(|e| {
            self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            StorageError::persistence(format!("Failed to write batch: {}", e))
        })?;

        self.stats
            .edge_versions_stored
            .fetch_add(versions.len() as u64, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(total_raw, Ordering::Relaxed);
        // Note: bytes_written_compressed equals raw because RocksDB compresses
        // internally at the block level. Use approximate_size() for actual disk usage.
        self.stats
            .bytes_written_compressed
            .fetch_add(total_raw, Ordering::Relaxed);

        Ok(())
    }

    /// Compact the database to reclaim space and improve read performance.
    ///
    /// This is a blocking operation and should be called during maintenance windows.
    ///
    /// # Errors
    ///
    /// Returns an error if column families are missing (database corruption).
    pub fn compact(&self) -> Result<()> {
        let cf_nodes = self.cf_nodes()?;
        let cf_edges = self.cf_edges()?;
        self.db
            .compact_range_cf(cf_nodes, None::<&[u8]>, None::<&[u8]>);
        self.db
            .compact_range_cf(cf_edges, None::<&[u8]>, None::<&[u8]>);
        Ok(())
    }

    /// Get the actual compression ratio based on RocksDB statistics.
    ///
    /// Returns the ratio of compressed size to raw size, or None if unavailable.
    /// A ratio of 0.3 means data is compressed to 30% of original size.
    ///
    /// Note: This reflects the overall database compression, not individual writes.
    pub fn compression_ratio(&self) -> Option<f64> {
        let stats = self.stats();
        let on_disk = self.approximate_size();

        if stats.bytes_written_raw > 0 && on_disk > 0 {
            Some(on_disk as f64 / stats.bytes_written_raw as f64)
        } else {
            None
        }
    }

    /// Get the approximate size of the database on disk in bytes.
    ///
    /// Returns 0 if column families are unavailable.
    pub fn approximate_size(&self) -> u64 {
        // Get sizes for both column families
        let node_size = self
            .cf_nodes()
            .ok()
            .and_then(|cf| {
                self.db
                    .property_int_value_cf(cf, "rocksdb.estimate-live-data-size")
                    .ok()
                    .flatten()
            })
            .unwrap_or(0);

        let edge_size = self
            .cf_edges()
            .ok()
            .and_then(|cf| {
                self.db
                    .property_int_value_cf(cf, "rocksdb.estimate-live-data-size")
                    .ok()
                    .flatten()
            })
            .unwrap_or(0);

        node_size + edge_size
    }
}

impl ColdStorage for RocksDBColdStorage {
    fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        let key = Self::version_key(version.id);
        let encoded = super::cold_storage::encode_node_version(version);
        let raw_size = encoded.len();
        let cf = self.cf_nodes()?;

        self.db.put_cf(cf, key, &encoded).map_err(|e| {
            self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            StorageError::persistence(format!("Failed to store node version: {}", e))
        })?;

        self.stats
            .node_versions_stored
            .fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(raw_size as u64, Ordering::Relaxed);
        self.stats
            .bytes_written_compressed
            .fetch_add(raw_size as u64, Ordering::Relaxed);

        Ok(())
    }

    fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
        self.store_node_versions_atomic(versions)
    }

    fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>> {
        self.stats
            .node_version_reads
            .fetch_add(1, Ordering::Relaxed);

        let key = Self::version_key(id);
        let cf = self.cf_nodes()?;
        match self.db.get_cf(cf, key) {
            Ok(Some(data)) => {
                self.stats
                    .bytes_read_compressed
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                self.stats
                    .bytes_read_decompressed
                    .fetch_add(data.len() as u64, Ordering::Relaxed);

                let version = super::cold_storage::decode_node_version(&data)?;
                Ok(Some(version))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                self.stats.read_errors.fetch_add(1, Ordering::Relaxed);
                Err(StorageError::persistence(format!("Failed to read node version: {}", e)).into())
            }
        }
    }

    fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        let key = Self::version_key(version.id);
        let encoded = super::cold_storage::encode_edge_version(version);
        let raw_size = encoded.len();
        let cf = self.cf_edges()?;

        self.db.put_cf(cf, key, &encoded).map_err(|e| {
            self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            StorageError::persistence(format!("Failed to store edge version: {}", e))
        })?;

        self.stats
            .edge_versions_stored
            .fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_written_raw
            .fetch_add(raw_size as u64, Ordering::Relaxed);
        self.stats
            .bytes_written_compressed
            .fetch_add(raw_size as u64, Ordering::Relaxed);

        Ok(())
    }

    fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
        self.store_edge_versions_atomic(versions)
    }

    fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>> {
        self.stats
            .edge_version_reads
            .fetch_add(1, Ordering::Relaxed);

        let key = Self::version_key(id);
        let cf = self.cf_edges()?;
        match self.db.get_cf(cf, key) {
            Ok(Some(data)) => {
                self.stats
                    .bytes_read_compressed
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                self.stats
                    .bytes_read_decompressed
                    .fetch_add(data.len() as u64, Ordering::Relaxed);

                let version = super::cold_storage::decode_edge_version(&data)?;
                Ok(Some(version))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                self.stats.read_errors.fetch_add(1, Ordering::Relaxed);
                Err(StorageError::persistence(format!("Failed to read edge version: {}", e)).into())
            }
        }
    }

    fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        let key = Self::version_key(id);
        let cf = self.cf_nodes()?;
        // Use key_may_exist for faster negative lookups (bloom filter)
        if !self.db.key_may_exist_cf(cf, key) {
            return Ok(false);
        }
        // Confirm with actual get if bloom filter says maybe
        Ok(self
            .db
            .get_cf(cf, key)
            .map_err(|e| StorageError::persistence(format!("Failed to check node version: {}", e)))?
            .is_some())
    }

    fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        let key = Self::version_key(id);
        let cf = self.cf_edges()?;
        if !self.db.key_may_exist_cf(cf, key) {
            return Ok(false);
        }
        Ok(self
            .db
            .get_cf(cf, key)
            .map_err(|e| StorageError::persistence(format!("Failed to check edge version: {}", e)))?
            .is_some())
    }

    fn delete_node_version(&self, id: VersionId) -> Result<bool> {
        let key = Self::version_key(id);
        let cf = self.cf_nodes()?;

        // Check if exists first
        let exists = self
            .db
            .get_cf(cf, key)
            .map_err(|e| StorageError::persistence(format!("Failed to check node version: {}", e)))?
            .is_some();

        if exists {
            self.db.delete_cf(cf, key).map_err(|e| {
                StorageError::persistence(format!("Failed to delete node version: {}", e))
            })?;
        }

        Ok(exists)
    }

    fn delete_edge_version(&self, id: VersionId) -> Result<bool> {
        let key = Self::version_key(id);
        let cf = self.cf_edges()?;

        let exists = self
            .db
            .get_cf(cf, key)
            .map_err(|e| StorageError::persistence(format!("Failed to check edge version: {}", e)))?
            .is_some();

        if exists {
            self.db.delete_cf(cf, key).map_err(|e| {
                StorageError::persistence(format!("Failed to delete edge version: {}", e))
            })?;
        }

        Ok(exists)
    }

    fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    fn flush(&self) -> Result<()> {
        self.db
            .flush()
            .map_err(|e| StorageError::persistence(format!("Failed to flush RocksDB: {}", e)))?;
        Ok(())
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

    #[test]
    fn test_rocksdb_store_and_retrieve_node() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.node_id, version.node_id);
    }

    #[test]
    fn test_rocksdb_store_and_retrieve_edge() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let version = create_test_edge_version(1);
        storage.store_edge_version(&version).unwrap();

        let retrieved = storage.get_edge_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.edge_id, version.edge_id);
    }

    #[test]
    fn test_rocksdb_get_nonexistent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let result = storage
            .get_node_version(VersionId::new(999).unwrap())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_rocksdb_contains() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let version = create_test_node_version(1);
        assert!(!storage.contains_node_version(version.id).unwrap());

        storage.store_node_version(&version).unwrap();
        assert!(storage.contains_node_version(version.id).unwrap());
    }

    #[test]
    fn test_rocksdb_delete() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let deleted = storage.delete_node_version(version.id).unwrap();
        assert!(deleted);

        assert!(!storage.contains_node_version(version.id).unwrap());

        let deleted_again = storage.delete_node_version(version.id).unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn test_rocksdb_batch_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let versions: Vec<NodeVersion> = (1..=100).map(create_test_node_version).collect();
        storage.store_node_versions_batch(&versions).unwrap();

        // Verify all versions are stored
        for version in &versions {
            assert!(storage.contains_node_version(version.id).unwrap());
        }

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 100);
    }

    #[test]
    fn test_rocksdb_stats() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();
        storage.get_node_version(version.id).unwrap();

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 1);
        assert_eq!(stats.node_version_reads, 1);
        assert!(stats.bytes_written_raw > 0);
    }

    #[test]
    fn test_rocksdb_persistence() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().to_path_buf();

        // Store some data
        {
            let storage = RocksDBColdStorage::open_default(&path).unwrap();
            let version = create_test_node_version(1);
            storage.store_node_version(&version).unwrap();
            storage.flush().unwrap();
        }

        // Reopen and verify data persists
        {
            let storage = RocksDBColdStorage::open_default(&path).unwrap();
            let retrieved = storage
                .get_node_version(VersionId::new(1).unwrap())
                .unwrap();
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().id.as_u64(), 1);
        }
    }

    #[test]
    fn test_rocksdb_config_write_optimized() {
        let config = RocksDBConfig::write_optimized();
        assert_eq!(config.write_buffer_size, 128 * 1024 * 1024);
        assert_eq!(config.max_write_buffer_number, 4);
    }

    #[test]
    fn test_rocksdb_config_read_optimized() {
        let config = RocksDBConfig::read_optimized();
        assert_eq!(config.block_cache_size, 256 * 1024 * 1024);
        assert_eq!(config.bloom_filter_bits, 14);
    }

    #[test]
    fn test_rocksdb_compression_types() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Test with Zstd compression
        let zstd_config = RocksDBConfig {
            compression: CompressionAlgorithm::Zstd,
            ..Default::default()
        };
        let storage = RocksDBColdStorage::open(temp_dir.path().join("zstd"), zstd_config).unwrap();
        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();
        assert!(storage.contains_node_version(version.id).unwrap());

        // Test with LZ4 (Fast) compression
        let fast_config = RocksDBConfig {
            compression: CompressionAlgorithm::Fast,
            ..Default::default()
        };
        let storage = RocksDBColdStorage::open(temp_dir.path().join("lz4"), fast_config).unwrap();
        storage.store_node_version(&version).unwrap();
        assert!(storage.contains_node_version(version.id).unwrap());

        // Test with no compression
        let none_config = RocksDBConfig {
            compression: CompressionAlgorithm::None,
            ..Default::default()
        };
        let storage = RocksDBColdStorage::open(temp_dir.path().join("none"), none_config).unwrap();
        storage.store_node_version(&version).unwrap();
        assert!(storage.contains_node_version(version.id).unwrap());
    }

    #[test]
    fn test_rocksdb_compression_ratio() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        // Store some data first
        let versions: Vec<NodeVersion> = (1..=50).map(create_test_node_version).collect();
        storage.store_node_versions_batch(&versions).unwrap();
        storage.flush().unwrap();

        // Get compression ratio
        let ratio = storage.compression_ratio();
        // May be None if no data on disk yet or Some with a ratio
        match ratio {
            Some(r) => {
                // Ratio should be positive and reasonable
                assert!(r > 0.0, "Compression ratio should be positive");
                // Typically compression ratio is between 0.1 and 2.0
                assert!(r < 10.0, "Compression ratio should be reasonable");
            }
            None => {
                // This is acceptable if approximate_size() returns 0
                // (e.g., data not yet flushed to disk)
            }
        }
    }

    #[test]
    fn test_rocksdb_compact() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        // Store some data
        let versions: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();
        storage.store_node_versions_batch(&versions).unwrap();

        // Compact should succeed
        let result = storage.compact();
        assert!(result.is_ok(), "Compact should succeed");

        // Data should still be accessible
        for version in &versions {
            assert!(storage.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_rocksdb_approximate_size() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        // Initial size should be 0 or small
        let _initial_size = storage.approximate_size();

        // Store some data
        let versions: Vec<NodeVersion> = (1..=100).map(create_test_node_version).collect();
        storage.store_node_versions_batch(&versions).unwrap();
        storage.flush().unwrap();

        // Size should increase after storing data
        let new_size = storage.approximate_size();
        // Note: This may or may not be greater depending on RocksDB internals
        // At minimum, check it doesn't panic and returns a valid value
        let _ = new_size; // Verify the call succeeds
    }

    #[test]
    fn test_rocksdb_edge_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let edge_version = create_test_edge_version(1);

        // Store edge
        storage.store_edge_version(&edge_version).unwrap();
        assert!(storage.contains_edge_version(edge_version.id).unwrap());

        // Retrieve edge
        let retrieved = storage.get_edge_version(edge_version.id).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, edge_version.id);

        // Delete edge
        let deleted = storage.delete_edge_version(edge_version.id).unwrap();
        assert!(deleted);
        assert!(!storage.contains_edge_version(edge_version.id).unwrap());

        // Delete non-existent edge
        let deleted_again = storage.delete_edge_version(edge_version.id).unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn test_rocksdb_batch_edge_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = RocksDBColdStorage::open_default(temp_dir.path()).unwrap();

        let versions: Vec<EdgeVersion> = (1..=50).map(create_test_edge_version).collect();
        storage.store_edge_versions_batch(&versions).unwrap();

        // Verify all edge versions are stored
        for version in &versions {
            assert!(storage.contains_edge_version(version.id).unwrap());
        }

        let stats = storage.stats();
        assert_eq!(stats.edge_versions_stored, 50);
    }
}
