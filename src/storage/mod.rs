//! Storage subsystem for graph data.
//!
//! # Architecture
//!
//! AletheiaDB uses a hybrid storage architecture to optimize for both fast current-state queries
//! and efficient historical reconstruction:
//!
//! 1.  **Current Storage** ([`current`]):
//!     - Stores the latest version of every node and edge.
//!     - Optimized for O(1) lookups and fast traversals.
//!     - Uses CSR (Compressed Sparse Row) adjacency indexes.
//!
//! 2.  **Historical Storage** ([`historical`]):
//!     - Stores all previous versions of data.
//!     - Uses **Anchor+Delta** compression to reduce storage overhead by 5-6X.
//!     - Optimized for temporal queries (point-in-time reconstruction).
//!
//! 3.  **Tiered Storage** ([`tiered_storage`]):
//!     - Manages the lifecycle of data across Hot (RAM), Warm (RAM/Disk), and Cold (Disk) tiers.
//!     - Automatically migrates older versions to cheaper storage (Redb).
//!
//! 4.  **Write-Ahead Log (WAL)** ([`wal`]):
//!     - Ensures durability and atomicity of transactions.
//!     - Uses a concurrent, lock-free ring buffer design.
//!
//! # Data Flow
//!
//! 1.  **Write**: Transaction writes to WAL (for durability) and updates Current Storage (in-memory).
//! 2.  **Version**: The previous version from Current Storage is moved to Historical Storage.
//! 3.  **Query**:
//!     - `get_node()` hits Current Storage (fast).
//!     - `get_node_at_time()` hits Historical Storage (reconstructs state).
//! 4.  **Migration**: Background tasks move old historical versions to Cold Storage.

pub mod checkpoint;
pub mod compression;
pub mod current;
pub mod historical;
pub mod index_persistence;
pub mod migration;
pub mod redb_cold_storage;
pub mod sharding;
pub mod snapshot;
pub mod tiered_storage;
pub mod wal;
pub mod wal_reader;

// Re-export commonly used types
pub use crate::core::version;
pub use crate::core::version::{
    AnchorConfig, EdgeVersion, NodeVersion, PropertyDelta, VersionData,
};
pub use checkpoint::{
    CheckpointConfig as UnifiedCheckpointConfig, CheckpointManager, CheckpointStats,
};
pub use current::{CurrentStats, CurrentStorage, DEFAULT_MAX_VECTOR_PROPERTIES, VectorIndexInfo};
pub use historical::{CacheMetrics, HistoricalStats, HistoricalStorage};
pub use migration::{
    MigrationCallback, MigrationCandidate, MigrationPolicy, MigrationProgress, MigrationService,
    MigrationStats,
};
pub use redb_cold_storage::{
    AtomicColdStorageStats, ColdStorageConfig, ColdStorageStats, CompressionAlgorithm,
    RedbColdStorage, RedbConfig, decode_edge_version, decode_node_version, encode_edge_version,
    encode_node_version,
};
pub use snapshot::{CurrentStorageSnapshot, HistoricalStorageSnapshot, StorageSnapshot};
pub use tiered_storage::{
    LatencyPercentiles, TieredStorage, TieredStorageConfig, TieredStorageMetrics,
};
pub use wal::{LSN, WalEntry, WalOperation};
pub use wal_reader::read_wal_entries;
