//! Storage subsystem for graph data.
//!
//! This module contains storage engines for both current and historical data:
//! - Current storage: Optimized for fast current-state queries (hot path)
//! - Historical storage: Anchor+delta compression for temporal queries
//! - Cold storage: Disk-based tiered storage for historical versions
//! - Tiered storage: Transparent hot/warm/cold data access
//! - Version management: Version chain structures and compression
//! - WAL: Write-ahead log for durability and crash recovery
//! - Checkpoint: Full state snapshots via index persistence
//! - Sharding: Horizontal scaling via domain-based partitioning (ADR-0014)

#[cfg(not(target_arch = "wasm32"))]
pub mod backup;
#[cfg(not(target_arch = "wasm32"))]
pub mod checkpoint;
pub(crate) mod cold_change_directory;
// Only reached from the (gated) cold-storage / index-persistence tiers, which
// use zstd — unavailable on wasm32.
#[cfg(not(target_arch = "wasm32"))]
pub mod compression;
pub mod current;
pub mod historical;
pub mod index_persistence;
#[cfg(not(target_arch = "wasm32"))]
pub mod migration;
#[cfg(not(target_arch = "wasm32"))]
pub mod redb_cold_storage;
pub mod sharding;
pub mod snapshot;
#[cfg(not(target_arch = "wasm32"))]
pub mod tiered_storage;
pub mod wal;
pub mod wal_reader;

// Re-export commonly used types
pub use crate::core::version;
pub use crate::core::version::{
    AnchorConfig, EdgeVersion, NodeVersion, PropertyDelta, VersionData,
};
#[cfg(not(target_arch = "wasm32"))]
pub use checkpoint::{
    CheckpointConfig as UnifiedCheckpointConfig, CheckpointManager, CheckpointStats,
};
pub use current::{CurrentStats, CurrentStorage, DEFAULT_MAX_VECTOR_PROPERTIES, VectorIndexInfo};
pub use historical::{CacheMetrics, HistoricalStats, HistoricalStorage};
#[cfg(not(target_arch = "wasm32"))]
pub use migration::{
    MigrationCallback, MigrationCandidate, MigrationPolicy, MigrationProgress, MigrationService,
    MigrationStats,
};
#[cfg(not(target_arch = "wasm32"))]
pub use redb_cold_storage::{
    AtomicColdStorageStats, ColdStorageConfig, ColdStorageStats, ColdTemporalExtentBounds,
    CompressionAlgorithm, RedbColdStorage, RedbConfig, decode_edge_version, decode_node_version,
    encode_edge_version, encode_node_version,
};
pub use snapshot::{CurrentStorageSnapshot, HistoricalStorageSnapshot};
#[cfg(not(target_arch = "wasm32"))]
pub use tiered_storage::{
    LatencyPercentiles, TieredStorage, TieredStorageConfig, TieredStorageMetrics,
};
pub use wal::{LSN, WalEntry, WalOperation};
pub use wal_reader::read_wal_entries;

/// Recovery logic for replaying WAL into storage.
pub(crate) mod recovery;
