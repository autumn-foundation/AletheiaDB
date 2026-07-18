//! Public API types for index persistence.

use std::path::PathBuf;
use std::time::Duration;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::formats::PersistencePolicies;

/// Configuration for index persistence.
///
/// Index persistence is **opt-in**: the default configuration is disabled.
/// To enable it, set `enabled: true` and point `data_dir` at an explicit,
/// application-owned directory:
///
/// ```ignore
/// PersistenceConfig {
///     enabled: true,
///     data_dir: my_data_dir.join("indexes"),
///     load_on_startup: true,
///     ..Default::default()
/// }
/// ```
///
/// Or use [`crate::AletheiaDB::open`] /
/// [`crate::config::durable_config_for_data_dir`], which wire this up for you
/// under a single data-directory root.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct PersistenceConfig {
    /// Whether persistence is enabled. Defaults to `false`; always pair
    /// `enabled: true` with an explicit `data_dir`.
    pub enabled: bool,
    /// Base data directory. The default `"data"` is a cwd-relative
    /// placeholder that is inert while `enabled` is `false` — override it
    /// with an explicit path whenever you enable persistence.
    pub data_dir: PathBuf,
    /// Persistence trigger policies
    pub policies: PersistencePolicies,
    /// Whether to load indexes on startup
    pub load_on_startup: bool,
    /// Whether to use memory-mapped loading
    pub use_mmap: bool,
    /// Maximum number of unique strings the process-global interner may hold.
    ///
    /// This bounds memory against a DoS via unbounded unique labels / property
    /// keys / property values (each interned string costs ~100 bytes of
    /// map/pointer overhead plus the string bytes). It is read at database
    /// `open()` and drives **both** the runtime intern cap and the persisted
    /// interner's load-validation cap, so raising it lets a grown database
    /// reopen cleanly and intern past the old limit.
    ///
    /// Defaults to [`PersistenceConfig::DEFAULT_MAX_INTERNED_STRINGS`] (10M,
    /// ~1 GB). Because it is read at `open()`, **changing it requires a
    /// restart**. The process-global interner means the last database opened in
    /// a process wins this setting.
    #[cfg_attr(feature = "serde", serde(default = "default_max_interned_strings"))]
    pub max_interned_strings: usize,
}

/// Serde field default for [`PersistenceConfig::max_interned_strings`], so a
/// partial `[persistence]` TOML table that omits the field still gets the 10M
/// default rather than `0`.
#[cfg(feature = "serde")]
fn default_max_interned_strings() -> usize {
    PersistenceConfig::DEFAULT_MAX_INTERNED_STRINGS
}

impl PersistenceConfig {
    /// Default maximum number of unique interned strings (10,000,000, ~1 GB).
    ///
    /// Kept in lockstep with
    /// [`crate::core::interning::DEFAULT_MAX_INTERNED_STRINGS`].
    pub const DEFAULT_MAX_INTERNED_STRINGS: usize = 10_000_000;
}

/// The default is **disabled** persistence (Issue #3388).
///
/// Prior to Issue #3388 the default was `enabled: true` with the
/// cwd-relative `data_dir: "data"`. Every database built from a default or
/// builder config silently wrote index snapshots into `./data` on shutdown
/// and loaded whatever `./data` happened to contain on startup — so
/// unrelated processes/tests sharing a working directory could observe each
/// other's data, and a stale `./data` could short-circuit WAL replay.
///
/// # Migration
///
/// If you relied on the old implicit default, opt in explicitly:
///
/// - set `.persistence(PersistenceConfig { enabled: true, data_dir: <your
///   path>, load_on_startup: true, ..Default::default() })` on the config
///   builder, or
/// - use [`crate::AletheiaDB::open`] (or
///   [`crate::config::durable_config_for_data_dir`]), the canonical durable
///   entry points, which always use explicit directories.
impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            data_dir: PathBuf::from("data"),
            policies: PersistencePolicies::default(),
            load_on_startup: true,
            use_mmap: true,
            max_interned_strings: Self::DEFAULT_MAX_INTERNED_STRINGS,
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
