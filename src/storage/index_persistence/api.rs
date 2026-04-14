//! Public API types for index persistence.

use std::path::PathBuf;
use std::time::Duration;

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

use super::formats::PersistencePolicies;

/// Configuration for index persistence.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
pub struct PersistenceConfig {
    /// Whether persistence is enabled
    pub enabled: bool,
    /// Base data directory
    pub data_dir: PathBuf,
    /// Persistence trigger policies
    pub policies: PersistencePolicies,
    /// Whether to load indexes on startup
    pub load_on_startup: bool,
    /// Whether to use memory-mapped loading
    pub use_mmap: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            data_dir: PathBuf::from("data"),
            policies: PersistencePolicies::default(),
            load_on_startup: true,
            use_mmap: true,
        }
    }
}

/// Statistics from a persistence operation.
#[derive(Debug, Clone)]
pub struct PersistenceStats {
    /// Time taken for persistence
    pub duration: Duration,
    /// Total bytes written
    pub bytes_written: u64,
    /// Names of indexes that were persisted
    pub indexes_persisted: Vec<String>,
}

/// Status of the persistence layer.
#[derive(Debug, Clone)]
pub struct PersistenceStatus {
    /// LSN in the manifest
    pub manifest_lsn: u64,
    /// Current database LSN
    pub current_lsn: u64,
    /// Status of each vector index
    pub vector_indexes: Vec<VectorIndexStatus>,
    /// Status of graph index
    pub graph_index: Option<IndexStatus>,
    /// Status of temporal index
    pub temporal_index: Option<IndexStatus>,
    /// Status of string interner
    pub string_interner: Option<IndexStatus>,
}

/// Status of an individual index.
#[derive(Debug, Clone)]
pub struct IndexStatus {
    /// When the index was last persisted
    pub last_persisted: Option<i64>,
    /// Number of mutations since last persist
    pub mutations_since_persist: u64,
    /// Whether the index has unpersisted changes
    pub dirty: bool,
    /// Size of the index on disk
    pub size_bytes: u64,
}

/// Status of a vector index.
#[derive(Debug, Clone)]
pub struct VectorIndexStatus {
    /// Property name
    pub property_name: String,
    /// Index status
    pub status: IndexStatus,
    /// Number of temporal snapshots
    pub snapshot_count: u32,
}
