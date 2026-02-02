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

pub mod checkpoint;
pub mod compression;
pub mod current;
pub mod historical;
pub mod index_persistence;
pub mod migration;
pub mod persistence;
pub mod redb_cold_storage;
pub mod snapshot;
pub mod tiered_storage;
pub mod version;
pub mod wal;
pub mod wal_reader;

// Re-export commonly used types
pub use checkpoint::{
    CheckpointConfig as UnifiedCheckpointConfig, CheckpointManager, CheckpointStats,
};
pub use current::{CurrentStats, CurrentStorage, DEFAULT_MAX_VECTOR_PROPERTIES, VectorIndexInfo};
pub use historical::{CacheMetrics, HistoricalStats, HistoricalStorage};
pub use migration::{
    MigrationCallback, MigrationCandidate, MigrationPolicy, MigrationProgress, MigrationService,
    MigrationStats,
};
pub use persistence::{Checkpoint, CheckpointConfig, PersistenceManager};
pub use redb_cold_storage::{
    AtomicColdStorageStats, ColdStorageConfig, ColdStorageStats, CompressionAlgorithm,
    RedbColdStorage, RedbConfig, decode_edge_version, decode_node_version, encode_edge_version,
    encode_node_version,
};
pub use snapshot::{CurrentStorageSnapshot, HistoricalStorageSnapshot, StorageSnapshot};
pub use tiered_storage::{
    LatencyPercentiles, TieredStorage, TieredStorageConfig, TieredStorageMetrics,
};
pub use version::{AnchorConfig, EdgeVersion, NodeVersion, PropertyDelta, VersionData};
pub use wal::{LSN, WalEntry, WalOperation};
pub use wal_reader::read_wal_entries;
