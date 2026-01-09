//! Storage subsystem for graph data.
//!
//! This module contains storage engines for both current and historical data:
//! - Current storage: Optimized for fast current-state queries (hot path)
//! - Historical storage: Anchor+delta compression for temporal queries
//! - Version management: Version chain structures and compression
//! - WAL: Write-ahead log for durability and crash recovery
//! - Persistence: Memory-mapped file storage and checkpointing

pub mod current;
pub mod historical;
pub mod observer;
pub mod persistence;
pub mod version;
pub mod wal;

// Re-export commonly used types
pub use current::{CurrentStats, CurrentStorage};
pub use historical::{HistoricalStats, HistoricalStorage};
pub use observer::{Observer, StorageEvent, StorageObserver};
pub use persistence::{Checkpoint, CheckpointConfig, PersistenceManager};
pub use version::{
    AnchorConfig, EdgeVersion, NodeVersion, PropertyDelta, VersionData, VersionMetadata,
};
pub use wal::{LSN, WalConfig, WalEntry, WalOperation, WriteAheadLog};
