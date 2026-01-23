//! Storage subsystem for graph data.
//!
//! This module contains storage engines for both current and historical data:
//! - Current storage: Optimized for fast current-state queries (hot path)
//! - Historical storage: Anchor+delta compression for temporal queries
//! - Cold storage: Disk-based tiered storage for historical versions
//! - Tiered storage: Transparent hot/warm/cold data access
//! - Version management: Version chain structures and compression
//! - WAL: Write-ahead log for durability and crash recovery
//! - Persistence: Memory-mapped file storage and checkpointing
//! - Checkpoint: Full state snapshots via index persistence
//! - Sharding: Horizontal scaling via domain-based partitioning (ADR-0014)

pub mod checkpoint;
pub mod cold_storage;
pub mod current;
pub mod historical;
pub mod index_persistence;
pub mod migration;
pub mod observer;
pub mod persistence;
#[cfg(feature = "tiered-storage")]
pub mod rocksdb_cold_storage;
pub mod sharding;
pub mod snapshot;
pub mod tiered_storage;
pub mod version;
pub mod wal;
pub mod wal_reader;

// Re-export commonly used types
pub use checkpoint::{
    CheckpointConfig as UnifiedCheckpointConfig, CheckpointManager, CheckpointStats,
};
pub use cold_storage::{
    AtomicColdStorageStats, ColdStorage, ColdStorageConfig, ColdStorageStats, CompressionAlgorithm,
    FileColdStorage, InMemoryColdStorage, decode_edge_version, decode_node_version,
    encode_edge_version, encode_node_version,
};
pub use current::{CurrentStats, CurrentStorage, DEFAULT_MAX_VECTOR_PROPERTIES, VectorIndexInfo};
pub use historical::{CacheMetrics, HistoricalStats, HistoricalStorage};
pub use migration::{
    MigrationCallback, MigrationCandidate, MigrationPolicy, MigrationProgress, MigrationService,
    MigrationStats,
};
pub use observer::{Observer, StorageEvent, StorageObserver};
pub use persistence::{Checkpoint, CheckpointConfig, PersistenceManager};
#[cfg(feature = "tiered-storage")]
pub use rocksdb_cold_storage::{RocksDBColdStorage, RocksDBConfig};
pub use snapshot::{CurrentStorageSnapshot, HistoricalStorageSnapshot, StorageSnapshot};
pub use tiered_storage::{
    LatencyPercentiles, TieredStorage, TieredStorageConfig, TieredStorageMetrics,
};
pub use version::{
    AnchorConfig, EdgeVersion, NodeVersion, PropertyDelta, VersionData, VersionMetadata,
};
pub use wal::{LSN, WalEntry, WalOperation};
pub use wal_reader::read_wal_entries;
