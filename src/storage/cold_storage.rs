//! Cold storage backend for tiered storage architecture.
//!
//! This module implements the cold tier of the tiered storage system, providing
//! disk-based storage for historical versions that have been migrated from the
//! hot tier (in-memory).
//!
//! # Architecture
//!
//! The cold storage system follows ADR-0013 (Tiered Storage Architecture):
//! - **Hot Tier**: In-memory (DashMap), 22-70ns latency
//! - **Warm Tier**: LRU cache for recently accessed cold data, <1µs latency
//! - **Cold Tier**: Disk-based compressed storage, <1ms latency (p50)
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
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::storage::cold_storage::{ColdStorage, FileColdStorage, ColdStorageConfig};
//!
//! let config = ColdStorageConfig::default();
//! let storage = FileColdStorage::new("data/cold", config)?;
//!
//! // Store a version
//! storage.store_node_version(&version)?;
//!
//! // Retrieve a version
//! let retrieved = storage.get_node_version(version_id)?;
//! ```

use crate::core::id::VersionId;
#[cfg(test)]
use crate::storage::version::VersionMetadata;
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::storage::wal::LSN;
use crate::utils::error::{Result, StorageError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

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
///
/// # Compression Stats Note
///
/// For backends that handle compression internally at the storage level,
/// `bytes_written_compressed` may equal `bytes_written_raw` because the actual
/// compression happens asynchronously during compaction. To get the true on-disk
/// size, use the backend's `approximate_size()` method instead.
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
    ///
    /// Note: For backends that handle compression internally,
    /// this may equal `bytes_written_raw`. Use `approximate_size()` or
    /// `compression_ratio()` on the backend for actual compression metrics.
    pub bytes_written_compressed: u64,
    /// Total bytes read (compressed size from storage).
    ///
    /// Note: For backends with transparent decompression, this may equal
    /// `bytes_read_decompressed`.
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
    ///
    /// Returns 1.0 if no data has been written.
    ///
    /// Note: For some backends, this ratio may be 1.0 because compression
    /// happens at the storage level during compaction. Use the backend's own
    /// `compression_ratio()` method for actual on-disk compression metrics.
    pub fn compression_ratio(&self) -> f64 {
        if self.bytes_written_compressed == 0 {
            1.0
        } else {
            self.bytes_written_raw as f64 / self.bytes_written_compressed as f64
        }
    }
}

/// Trait for cold storage backends.
///
/// This trait defines the interface for storing and retrieving historical
/// versions from cold storage. Implementations must be thread-safe.
pub trait ColdStorage: Send + Sync {
    /// Store a node version to cold storage.
    ///
    /// # Arguments
    ///
    /// * `version` - The node version to store
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if storage fails.
    fn store_node_version(&self, version: &NodeVersion) -> Result<()>;

    /// Store multiple node versions in a batch.
    ///
    /// This is more efficient than individual stores for bulk migrations.
    ///
    /// # Arguments
    ///
    /// * `versions` - The node versions to store
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if storage fails.
    fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {
        for version in versions {
            self.store_node_version(version)?;
        }
        Ok(())
    }

    /// Retrieve a node version from cold storage.
    ///
    /// # Arguments
    ///
    /// * `id` - The version ID to retrieve
    ///
    /// # Returns
    ///
    /// Returns `Some(version)` if found, `None` if not found, or an error.
    fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>>;

    /// Retrieve multiple node versions in a batch.
    ///
    /// # Arguments
    ///
    /// * `ids` - The version IDs to retrieve
    ///
    /// # Returns
    ///
    /// Returns a vector of optional versions (same order as input IDs).
    fn get_node_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<NodeVersion>>> {
        ids.iter().map(|id| self.get_node_version(*id)).collect()
    }

    /// Store an edge version to cold storage.
    ///
    /// # Arguments
    ///
    /// * `version` - The edge version to store
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if storage fails.
    fn store_edge_version(&self, version: &EdgeVersion) -> Result<()>;

    /// Store multiple edge versions in a batch.
    ///
    /// # Arguments
    ///
    /// * `versions` - The edge versions to store
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if storage fails.
    fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {
        for version in versions {
            self.store_edge_version(version)?;
        }
        Ok(())
    }

    /// Retrieve an edge version from cold storage.
    ///
    /// # Arguments
    ///
    /// * `id` - The version ID to retrieve
    ///
    /// # Returns
    ///
    /// Returns `Some(version)` if found, `None` if not found, or an error.
    fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>>;

    /// Retrieve multiple edge versions in a batch.
    ///
    /// # Arguments
    ///
    /// * `ids` - The version IDs to retrieve
    ///
    /// # Returns
    ///
    /// Returns a vector of optional versions (same order as input IDs).
    fn get_edge_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<EdgeVersion>>> {
        ids.iter().map(|id| self.get_edge_version(*id)).collect()
    }

    /// Check if a node version exists in cold storage.
    fn contains_node_version(&self, id: VersionId) -> Result<bool>;

    /// Check if an edge version exists in cold storage.
    fn contains_edge_version(&self, id: VersionId) -> Result<bool>;

    /// Delete a node version from cold storage.
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if deleted, `Ok(false)` if not found.
    fn delete_node_version(&self, id: VersionId) -> Result<bool>;

    /// Delete an edge version from cold storage.
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if deleted, `Ok(false)` if not found.
    fn delete_edge_version(&self, id: VersionId) -> Result<bool>;

    /// Get statistics about cold storage.
    fn stats(&self) -> ColdStorageStats;

    /// Flush any buffered writes to disk.
    fn flush(&self) -> Result<()>;

    /// Close the cold storage, flushing any pending writes.
    fn close(&self) -> Result<()> {
        self.flush()
    }

    // ========================================================================
    // LSN Tracking (ADR-0025)
    //
    // These methods enable coordinated WAL truncation by tracking which LSN
    // has been durably flushed to cold storage. The key invariant is:
    //   WAL_truncation_lsn <= Redb_flushed_lsn
    //
    // This ensures any operation truncated from WAL has been durably persisted.
    // ========================================================================

    /// Get the highest LSN that has been durably flushed to cold storage.
    ///
    /// Returns `None` if no data has been flushed yet (cold storage is empty
    /// or the backend doesn't support LSN tracking).
    ///
    /// # LSN Tracking
    ///
    /// The flushed LSN represents a recovery checkpoint:
    /// - All operations with LSN <= flushed_lsn are in cold storage
    /// - WAL can be safely truncated up to this LSN
    /// - Recovery should replay from flushed_lsn + 1
    ///
    /// # Default Implementation
    ///
    /// Returns `None`, indicating LSN tracking is not supported.
    /// Implementations that support LSN tracking should override this.
    fn get_flushed_lsn(&self) -> Result<Option<LSN>> {
        Ok(None)
    }

    /// Store a batch of versions with LSN tracking.
    ///
    /// This operation should be atomic - either all versions are stored and
    /// the flushed_lsn is updated, or nothing is changed. This atomicity
    /// enables safe WAL truncation.
    ///
    /// # Arguments
    ///
    /// * `nodes` - Node versions to store
    /// * `edges` - Edge versions to store
    /// * `lsn` - The LSN to record as flushed after this batch
    ///
    /// # LSN Semantics
    ///
    /// After a successful call, `get_flushed_lsn()` should return at least `lsn`.
    /// The implementation may update the flushed_lsn to be:
    /// - Exactly `lsn`, or
    /// - `max(current_flushed_lsn, lsn)` if batches may arrive out of order
    ///
    /// # Default Implementation
    ///
    /// Delegates to `store_node_versions_batch` and `store_edge_versions_batch`,
    /// ignoring the LSN. Implementations that support LSN tracking should
    /// override this to provide atomic batch + LSN updates.
    fn store_batch_with_lsn(
        &self,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        _lsn: LSN,
    ) -> Result<()> {
        self.store_node_versions_batch(nodes)?;
        self.store_edge_versions_batch(edges)?;
        Ok(())
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
    /// Total bytes read (compressed).
    pub bytes_read_compressed: AtomicU64,
    /// Total bytes read (after decompression).
    pub bytes_read_decompressed: AtomicU64,
    /// Number of read errors.
    pub read_errors: AtomicU64,
    /// Number of write errors.
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

/// In-memory cold storage for testing purposes.
///
/// This implementation stores versions in memory using a HashMap.
/// It's useful for testing the cold storage interface without disk I/O.
pub struct InMemoryColdStorage {
    node_versions: parking_lot::RwLock<std::collections::HashMap<VersionId, Vec<u8>>>,
    edge_versions: parking_lot::RwLock<std::collections::HashMap<VersionId, Vec<u8>>>,
    config: ColdStorageConfig,
    stats: AtomicColdStorageStats,
}

impl InMemoryColdStorage {
    /// Create a new in-memory cold storage.
    pub fn new(config: ColdStorageConfig) -> Self {
        Self {
            node_versions: parking_lot::RwLock::new(std::collections::HashMap::new()),
            edge_versions: parking_lot::RwLock::new(std::collections::HashMap::new()),
            config,
            stats: AtomicColdStorageStats::new(),
        }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(ColdStorageConfig::default())
    }

    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        match self.config.compression.zstd_level() {
            Some(level) => {
                let compressed =
                    zstd::encode_all(data, level).map_err(|e| -> crate::utils::error::Error {
                        StorageError::io_error(e.to_string()).into()
                    })?;

                // Add checksum if enabled
                if self.config.enable_checksums {
                    let checksum = crc32fast::hash(&compressed);
                    let mut result = Vec::with_capacity(compressed.len() + 4);
                    result.extend_from_slice(&checksum.to_le_bytes());
                    result.extend_from_slice(&compressed);
                    Ok(result)
                } else {
                    Ok(compressed)
                }
            }
            None => {
                // No compression
                if self.config.enable_checksums {
                    let checksum = crc32fast::hash(data);
                    let mut result = Vec::with_capacity(data.len() + 4);
                    result.extend_from_slice(&checksum.to_le_bytes());
                    result.extend_from_slice(data);
                    Ok(result)
                } else {
                    Ok(data.to_vec())
                }
            }
        }
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let (data_to_decompress, expected_checksum) = if self.config.enable_checksums {
            if data.len() < 4 {
                return Err(
                    StorageError::corruption("Data too short for checksum".to_string()).into(),
                );
            }
            let checksum = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let payload = &data[4..];
            (payload, Some(checksum))
        } else {
            (data, None)
        };

        // Verify checksum if enabled
        if let Some(expected) = expected_checksum {
            let actual = crc32fast::hash(data_to_decompress);
            if actual != expected {
                return Err(StorageError::corruption(format!(
                    "Checksum mismatch: expected {}, got {}",
                    expected, actual
                ))
                .into());
            }
        }

        match self.config.compression.zstd_level() {
            Some(_) => {
                zstd::decode_all(data_to_decompress).map_err(|e| -> crate::utils::error::Error {
                    StorageError::io_error(e.to_string()).into()
                })
            }
            None => Ok(data_to_decompress.to_vec()),
        }
    }
}

impl ColdStorage for InMemoryColdStorage {
    fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        let encoded = encode_node_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        self.node_versions.write().insert(version.id, compressed);

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

        let guard = self.node_versions.read();
        match guard.get(&id) {
            Some(compressed) => {
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
            None => Ok(None),
        }
    }

    fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        let encoded = encode_edge_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        self.edge_versions.write().insert(version.id, compressed);

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

        let guard = self.edge_versions.read();
        match guard.get(&id) {
            Some(compressed) => {
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
            None => Ok(None),
        }
    }

    fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        Ok(self.node_versions.read().contains_key(&id))
    }

    fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        Ok(self.edge_versions.read().contains_key(&id))
    }

    fn delete_node_version(&self, id: VersionId) -> Result<bool> {
        Ok(self.node_versions.write().remove(&id).is_some())
    }

    fn delete_edge_version(&self, id: VersionId) -> Result<bool> {
        Ok(self.edge_versions.write().remove(&id).is_some())
    }

    fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    fn flush(&self) -> Result<()> {
        // In-memory storage doesn't need flushing
        Ok(())
    }
}

/// File-based cold storage backend.
///
/// Stores versions as individual compressed files organized by version ID.
/// This is a simple but effective implementation suitable for moderate workloads.
///
/// # Directory Structure
///
/// ```text
/// cold_storage/
/// ├── nodes/
/// │   ├── 00/
/// │   │   ├── 00000001.bin
/// │   │   └── 00000002.bin
/// │   └── 01/
/// │       └── 00000100.bin
/// └── edges/
///     ├── 00/
///     │   └── 00000001.bin
///     └── 01/
///         └── 00000100.bin
/// ```
pub struct FileColdStorage {
    base_path: PathBuf,
    config: ColdStorageConfig,
    stats: AtomicColdStorageStats,
}

impl FileColdStorage {
    /// Create a new file-based cold storage.
    ///
    /// # Arguments
    ///
    /// * `base_path` - Base directory for cold storage files
    /// * `config` - Configuration options
    ///
    /// # Returns
    ///
    /// Returns the cold storage instance or an error if directory creation fails.
    pub fn new<P: AsRef<Path>>(base_path: P, config: ColdStorageConfig) -> Result<Self> {
        let base_path = base_path.as_ref().to_path_buf();

        // Create directory structure
        std::fs::create_dir_all(base_path.join("nodes")).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to create nodes directory: {}", e)).into()
            },
        )?;
        std::fs::create_dir_all(base_path.join("edges")).map_err(
            |e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to create edges directory: {}", e)).into()
            },
        )?;

        Ok(Self {
            base_path,
            config,
            stats: AtomicColdStorageStats::new(),
        })
    }

    /// Create with default configuration.
    pub fn with_default_config<P: AsRef<Path>>(base_path: P) -> Result<Self> {
        Self::new(base_path, ColdStorageConfig::default())
    }

    /// Get the file path for a node version.
    fn node_path(&self, id: VersionId) -> PathBuf {
        let id_val = id.as_u64();
        let shard = (id_val / 10000) % 256; // Shard by 10k versions
        let shard_dir = self.base_path.join("nodes").join(format!("{:02x}", shard));
        shard_dir.join(format!("{:016x}.bin", id_val))
    }

    /// Get the file path for an edge version.
    fn edge_path(&self, id: VersionId) -> PathBuf {
        let id_val = id.as_u64();
        let shard = (id_val / 10000) % 256; // Shard by 10k versions
        let shard_dir = self.base_path.join("edges").join(format!("{:02x}", shard));
        shard_dir.join(format!("{:016x}.bin", id_val))
    }

    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        match self.config.compression.zstd_level() {
            Some(level) => {
                let compressed =
                    zstd::encode_all(data, level).map_err(|e| -> crate::utils::error::Error {
                        StorageError::io_error(e.to_string()).into()
                    })?;

                if self.config.enable_checksums {
                    let checksum = crc32fast::hash(&compressed);
                    let mut result = Vec::with_capacity(compressed.len() + 4);
                    result.extend_from_slice(&checksum.to_le_bytes());
                    result.extend_from_slice(&compressed);
                    Ok(result)
                } else {
                    Ok(compressed)
                }
            }
            None => {
                if self.config.enable_checksums {
                    let checksum = crc32fast::hash(data);
                    let mut result = Vec::with_capacity(data.len() + 4);
                    result.extend_from_slice(&checksum.to_le_bytes());
                    result.extend_from_slice(data);
                    Ok(result)
                } else {
                    Ok(data.to_vec())
                }
            }
        }
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let (data_to_decompress, expected_checksum) = if self.config.enable_checksums {
            if data.len() < 4 {
                return Err(
                    StorageError::corruption("Data too short for checksum".to_string()).into(),
                );
            }
            let checksum = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let payload = &data[4..];
            (payload, Some(checksum))
        } else {
            (data, None)
        };

        if let Some(expected) = expected_checksum {
            let actual = crc32fast::hash(data_to_decompress);
            if actual != expected {
                return Err(StorageError::corruption(format!(
                    "Checksum mismatch: expected {}, got {}",
                    expected, actual
                ))
                .into());
            }
        }

        match self.config.compression.zstd_level() {
            Some(_) => {
                zstd::decode_all(data_to_decompress).map_err(|e| -> crate::utils::error::Error {
                    StorageError::io_error(e.to_string()).into()
                })
            }
            None => Ok(data_to_decompress.to_vec()),
        }
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to create directory: {}", e)).into()
            })?;
        }

        // Write atomically via temp file
        let temp_path = path.with_extension("tmp");
        std::fs::write(&temp_path, data).map_err(|e| -> crate::utils::error::Error {
            StorageError::io_error(format!("Failed to write temp file: {}", e)).into()
        })?;

        if self.config.sync_writes {
            // Open and sync the file
            let file =
                std::fs::File::open(&temp_path).map_err(|e| -> crate::utils::error::Error {
                    StorageError::io_error(format!("Failed to open for sync: {}", e)).into()
                })?;
            file.sync_all().map_err(|e| -> crate::utils::error::Error {
                StorageError::io_error(format!("Failed to sync: {}", e)).into()
            })?;
        }

        // Atomic rename
        std::fs::rename(&temp_path, path).map_err(|e| -> crate::utils::error::Error {
            StorageError::io_error(format!("Failed to rename: {}", e)).into()
        })?;

        Ok(())
    }

    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StorageError::io_error(format!("Failed to read file: {}", e)).into()),
        }
    }
}

impl ColdStorage for FileColdStorage {
    fn store_node_version(&self, version: &NodeVersion) -> Result<()> {
        let encoded = encode_node_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        let path = self.node_path(version.id);
        self.write_file(&path, &compressed)?;

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

        let path = self.node_path(id);
        match self.read_file(&path)? {
            Some(compressed) => {
                self.stats
                    .bytes_read_compressed
                    .fetch_add(compressed.len() as u64, Ordering::Relaxed);

                let decompressed = self.decompress(&compressed)?;

                self.stats
                    .bytes_read_decompressed
                    .fetch_add(decompressed.len() as u64, Ordering::Relaxed);

                let version = decode_node_version(&decompressed)?;
                Ok(Some(version))
            }
            None => Ok(None),
        }
    }

    fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {
        let encoded = encode_edge_version(version);
        let raw_size = encoded.len();
        let compressed = self.compress(&encoded)?;
        let compressed_size = compressed.len();

        let path = self.edge_path(version.id);
        self.write_file(&path, &compressed)?;

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

        let path = self.edge_path(id);
        match self.read_file(&path)? {
            Some(compressed) => {
                self.stats
                    .bytes_read_compressed
                    .fetch_add(compressed.len() as u64, Ordering::Relaxed);

                let decompressed = self.decompress(&compressed)?;

                self.stats
                    .bytes_read_decompressed
                    .fetch_add(decompressed.len() as u64, Ordering::Relaxed);

                let version = decode_edge_version(&decompressed)?;
                Ok(Some(version))
            }
            None => Ok(None),
        }
    }

    fn contains_node_version(&self, id: VersionId) -> Result<bool> {
        let path = self.node_path(id);
        Ok(path.exists())
    }

    fn contains_edge_version(&self, id: VersionId) -> Result<bool> {
        let path = self.edge_path(id);
        Ok(path.exists())
    }

    fn delete_node_version(&self, id: VersionId) -> Result<bool> {
        let path = self.node_path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StorageError::io_error(format!("Failed to delete: {}", e)).into()),
        }
    }

    fn delete_edge_version(&self, id: VersionId) -> Result<bool> {
        let path = self.edge_path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StorageError::io_error(format!("Failed to delete: {}", e)).into()),
        }
    }

    fn stats(&self) -> ColdStorageStats {
        self.stats.snapshot()
    }

    fn flush(&self) -> Result<()> {
        // File-based storage writes immediately, nothing to flush
        Ok(())
    }
}

// ============================================================================
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
/// Note: Array is stored as a serialized Vec<u8> to avoid recursive type issues with bitcode.
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
        metadata: crate::storage::version::VersionMetadata::default_for_existing(),
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
        metadata: crate::storage::version::VersionMetadata::default_for_existing(),
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
            VersionMetadata::default_for_existing(),
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
            VersionMetadata::default_for_existing(),
        )
    }

    // ========================================================================
    // ColdStorage trait tests (using InMemoryColdStorage)
    // ========================================================================

    #[test]
    fn test_store_and_retrieve_node_version() {
        let storage = InMemoryColdStorage::default_config();
        let version = create_test_node_version(1);

        storage.store_node_version(&version).unwrap();
        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();

        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.node_id, version.node_id);
    }

    #[test]
    fn test_store_and_retrieve_edge_version() {
        let storage = InMemoryColdStorage::default_config();
        let version = create_test_edge_version(1);

        storage.store_edge_version(&version).unwrap();
        let retrieved = storage.get_edge_version(version.id).unwrap().unwrap();

        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.edge_id, version.edge_id);
        assert_eq!(retrieved.source, version.source);
        assert_eq!(retrieved.target, version.target);
    }

    #[test]
    fn test_get_nonexistent_version() {
        let storage = InMemoryColdStorage::default_config();

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
    fn test_contains_version() {
        let storage = InMemoryColdStorage::default_config();
        let version = create_test_node_version(1);

        assert!(!storage.contains_node_version(version.id).unwrap());

        storage.store_node_version(&version).unwrap();

        assert!(storage.contains_node_version(version.id).unwrap());
    }

    #[test]
    fn test_delete_version() {
        let storage = InMemoryColdStorage::default_config();
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
    fn test_batch_store_and_retrieve() {
        let storage = InMemoryColdStorage::default_config();
        let versions: Vec<NodeVersion> = (1..=10).map(create_test_node_version).collect();

        storage.store_node_versions_batch(&versions).unwrap();

        let ids: Vec<VersionId> = versions.iter().map(|v| v.id).collect();
        let retrieved = storage.get_node_versions_batch(&ids).unwrap();

        assert_eq!(retrieved.len(), 10);
        for (i, v) in retrieved.iter().enumerate() {
            assert!(v.is_some());
            assert_eq!(v.as_ref().unwrap().id, versions[i].id);
        }
    }

    #[test]
    fn test_stats_tracking() {
        let storage = InMemoryColdStorage::default_config();
        let version = create_test_node_version(1);

        storage.store_node_version(&version).unwrap();
        storage.get_node_version(version.id).unwrap();

        let stats = storage.stats();
        assert_eq!(stats.node_versions_stored, 1);
        assert_eq!(stats.node_version_reads, 1);
        assert!(stats.bytes_written_raw > 0);
        assert!(stats.bytes_written_compressed > 0);
    }

    #[test]
    fn test_compression_ratio() {
        let storage = InMemoryColdStorage::new(ColdStorageConfig {
            compression: CompressionAlgorithm::Zstd,
            ..Default::default()
        });

        // Create a version with repetitive data for better compression
        let props = PropertyMapBuilder::new()
            .insert("description", "This is a test description with repetitive content that should compress well. This is a test description with repetitive content that should compress well.")
            .build();

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            props,
            VersionMetadata::default_for_existing(),
        );

        storage.store_node_version(&version).unwrap();

        let stats = storage.stats();
        let ratio = stats.compression_ratio();
        // Compression should provide some benefit
        assert!(
            ratio >= 1.0,
            "Expected compression ratio >= 1.0, got {}",
            ratio
        );
    }

    #[test]
    fn test_no_compression() {
        let storage = InMemoryColdStorage::new(ColdStorageConfig {
            compression: CompressionAlgorithm::None,
            enable_checksums: false,
            ..Default::default()
        });

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    #[test]
    fn test_fast_compression() {
        let storage = InMemoryColdStorage::new(ColdStorageConfig {
            compression: CompressionAlgorithm::Fast,
            ..Default::default()
        });

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    #[test]
    fn test_checksum_verification() {
        let storage = InMemoryColdStorage::new(ColdStorageConfig {
            compression: CompressionAlgorithm::None,
            enable_checksums: true,
            ..Default::default()
        });

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
    }

    // ========================================================================
    // FileColdStorage tests
    // ========================================================================

    #[test]
    fn test_file_storage_store_and_retrieve() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = FileColdStorage::with_default_config(temp_dir.path()).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        let retrieved = storage.get_node_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.node_id, version.node_id);
    }

    #[test]
    fn test_file_storage_edge_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = FileColdStorage::with_default_config(temp_dir.path()).unwrap();

        let version = create_test_edge_version(1);
        storage.store_edge_version(&version).unwrap();

        let retrieved = storage.get_edge_version(version.id).unwrap().unwrap();
        assert_eq!(retrieved.id, version.id);
        assert_eq!(retrieved.source, version.source);
        assert_eq!(retrieved.target, version.target);
    }

    #[test]
    fn test_file_storage_nonexistent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = FileColdStorage::with_default_config(temp_dir.path()).unwrap();

        let result = storage
            .get_node_version(VersionId::new(999).unwrap())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_file_storage_delete() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = FileColdStorage::with_default_config(temp_dir.path()).unwrap();

        let version = create_test_node_version(1);
        storage.store_node_version(&version).unwrap();

        assert!(storage.contains_node_version(version.id).unwrap());

        let deleted = storage.delete_node_version(version.id).unwrap();
        assert!(deleted);

        assert!(!storage.contains_node_version(version.id).unwrap());
    }

    #[test]
    fn test_file_storage_overwrite() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = FileColdStorage::with_default_config(temp_dir.path()).unwrap();

        let version1 = create_test_node_version(1);
        storage.store_node_version(&version1).unwrap();

        // Create a new version with the same ID but different properties
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
            VersionMetadata::default_for_existing(),
        );
        storage.store_node_version(&version2).unwrap();

        let retrieved = storage.get_node_version(version2.id).unwrap().unwrap();
        // Should get the second version
        assert_eq!(
            retrieved.temporal.transaction_time().start().wallclock(),
            2000
        );
    }

    // ========================================================================
    // Serialization tests
    // ========================================================================

    #[test]
    fn test_node_version_serialization_roundtrip() {
        let version = create_test_node_version(42);
        let encoded = encode_node_version(&version);
        let decoded = decode_node_version(&encoded).unwrap();

        assert_eq!(decoded.id, version.id);
        assert_eq!(decoded.node_id, version.node_id);
        assert_eq!(decoded.label, version.label);
    }

    #[test]
    fn test_edge_version_serialization_roundtrip() {
        let version = create_test_edge_version(42);
        let encoded = encode_edge_version(&version);
        let decoded = decode_edge_version(&encoded).unwrap();

        assert_eq!(decoded.id, version.id);
        assert_eq!(decoded.edge_id, version.edge_id);
        assert_eq!(decoded.source, version.source);
        assert_eq!(decoded.target, version.target);
    }

    #[test]
    fn test_delta_version_serialization() {
        let old_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let new_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 31i64)
            .insert("city", "NYC")
            .build();

        let version = NodeVersion::new_delta(
            VersionId::new(2).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(2000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            &old_props,
            &new_props,
            VersionId::new(1).unwrap(),
            VersionMetadata::default_for_existing(),
        );

        let encoded = encode_node_version(&version);
        let decoded = decode_node_version(&encoded).unwrap();

        assert!(decoded.is_delta());
        assert_eq!(decoded.prev_version, Some(VersionId::new(1).unwrap()));
    }

    #[test]
    fn test_version_with_vector_property() {
        use crate::core::property::PropertyValue;

        let props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector([0.1f32, 0.2, 0.3]))
            .build();

        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(100).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Document").unwrap(),
            props,
            VersionMetadata::default_for_existing(),
        );

        let encoded = encode_node_version(&version);
        let decoded = decode_node_version(&encoded).unwrap();

        assert_eq!(decoded.id, version.id);
    }

    // ========================================================================
    // Compression algorithm tests
    // ========================================================================

    #[test]
    fn test_compression_algorithm_levels() {
        assert_eq!(CompressionAlgorithm::None.zstd_level(), None);
        assert_eq!(CompressionAlgorithm::Zstd.zstd_level(), Some(3));
        assert_eq!(CompressionAlgorithm::Fast.zstd_level(), Some(1));
    }

    #[test]
    fn test_default_config() {
        let config = ColdStorageConfig::default();
        assert_eq!(config.compression, CompressionAlgorithm::Zstd);
        assert!(!config.sync_writes);
        assert!(config.enable_checksums);
        assert_eq!(config.batch_size, 1000);
    }

    // ========================================================================
    // ColdStorageStats tests
    // ========================================================================

    #[test]
    fn test_cold_storage_stats_default() {
        let stats = ColdStorageStats::default();
        assert_eq!(stats.node_versions_stored, 0);
        assert_eq!(stats.edge_versions_stored, 0);
        assert_eq!(stats.node_version_reads, 0);
        assert_eq!(stats.edge_version_reads, 0);
        assert_eq!(stats.bytes_written_raw, 0);
        assert_eq!(stats.bytes_written_compressed, 0);
        assert_eq!(stats.bytes_read_compressed, 0);
        assert_eq!(stats.bytes_read_decompressed, 0);
        assert_eq!(stats.read_errors, 0);
        assert_eq!(stats.write_errors, 0);
    }

    #[test]
    fn test_cold_storage_stats_compression_ratio_no_data() {
        let stats = ColdStorageStats::default();
        // With no data written, compression ratio should be 1.0
        assert_eq!(stats.compression_ratio(), 1.0);
    }

    #[test]
    fn test_cold_storage_stats_compression_ratio_with_data() {
        let stats = ColdStorageStats {
            bytes_written_raw: 1000,
            bytes_written_compressed: 500,
            ..Default::default()
        };
        // Ratio = raw / compressed = 1000 / 500 = 2.0
        assert_eq!(stats.compression_ratio(), 2.0);
    }

    #[test]
    fn test_cold_storage_stats_compression_ratio_no_compression() {
        let stats = ColdStorageStats {
            bytes_written_raw: 1000,
            bytes_written_compressed: 1000,
            ..Default::default()
        };
        // When raw == compressed, ratio is 1.0 (no compression benefit)
        assert_eq!(stats.compression_ratio(), 1.0);
    }

    #[test]
    fn test_cold_storage_stats_compression_ratio_good_compression() {
        let stats = ColdStorageStats {
            bytes_written_raw: 10000,
            bytes_written_compressed: 2000,
            ..Default::default()
        };
        // Ratio = 10000 / 2000 = 5.0 (5x compression)
        assert_eq!(stats.compression_ratio(), 5.0);
    }
}
