//! Unified configuration for AletheiaDB.
//!
//! This module provides a centralized configuration system that consolidates
//! all previously hardcoded values across WAL, historical storage, and vector indexes.
//!
//! # Features
//!
//! - **`config-toml`** (enabled by default): Adds TOML file support via `from_toml_file()`,
//!   `from_toml_str()`, `to_toml_file()`, and `to_toml_string()` methods.
//!   Disable with `default-features = false` if only using programmatic configuration.
//!
//! # Example (Programmatic)
//!
//! ```ignore
//! use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder, HistoricalConfigBuilder};
//!
//! let config = AletheiaDBConfig::builder()
//!     .wal(WalConfigBuilder::new()
//!         .with_validated(32, 2048, 64 * 1024, 64 * 1024 * 1024, 10, 10).unwrap()
//!         .build())
//!     .historical(HistoricalConfigBuilder::new()
//!         .max_versions_per_entity(5000).unwrap()
//!         .max_reconstruction_depth(200).unwrap()
//!         .build())
//!     .build();
//!
//! let db = AletheiaDB::with_unified_config(config);
//! ```
//!
//! # Example (TOML - requires `config-toml` feature)
//!
//! ```ignore
//! use aletheiadb::config::AletheiaDBConfig;
//!
//! let config = AletheiaDBConfig::from_toml_file("config.toml")?;
//! let db = AletheiaDB::with_unified_config(config);
//! ```

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "config-toml")]
use std::fs;
#[cfg(feature = "config-toml")]
use std::path::Path;

use crate::storage::index_persistence::PersistenceConfig;
use crate::storage::version::AnchorConfig;

/// Configuration for WAL (Write-Ahead Log) system.
///
/// Controls buffer sizes, stripe configuration, flush behavior, and durability settings.
/// This consolidates all WAL-related configuration in one place.
/// Configuration options for the Write-Ahead Log (WAL).
///
/// # The Spark
/// The WAL is the backbone of durability in AletheiaDB. This struct allows you to tune
/// its behavior, such as concurrency (stripes), sync intervals, and directory paths,
/// to balance between latency and throughput.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[non_exhaustive]
pub struct WalConfig {
    /// Number of stripes for concurrent appends (must be power of 2).
    /// Higher values improve concurrency but use more memory.
    /// Default: 16
    pub num_stripes: usize,

    /// Ring buffer capacity per stripe.
    /// Larger capacity reduces backpressure but uses more memory.
    /// Default: 1024
    pub stripe_capacity: usize,

    /// Write buffer size for segment files (in bytes).
    /// Affects I/O batching efficiency.
    /// Default: 64KB (65536 bytes)
    pub write_buffer_size: usize,

    /// Maximum segment size before rotation (in bytes).
    /// Default: 64MB (67108864 bytes)
    pub segment_size: usize,

    /// Flush interval in milliseconds for async/group-commit modes.
    /// Lower values reduce latency but increase I/O overhead.
    /// Default: 10ms
    pub flush_interval_ms: u64,

    /// Directory where WAL files are stored.
    /// Default: "aletheiadb/wal"
    pub wal_dir: std::path::PathBuf,

    /// Number of WAL segments to keep for recovery.
    /// Default: 10
    pub segments_to_retain: usize,

    /// Durability mode controlling when data is synced to disk.
    /// This determines the tradeoff between durability guarantees and performance.
    /// Default: GroupCommit (10ms delay, 200 batch size)
    pub durability_mode: crate::storage::wal::DurabilityMode,

    /// Recovery policy for a crash-torn trailing entry (Issue #3433).
    ///
    /// When `true` (the default), WAL replay stops at a crash-torn trailing
    /// entry in the FINAL segment — the shapes a crash during append leaves: a
    /// partial header, a payload truncated past end-of-file, a zeroed/garbage
    /// op-type byte, or a checksum mismatch on a half-written payload — applying
    /// everything decoded before it and logging a warning, instead of
    /// hard-failing startup. A torn tail was never acknowledged, so discarding
    /// it is correct, not data loss.
    ///
    /// Tolerance never swallows real corruption: an undecodable entry FOLLOWED
    /// BY a valid committed entry (a valid frame after it in an encrypted
    /// segment, or a higher-LSN entry found by the plaintext forward probe) is
    /// mid-log damage, not a torn tail, and ALWAYS hard-errors — even with this
    /// flag `true`. Corruption in a non-final segment always hard-errors too.
    ///
    /// When `false`, recovery is fail-stop: every genuine-torn-tail shape above
    /// aborts startup so an operator can inspect the log manually, instead of
    /// automatic tail truncation. The ONE exception, in both modes, is an
    /// all-zero pre-allocation padding window at the end of a segment: that is
    /// treated as end-of-log, never an error (hard-erroring on it would brick
    /// normal startup).
    ///
    /// Scope: this opt-out governs the RECOVERY REPLAY path only. The #3428
    /// LSN-seeding scan (`max_lsn_in_dir`) stays torn-tail-tolerant regardless,
    /// so setting this to `false` does not re-brick the writer at seed time.
    /// Default: true
    pub tolerate_torn_tail: bool,

    /// Maximum time (milliseconds) a writer blocks on a full WAL ring buffer
    /// before failing with a diagnosable error (Issue #3798).
    ///
    /// Bounds one append CALL (a batch shares one budget, it is not re-armed
    /// per operation). A writer that blocks forever behind a dead or wedged
    /// flush thread is indistinguishable from a hung process, which is what
    /// this bound exists to prevent — it is stall DETECTION, not a latency
    /// SLA, so the default is generous enough that healthy backpressure never
    /// trips it.
    ///
    /// `0` restores the legacy unbounded block.
    /// Default: 30_000 (30s)
    pub max_append_block_ms: u64,

    /// Bound (milliseconds) on acquiring the group-commit coordinator's state
    /// mutex (Issue #3798).
    ///
    /// Deadlock DETECTION, not a performance SLA: the default is deliberately
    /// larger than the coordinator's own flush-wait timeout, so a stuck
    /// *flusher* is always reported first and this bound only fires for a
    /// genuinely stuck *mutex*. Only meaningful for the GroupCommit /
    /// AsyncBatched durability modes, which are the ones that run a
    /// coordinator.
    ///
    /// `0` restores the legacy unbounded `Mutex::lock`.
    /// Default: 120_000 (2 min)
    pub acquire_timeout_ms: u64,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            num_stripes: 16,
            stripe_capacity: 1024,
            write_buffer_size: 64 * 1024,   // 64KB
            segment_size: 64 * 1024 * 1024, // 64MB
            flush_interval_ms: 10,
            wal_dir: std::path::PathBuf::from("aletheiadb/wal"),
            segments_to_retain: 10,
            durability_mode: crate::storage::wal::DurabilityMode::group_commit_default(),
            tolerate_torn_tail: true,
            max_append_block_ms: crate::storage::wal::concurrent::DEFAULT_MAX_APPEND_BLOCK_MS,
            acquire_timeout_ms: crate::storage::wal::group_commit::DEFAULT_ACQUIRE_TIMEOUT_MS,
        }
    }
}

/// Builder for WAL configuration.
///
/// Provides a fluent API for constructing WAL configuration with validation.
#[must_use = "builders do nothing unless you call build()"]
#[derive(Debug)]
pub struct WalConfigBuilder {
    config: WalConfig,
}

impl WalConfigBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            config: WalConfig::default(),
        }
    }

    /// Set all validated parameters at once (single validation point).
    ///
    /// This is a convenience method that sets all parameters requiring validation
    /// in a single call, reducing the need for multiple `.unwrap()` calls.
    ///
    /// # Parameters
    ///
    /// - `num_stripes`: Number of stripes for concurrent appends (will be rounded to next power of 2)
    /// - `stripe_capacity`: Ring buffer capacity per stripe
    /// - `write_buffer_size`: Write buffer size in bytes
    /// - `segment_size`: Maximum segment size before rotation in bytes (minimum 512)
    /// - `segments_to_retain`: Number of WAL segments to keep for recovery
    /// - `flush_interval_ms`: Flush interval in milliseconds for async/group-commit modes
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if any parameter is invalid:
    /// - Any value is 0
    /// - `segment_size` is less than 512 bytes
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::WalConfigBuilder;
    ///
    /// let config = WalConfigBuilder::new()
    ///     .with_validated(
    ///         32,              // num_stripes
    ///         2048,            // stripe_capacity
    ///         128 * 1024,      // write_buffer_size
    ///         64 * 1024 * 1024, // segment_size
    ///         10,              // segments_to_retain
    ///         10,              // flush_interval_ms
    ///     ).unwrap()  // Single unwrap!
    ///     .build();
    /// ```
    pub fn with_validated(
        self,
        num_stripes: usize,
        stripe_capacity: usize,
        write_buffer_size: usize,
        segment_size: usize,
        segments_to_retain: usize,
        flush_interval_ms: u64,
    ) -> Result<Self, ConfigError> {
        self.num_stripes(num_stripes)?
            .stripe_capacity(stripe_capacity)?
            .write_buffer_size(write_buffer_size)?
            .segment_size(segment_size)?
            .segments_to_retain(segments_to_retain)?
            .flush_interval_ms(flush_interval_ms)
    }

    /// Set the number of stripes (will be rounded to next power of 2).
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `num_stripes` is 0.
    pub fn num_stripes(mut self, num_stripes: usize) -> Result<Self, ConfigError> {
        if num_stripes == 0 {
            return Err(ConfigError::InvalidValue(
                "num_stripes must be greater than 0".into(),
            ));
        }
        let rounded = num_stripes.next_power_of_two();
        if rounded != num_stripes {
            #[cfg(feature = "observability")]
            tracing::warn!(
                original = num_stripes,
                rounded = rounded,
                "num_stripes rounded to next power of 2"
            );
        }
        self.config.num_stripes = rounded;
        Ok(self)
    }

    /// Set the stripe capacity.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `capacity` is 0.
    pub fn stripe_capacity(mut self, capacity: usize) -> Result<Self, ConfigError> {
        if capacity == 0 {
            return Err(ConfigError::InvalidValue(
                "stripe_capacity must be greater than 0".into(),
            ));
        }
        self.config.stripe_capacity = capacity;
        Ok(self)
    }

    /// Set the write buffer size in bytes.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `size` is 0.
    pub fn write_buffer_size(mut self, size: usize) -> Result<Self, ConfigError> {
        if size == 0 {
            return Err(ConfigError::InvalidValue(
                "write_buffer_size must be greater than 0".into(),
            ));
        }
        self.config.write_buffer_size = size;
        Ok(self)
    }

    /// Set the segment size in bytes.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `size` is 0 or less than 512 bytes.
    ///
    /// **Note**: While 512 bytes is allowed (for testing), production use should
    /// be at least 1MB for reasonable performance.
    pub fn segment_size(mut self, size: usize) -> Result<Self, ConfigError> {
        const MIN_SEGMENT_SIZE: usize = 512; // Allow small sizes for testing
        if size == 0 {
            return Err(ConfigError::InvalidValue(
                "segment_size must be greater than 0".into(),
            ));
        }
        if size < MIN_SEGMENT_SIZE {
            return Err(ConfigError::InvalidValue(format!(
                "segment_size must be at least {} bytes, got {}",
                MIN_SEGMENT_SIZE, size
            )));
        }
        self.config.segment_size = size;
        Ok(self)
    }

    /// Set the flush interval in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `ms` is 0.
    pub fn flush_interval_ms(mut self, ms: u64) -> Result<Self, ConfigError> {
        if ms == 0 {
            return Err(ConfigError::InvalidValue(
                "flush_interval_ms must be greater than 0".into(),
            ));
        }
        self.config.flush_interval_ms = ms;
        Ok(self)
    }

    /// Set the WAL directory path.
    pub fn wal_dir(mut self, path: std::path::PathBuf) -> Self {
        self.config.wal_dir = path;
        self
    }

    /// Set the number of WAL segments to retain.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `segments_to_retain` is 0.
    pub fn segments_to_retain(mut self, segments: usize) -> Result<Self, ConfigError> {
        if segments == 0 {
            return Err(ConfigError::InvalidValue(
                "segments_to_retain must be greater than 0".into(),
            ));
        }
        self.config.segments_to_retain = segments;
        Ok(self)
    }

    /// Set the durability mode.
    pub fn durability_mode(mut self, mode: crate::storage::wal::DurabilityMode) -> Self {
        self.config.durability_mode = mode;
        self
    }

    /// Set the crash-torn-tail recovery policy (Issue #3433).
    ///
    /// `true` (default) tolerates a torn trailing entry in the final WAL
    /// segment on replay; `false` selects fail-stop recovery (any parse
    /// failure aborts startup). See [`WalConfig::tolerate_torn_tail`].
    pub fn tolerate_torn_tail(mut self, tolerate: bool) -> Self {
        self.config.tolerate_torn_tail = tolerate;
        self
    }

    /// Set the bound on blocking WAL appends, in milliseconds (Issue #3798).
    ///
    /// `0` restores the legacy unbounded block. Deliberately not validated
    /// against zero: that value IS the documented escape hatch. See
    /// [`WalConfig::max_append_block_ms`].
    pub fn max_append_block_ms(mut self, ms: u64) -> Self {
        self.config.max_append_block_ms = ms;
        self
    }

    /// Set the bound on acquiring the group-commit state mutex, in
    /// milliseconds (Issue #3798).
    ///
    /// `0` restores legacy unbounded acquisition. See
    /// [`WalConfig::acquire_timeout_ms`].
    pub fn acquire_timeout_ms(mut self, ms: u64) -> Self {
        self.config.acquire_timeout_ms = ms;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> WalConfig {
        self.config
    }
}

impl Default for WalConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for historical storage.
///
/// Controls versioning, reconstruction limits, and caching behavior.
/// Configuration options for Historical Storage.
///
/// # The Spark
/// To support time-travel queries, AletheiaDB keeps past versions of nodes and edges.
/// This configuration dictates how those versions are managed, including pruning
/// thresholds and the directory where historical data is stored on disk.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[non_exhaustive]
pub struct HistoricalConfig {
    /// Maximum versions to retain per entity before pruning.
    /// Higher values preserve more history but use more memory.
    /// Default: 1000
    pub max_versions_per_entity: usize,

    /// Maximum depth for delta chain reconstruction.
    /// Protects against stack overflow and infinite loops.
    /// Default: 100
    pub max_reconstruction_depth: usize,

    /// Size of the reconstruction cache (number of entries).
    /// Larger cache improves temporal query performance.
    /// Default: 10000
    pub reconstruction_cache_size: usize,

    /// Create an anchor every N versions (default: 10).
    /// Smaller intervals mean faster reconstruction but more storage.
    pub anchor_interval: u32,

    /// Maximum delta chain length before forcing an anchor (default: 20).
    /// Ensures reconstruction cost is bounded.
    pub max_delta_chain: u32,

    /// Enable cold storage (Redb-based tiered storage) for unlimited historical depth.
    /// When enabled, old versions are migrated to disk automatically.
    /// Default: false
    pub enable_cold_storage: bool,

    /// Path to the cold storage Redb file.
    /// Required if `enable_cold_storage` is true.
    /// Default: None
    pub cold_storage_path: Option<std::path::PathBuf>,

    /// Age threshold for migrating versions to cold storage.
    /// Versions older than this duration are eligible for migration.
    /// Default: 1 hour
    pub migration_age_threshold: std::time::Duration,

    /// Maximum number of hot versions to keep per entity before triggering migration.
    /// Default: 1000 (same as max_versions_per_entity)
    pub max_hot_versions: usize,

    /// Safety cap (per entity kind: nodes, edges) on the number of
    /// ever-versioned entities `AletheiaDB::schema_as_of` will reconstruct
    /// in a single call. Without a cap, a bi-temporal schema query would be
    /// an unbounded scan over every entity ever versioned. When the actual
    /// population exceeds this cap, the scan is truncated to this many
    /// entities and `GraphSchema::sampled` is set to `true` to disclose it.
    /// Default: 50000
    pub max_schema_as_of_entities: usize,
}

impl Default for HistoricalConfig {
    fn default() -> Self {
        let anchor_defaults = AnchorConfig::default();
        Self {
            max_versions_per_entity: 1000,
            max_reconstruction_depth: 100,
            reconstruction_cache_size: 10000,
            anchor_interval: anchor_defaults.anchor_interval,
            max_delta_chain: anchor_defaults.max_delta_chain,
            enable_cold_storage: false,
            cold_storage_path: None,
            migration_age_threshold: std::time::Duration::from_secs(3600), // 1 hour
            max_hot_versions: 1000,
            max_schema_as_of_entities:
                crate::storage::historical::DEFAULT_MAX_SCHEMA_AS_OF_ENTITIES,
        }
    }
}

/// Builder for historical storage configuration.
///
/// Provides a fluent API for constructing historical storage configuration with validation.
#[must_use = "builders do nothing unless you call build()"]
#[derive(Debug)]
pub struct HistoricalConfigBuilder {
    config: HistoricalConfig,
}

impl HistoricalConfigBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            config: HistoricalConfig::default(),
        }
    }

    /// Set the maximum versions per entity.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `max` is 0.
    pub fn max_versions_per_entity(mut self, max: usize) -> Result<Self, ConfigError> {
        if max == 0 {
            return Err(ConfigError::InvalidValue(
                "max_versions_per_entity must be greater than 0".into(),
            ));
        }
        self.config.max_versions_per_entity = max;
        Ok(self)
    }

    /// Set the maximum reconstruction depth.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `depth` is 0 or greater than 1000.
    pub fn max_reconstruction_depth(mut self, depth: usize) -> Result<Self, ConfigError> {
        if depth == 0 {
            return Err(ConfigError::InvalidValue(
                "max_reconstruction_depth must be greater than 0".into(),
            ));
        }
        if depth > 1000 {
            return Err(ConfigError::InvalidValue(
                "max_reconstruction_depth cannot exceed 1000 (risk of stack overflow)".into(),
            ));
        }
        self.config.max_reconstruction_depth = depth;
        Ok(self)
    }

    /// Set the reconstruction cache size.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `size` is 0.
    pub fn reconstruction_cache_size(mut self, size: usize) -> Result<Self, ConfigError> {
        if size == 0 {
            return Err(ConfigError::InvalidValue(
                "reconstruction_cache_size must be greater than 0".into(),
            ));
        }
        self.config.reconstruction_cache_size = size;
        Ok(self)
    }

    /// Set the anchor interval.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `interval` is 0.
    pub fn anchor_interval(mut self, interval: u32) -> Result<Self, ConfigError> {
        if interval == 0 {
            return Err(ConfigError::InvalidValue(
                "anchor_interval must be greater than 0".into(),
            ));
        }
        self.config.anchor_interval = interval;
        Ok(self)
    }

    /// Set the maximum delta chain length.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `max` is 0.
    pub fn max_delta_chain(mut self, max: u32) -> Result<Self, ConfigError> {
        if max == 0 {
            return Err(ConfigError::InvalidValue(
                "max_delta_chain must be greater than 0".into(),
            ));
        }
        self.config.max_delta_chain = max;
        Ok(self)
    }

    /// Enable cold storage (Redb-based tiered storage).
    ///
    /// When enabled, old versions are automatically migrated to disk, allowing
    /// unlimited historical depth without consuming RAM.
    ///
    /// # Note
    ///
    /// You must also call [`cold_storage_path`](Self::cold_storage_path) to specify
    /// where the Redb file should be stored, or the database will fail to initialize.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::HistoricalConfigBuilder;
    ///
    /// let config = HistoricalConfigBuilder::new()
    ///     .enable_cold_storage(true)
    ///     .cold_storage_path("data/cold.redb")
    ///     .build();
    /// ```
    pub fn enable_cold_storage(mut self, enabled: bool) -> Self {
        self.config.enable_cold_storage = enabled;
        self
    }

    /// Set the path to the cold storage Redb file.
    ///
    /// This is required if [`enable_cold_storage`](Self::enable_cold_storage) is true.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::HistoricalConfigBuilder;
    ///
    /// let config = HistoricalConfigBuilder::new()
    ///     .enable_cold_storage(true)
    ///     .cold_storage_path("data/cold.redb")
    ///     .build();
    /// ```
    pub fn cold_storage_path<P: Into<std::path::PathBuf>>(mut self, path: P) -> Self {
        self.config.cold_storage_path = Some(path.into());
        self
    }

    /// Set the age threshold for migrating versions to cold storage.
    ///
    /// Versions older than this duration become eligible for migration to disk.
    ///
    /// # Default
    ///
    /// 1 hour (3600 seconds)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::HistoricalConfigBuilder;
    /// use std::time::Duration;
    ///
    /// let config = HistoricalConfigBuilder::new()
    ///     .enable_cold_storage(true)
    ///     .cold_storage_path("data/cold.redb")
    ///     .migration_age_threshold(Duration::from_secs(7200)) // 2 hours
    ///     .build();
    /// ```
    pub fn migration_age_threshold(mut self, threshold: std::time::Duration) -> Self {
        self.config.migration_age_threshold = threshold;
        self
    }

    /// Set the maximum number of hot versions to keep per entity.
    ///
    /// When the number of versions exceeds this threshold, older versions
    /// are migrated to cold storage.
    ///
    /// # Default
    ///
    /// 1000 (same as `max_versions_per_entity`)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::HistoricalConfigBuilder;
    ///
    /// let config = HistoricalConfigBuilder::new()
    ///     .enable_cold_storage(true)
    ///     .cold_storage_path("data/cold.redb")
    ///     .max_hot_versions(500) // Keep only 500 versions in RAM
    ///     .build();
    /// ```
    pub fn max_hot_versions(mut self, max: usize) -> Self {
        self.config.max_hot_versions = max;
        self
    }

    /// Set the safety cap (per entity kind) on the number of ever-versioned
    /// entities `AletheiaDB::schema_as_of` will reconstruct in a single
    /// call.
    ///
    /// Without a cap, a bi-temporal schema query would be an unbounded scan
    /// over every node/edge ever versioned. When the actual population
    /// exceeds this cap, the scan is truncated and `GraphSchema::sampled` is
    /// set to `true` to disclose it -- raise this if you need exhaustive
    /// bi-temporal schema results on a large history and can afford the
    /// extra scan cost; lower it to bound worst-case latency more tightly.
    ///
    /// # Default
    ///
    /// 50000
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::HistoricalConfigBuilder;
    ///
    /// let config = HistoricalConfigBuilder::new()
    ///     .max_schema_as_of_entities(200_000) // allow a larger bi-temporal scan
    ///     .build();
    /// ```
    pub fn max_schema_as_of_entities(mut self, max: usize) -> Self {
        self.config.max_schema_as_of_entities = max;
        self
    }

    /// Build the configuration.
    ///
    /// # Panics
    ///
    /// Panics if `enable_cold_storage` is true but `cold_storage_path` is not set.
    /// Use [`build_checked`](Self::build_checked) for a non-panicking version.
    pub fn build(self) -> HistoricalConfig {
        if self.config.enable_cold_storage && self.config.cold_storage_path.is_none() {
            panic!("cold_storage_path must be set when enable_cold_storage is true");
        }
        self.config
    }

    /// Build the configuration with validation.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `enable_cold_storage` is true
    /// but `cold_storage_path` is not set.
    pub fn build_checked(self) -> Result<HistoricalConfig, ConfigError> {
        if self.config.enable_cold_storage && self.config.cold_storage_path.is_none() {
            return Err(ConfigError::InvalidValue(
                "cold_storage_path must be set when enable_cold_storage is true".into(),
            ));
        }
        Ok(self.config)
    }
}

impl Default for HistoricalConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for vector index system.
///
/// Controls limits for k-NN queries and HNSW index structure.
/// Configuration options for Vector Indexing (HNSW).
///
/// # The Spark
/// Vector search requires careful tuning of the HNSW algorithm. This struct lets you
/// configure parameters like the number of layers, connections per node, and memory
/// limits to optimize the recall-vs-latency tradeoff.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[non_exhaustive]
pub struct VectorIndexConfig {
    /// Maximum value of k for k-NN queries.
    /// Prevents excessive memory usage from large result sets.
    /// Default: 10000
    pub max_k: usize,

    /// Maximum layer depth in HNSW index.
    /// Controls the maximum graph structure depth.
    /// Default: 16
    pub max_layer: usize,
}

impl Default for VectorIndexConfig {
    fn default() -> Self {
        Self {
            max_k: 10000,
            max_layer: 16,
        }
    }
}

/// Builder for vector index configuration.
///
/// Provides a fluent API for constructing vector index configuration with validation.
#[must_use = "builders do nothing unless you call build()"]
#[derive(Debug)]
pub struct VectorIndexConfigBuilder {
    config: VectorIndexConfig,
}

impl VectorIndexConfigBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            config: VectorIndexConfig::default(),
        }
    }

    /// Set the maximum k for k-NN queries.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `k` is 0 or greater than 100,000.
    pub fn max_k(mut self, k: usize) -> Result<Self, ConfigError> {
        if k == 0 {
            return Err(ConfigError::InvalidValue(
                "max_k must be greater than 0".into(),
            ));
        }
        if k > 100_000 {
            return Err(ConfigError::InvalidValue(
                "max_k cannot exceed 100,000 (DoS protection)".into(),
            ));
        }
        self.config.max_k = k;
        Ok(self)
    }

    /// Set the maximum layer depth.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `layer` is 0 or greater than 32.
    pub fn max_layer(mut self, layer: usize) -> Result<Self, ConfigError> {
        if layer == 0 {
            return Err(ConfigError::InvalidValue(
                "max_layer must be greater than 0".into(),
            ));
        }
        if layer > 32 {
            return Err(ConfigError::InvalidValue(
                "max_layer cannot exceed 32 (HNSW limitation)".into(),
            ));
        }
        self.config.max_layer = layer;
        Ok(self)
    }

    /// Build the configuration.
    pub fn build(self) -> VectorIndexConfig {
        self.config
    }
}

impl Default for VectorIndexConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Asynchronous replication (TCP transport) configuration (Issue #3355,
/// Slice C).
///
/// Disabled by default (`listen_addr`/`primary_addr` both `None`), so a
/// database's behavior and on-disk layout are unchanged unless an operator
/// opts in. Setting `listen_addr` makes `AletheiaDB::with_unified_config`
/// automatically start a [`crate::storage::replication::ReplicationServer`]
/// (serving `FetchEntries` streaming automatically; see
/// `crate::storage::replication::tcp`'s module docs for the one caveat this
/// entails for snapshot-bootstrap serving). Setting `primary_addr` makes it
/// automatically call [`crate::db::AletheiaDB::start_replication`] with a
/// [`crate::storage::replication::TcpSource`], entering read-only replica
/// mode. The two may be set simultaneously (a serving replica).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[non_exhaustive]
pub struct ReplicationConfig {
    /// If set, `with_unified_config` starts a TCP [`crate::storage::replication::ReplicationServer`]
    /// bound to this address (e.g. `"0.0.0.0:4460"`, or `"127.0.0.1:0"` to
    /// let the OS assign a port for tests).
    pub listen_addr: Option<String>,
    /// If set, `with_unified_config` connects a [`crate::storage::replication::TcpSource`]
    /// to this primary address and starts streaming replication, entering
    /// read-only replica mode.
    pub primary_addr: Option<String>,
    /// Inline shared-secret auth token. Prefer [`Self::auth_token_env`] for
    /// anything beyond local testing (an inline token risks being checked
    /// into a config file). See [`Self::resolve_token`] for precedence.
    pub auth_token: Option<String>,
    /// Name of an environment variable to read the shared-secret auth token
    /// from at startup. Takes precedence over [`Self::auth_token`] when set.
    pub auth_token_env: Option<String>,
    /// Replica applier poll interval, in milliseconds (how often to ask the
    /// primary for new entries when caught up).
    pub poll_interval_ms: u64,
    /// Maximum WAL entries requested per replica `fetch_entries` call.
    pub batch_max_entries: usize,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            listen_addr: None,
            primary_addr: None,
            auth_token: None,
            auth_token_env: None,
            poll_interval_ms: 50,
            batch_max_entries: 500,
        }
    }
}

impl ReplicationConfig {
    /// Fluent builder over [`ReplicationConfig::default`].
    pub fn builder() -> ReplicationConfigBuilder {
        ReplicationConfigBuilder::new()
    }

    /// Resolve the shared-secret auth token to use when starting a
    /// [`crate::storage::replication::ReplicationServer`] or
    /// [`crate::storage::replication::TcpSource`].
    ///
    /// Precedence: [`Self::auth_token_env`] (an environment variable name)
    /// wins over the inline [`Self::auth_token`] when both are set. This
    /// makes `auth_token_env` the preferred production path -- the token
    /// itself never has to touch a config file -- while `auth_token` remains
    /// available for local development/tests.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidValue`] if `auth_token_env` names a
    /// variable that is unset or empty, or if neither field resolves to a
    /// nonempty token. Callers are expected to invoke this only when at
    /// least one of `listen_addr`/`primary_addr` is set (an operator turning
    /// on replication without configuring a token is a startup-time
    /// misconfiguration, not a silent anonymous-access fallback).
    pub fn resolve_token(&self) -> std::result::Result<String, ConfigError> {
        if let Some(var) = &self.auth_token_env {
            return match std::env::var(var) {
                Ok(v) if !v.is_empty() => Ok(v),
                Ok(_) => Err(ConfigError::InvalidValue(format!(
                    "replication.auth_token_env names environment variable '{var}', but it is \
                     set to an empty string"
                ))),
                Err(_) => Err(ConfigError::InvalidValue(format!(
                    "replication.auth_token_env names environment variable '{var}', but it is \
                     not set"
                ))),
            };
        }
        match &self.auth_token {
            Some(token) if !token.is_empty() => Ok(token.clone()),
            _ => Err(ConfigError::InvalidValue(
                "replication requires auth_token or auth_token_env to be set whenever \
                 listen_addr or primary_addr is configured"
                    .to_string(),
            )),
        }
    }
}

/// Builder for [`ReplicationConfig`].
#[must_use = "builders do nothing unless you call build()"]
#[derive(Debug, Clone)]
pub struct ReplicationConfigBuilder {
    config: ReplicationConfig,
}

impl ReplicationConfigBuilder {
    /// Create a new builder with default (fully disabled) values.
    pub fn new() -> Self {
        Self {
            config: ReplicationConfig::default(),
        }
    }

    /// Start a [`crate::storage::replication::ReplicationServer`] on this
    /// address.
    pub fn listen_addr(mut self, addr: impl Into<String>) -> Self {
        self.config.listen_addr = Some(addr.into());
        self
    }

    /// Stream replication from a primary at this address.
    pub fn primary_addr(mut self, addr: impl Into<String>) -> Self {
        self.config.primary_addr = Some(addr.into());
        self
    }

    /// Set the inline shared-secret auth token (see [`ReplicationConfig::auth_token`]).
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.config.auth_token = Some(token.into());
        self
    }

    /// Name an environment variable to resolve the auth token from at
    /// startup (see [`ReplicationConfig::auth_token_env`]).
    pub fn auth_token_env(mut self, var: impl Into<String>) -> Self {
        self.config.auth_token_env = Some(var.into());
        self
    }

    /// Set the replica applier poll interval, in milliseconds.
    pub fn poll_interval_ms(mut self, ms: u64) -> Self {
        self.config.poll_interval_ms = ms;
        self
    }

    /// Set the maximum WAL entries requested per replica fetch call.
    pub fn batch_max_entries(mut self, n: usize) -> Self {
        self.config.batch_max_entries = n;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> ReplicationConfig {
        self.config
    }
}

impl Default for ReplicationConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Unified configuration for AletheiaDB.
///
/// This consolidates all configuration settings for the database,
/// making it easy to tune for different deployment scenarios.
/// The root configuration structure for AletheiaDB.
///
/// # The Spark
/// This is the master configuration object that aggregates all subsystem configs
/// (WAL, Historical, Vector, Persistence). It acts as the single source of truth
/// when bootstrapping a new database instance.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[non_exhaustive]
pub struct AletheiaDBConfig {
    /// WAL configuration
    pub wal: WalConfig,
    /// Historical storage configuration
    pub historical: HistoricalConfig,
    /// Vector index configuration
    pub vector: VectorIndexConfig,
    /// Index persistence configuration
    pub persistence: PersistenceConfig,
    /// Encryption at rest configuration
    pub encryption: crate::encryption::config::EncryptionConfig,
    /// Opt-in tamper-evident provenance hash chain (Issue #3351). Disabled by
    /// default, so a database keeps byte-identical behavior and on-disk layout
    /// unless the chain is explicitly enabled.
    pub chain: crate::provenance_chain::ChainConfig,
    /// Push-changefeed caps, including the per-principal subscription quota
    /// (Issue #3678). Governs the global subscription cap, per-subscription
    /// buffer, and the default + per-principal-override fairness limits enforced
    /// by every changefeed surface (MCP `await_changes`, HTTP `/changes/await`
    /// and `/changes/stream`).
    pub changefeed: crate::core::changefeed_subscription::ChangefeedConfig,
    /// Engine-lane per-query resource limits (Issue #3368): server default +
    /// operator ceiling for wall-clock timeout, result-row cap, and memory
    /// budget, enforced by [`crate::query::executor::QueryExecutor`] and
    /// overridable per-call via [`crate::query::QueryBuilder::with_timeout`]/
    /// [`with_max_rows`](crate::query::QueryBuilder::with_max_rows)/
    /// [`with_memory_budget`](crate::query::QueryBuilder::with_memory_budget).
    /// Defaults to [`EngineQueryLimitsConfig::default`](crate::query::limits::EngineQueryLimitsConfig::default)
    /// (enabled, generous ceilings) so existing behavior is unaffected.
    pub query_limits: crate::query::limits::EngineQueryLimitsConfig,
    /// Asynchronous replication (TCP transport) configuration (Issue #3355,
    /// Slice C). Disabled by default (both `listen_addr` and `primary_addr`
    /// unset), so a database's behavior is unchanged unless an operator opts
    /// in. See [`ReplicationConfig`] for what each field wires up.
    pub replication: ReplicationConfig,
    /// Background adjacency-index maintenance (Issue #3810). **Enabled by
    /// default**: a shared, process-wide worker compacts the delta buffer into
    /// the frozen CSR once writes go quiet, which is the only way reads reach
    /// the ADR-0026 frozen fast path. Disable with
    /// [`AdjacencyMaintenanceConfig::disabled`] to keep compaction strictly
    /// explicit (`AletheiaDB::compact_adjacency`).
    pub adjacency: crate::index::adjacency_maintenance::AdjacencyMaintenanceConfig,
}

/// Builder for unified database configuration.
///
/// Provides a fluent API for constructing complete database configuration.
#[must_use = "builders do nothing unless you call build()"]
#[derive(Debug)]
pub struct AletheiaDBConfigBuilder {
    config: AletheiaDBConfig,
}

impl AletheiaDBConfigBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            config: AletheiaDBConfig::default(),
        }
    }

    /// Set WAL configuration.
    pub fn wal(mut self, wal_config: WalConfig) -> Self {
        self.config.wal = wal_config;
        self
    }

    /// Set historical storage configuration.
    pub fn historical(mut self, historical_config: HistoricalConfig) -> Self {
        self.config.historical = historical_config;
        self
    }

    /// Set vector index configuration.
    pub fn vector(mut self, vector_config: VectorIndexConfig) -> Self {
        self.config.vector = vector_config;
        self
    }

    /// Set persistence configuration.
    pub fn persistence(mut self, persistence_config: PersistenceConfig) -> Self {
        self.config.persistence = persistence_config;
        self
    }

    /// Set the background adjacency-maintenance policy (Issue #3810).
    pub fn adjacency(
        mut self,
        adjacency_config: crate::index::adjacency_maintenance::AdjacencyMaintenanceConfig,
    ) -> Self {
        self.config.adjacency = adjacency_config;
        self
    }

    /// Set encryption at rest configuration.
    pub fn encryption(
        mut self,
        encryption_config: crate::encryption::config::EncryptionConfig,
    ) -> Self {
        self.config.encryption = encryption_config;
        self
    }

    /// Set the provenance hash chain configuration (Issue #3351).
    ///
    /// The default is disabled; passing a [`ChainConfig`](crate::provenance_chain::ChainConfig)
    /// with `enabled: true` opts the database into the tamper-evident sidecar
    /// chain over its recorded history.
    pub fn chain(mut self, chain_config: crate::provenance_chain::ChainConfig) -> Self {
        self.config.chain = chain_config;
        self
    }

    /// Set the push-changefeed configuration, including the per-principal
    /// subscription quota (Issue #3678).
    pub fn changefeed(
        mut self,
        changefeed_config: crate::core::changefeed_subscription::ChangefeedConfig,
    ) -> Self {
        self.config.changefeed = changefeed_config;
        self
    }

    /// Set engine-lane per-query resource limits (Issue #3368).
    pub fn query_limits(
        mut self,
        query_limits_config: crate::query::limits::EngineQueryLimitsConfig,
    ) -> Self {
        self.config.query_limits = query_limits_config;
        self
    }

    /// Set the asynchronous replication (TCP transport) configuration
    /// (Issue #3355, Slice C).
    pub fn replication(mut self, replication_config: ReplicationConfig) -> Self {
        self.config.replication = replication_config;
        self
    }

    /// Build the unified configuration.
    pub fn build(self) -> AletheiaDBConfig {
        self.config
    }
}

impl Default for AletheiaDBConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AletheiaDBConfig {
    /// Create a new builder for configuration.
    pub fn builder() -> AletheiaDBConfigBuilder {
        AletheiaDBConfigBuilder::new()
    }

    /// Load configuration from a TOML file.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::AletheiaDBConfig;
    ///
    /// let config = AletheiaDBConfig::from_toml_file("config.toml")?;
    /// ```
    #[cfg(feature = "config-toml")]
    pub fn from_toml_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let contents =
            fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::IoError(e.to_string()))?;
        Self::from_toml_str(&contents)
    }

    /// Parse configuration from a TOML string.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::AletheiaDBConfig;
    ///
    /// let toml_str = r#"
    /// [wal]
    /// num_stripes = 32
    /// stripe_capacity = 2048
    /// "#;
    ///
    /// let config = AletheiaDBConfig::from_toml_str(toml_str)?;
    /// ```
    #[cfg(feature = "config-toml")]
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::ParseError(e.to_string()))
    }

    /// Save configuration to a TOML file.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::config::AletheiaDBConfig;
    ///
    /// let config = AletheiaDBConfig::default();
    /// config.to_toml_file("config.toml")?;
    /// ```
    #[cfg(feature = "config-toml")]
    pub fn to_toml_file<P: AsRef<Path>>(&self, path: P) -> Result<(), ConfigError> {
        let toml_string = self.to_toml_string()?;
        fs::write(path.as_ref(), toml_string).map_err(|e| ConfigError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Convert configuration to a TOML string.
    #[cfg(feature = "config-toml")]
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::SerializeError(e.to_string()))
    }
}

/// Errors that can occur when loading or saving configuration.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// I/O error when reading or writing file.
    #[error("I/O error: {0}")]
    IoError(String),
    /// Error parsing TOML.
    #[error("Parse error: {0}")]
    ParseError(String),
    /// Error serializing to TOML.
    #[error("Serialize error: {0}")]
    SerializeError(String),
    /// Invalid configuration value.
    #[error("Invalid value: {0}")]
    InvalidValue(String),
}

// ---------------------------------------------------------------------------
// Environment-driven configuration
// ---------------------------------------------------------------------------

/// Name of the environment variable that, when set, points all exposed binaries
/// (`aletheia-server`, `aletheia-mcp`, `aletheia` CLI) and the Python SDK at a
/// durable data directory.
pub const DATA_DIR_ENV: &str = "ALETHEIADB_DATA_DIR";

/// Name of the environment variable that, when set, points all exposed binaries
/// and the Python SDK at a TOML config file (loaded via
/// [`AletheiaDBConfig::from_toml_file`]). Takes precedence over [`DATA_DIR_ENV`].
pub const CONFIG_ENV: &str = "ALETHEIADB_CONFIG";

/// Read the data directory from [`DATA_DIR_ENV`].
///
/// Returns `Some(path)` when the variable is set to a non-empty value
/// (whitespace is trimmed). Unset or empty resolves to `None`, signalling
/// the caller should fall back to ephemeral storage.
#[must_use]
pub fn data_dir_from_env() -> Option<std::path::PathBuf> {
    std::env::var(DATA_DIR_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
}

/// Read the TOML config path from [`CONFIG_ENV`].
///
/// Same semantics as [`data_dir_from_env`]: unset or empty → `None`.
#[must_use]
pub fn config_path_from_env() -> Option<std::path::PathBuf> {
    std::env::var(CONFIG_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
}

/// Build a canonical durable [`AletheiaDBConfig`] rooted at `data_dir`.
///
/// The shape — `{data_dir}/wal` for the WAL, `{data_dir}/indexes` for index
/// persistence, group-commit durability, and `load_on_startup = true` so a
/// restart replays prior state — is what every exposed binary (HTTP server,
/// MCP server, CLI, Python SDK) uses when `ALETHEIADB_DATA_DIR` is set.
/// Centralised here so the binaries don't drift out of sync.
#[must_use]
pub fn durable_config_for_data_dir(data_dir: impl Into<std::path::PathBuf>) -> AletheiaDBConfig {
    use crate::storage::index_persistence::PersistenceConfig;
    use crate::storage::wal::DurabilityMode;

    let data_dir = data_dir.into();
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::GroupCommit {
                    max_delay_ms: 10,
                    max_batch_size: 200,
                })
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wal_config_defaults() {
        let config = WalConfig::default();
        assert_eq!(config.num_stripes, 16);
        assert_eq!(config.stripe_capacity, 1024);
        assert_eq!(config.write_buffer_size, 64 * 1024);
        assert_eq!(config.segment_size, 64 * 1024 * 1024);
        assert_eq!(config.flush_interval_ms, 10);
    }

    #[test]
    fn test_wal_config_builder() {
        let config = WalConfigBuilder::new()
            .num_stripes(32)
            .unwrap()
            .stripe_capacity(2048)
            .unwrap()
            .write_buffer_size(128 * 1024)
            .unwrap()
            .segment_size(128 * 1024 * 1024)
            .unwrap()
            .flush_interval_ms(20)
            .unwrap()
            .build();

        assert_eq!(config.num_stripes, 32);
        assert_eq!(config.stripe_capacity, 2048);
        assert_eq!(config.write_buffer_size, 128 * 1024);
        assert_eq!(config.segment_size, 128 * 1024 * 1024);
        assert_eq!(config.flush_interval_ms, 20);
    }

    #[test]
    fn test_wal_config_builder_rounds_stripes_to_power_of_two() {
        let config = WalConfigBuilder::new()
            .num_stripes(30)
            .unwrap() // Not a power of 2
            .build();

        assert_eq!(config.num_stripes, 32); // Rounded up to next power of 2
    }

    #[test]
    fn test_wal_config_with_validated() {
        let config = WalConfigBuilder::new()
            .with_validated(
                32,               // num_stripes
                2048,             // stripe_capacity
                128 * 1024,       // write_buffer_size
                64 * 1024 * 1024, // segment_size
                10,               // segments_to_retain
                20,               // flush_interval_ms
            )
            .unwrap() // Single unwrap!
            .build();

        assert_eq!(config.num_stripes, 32);
        assert_eq!(config.stripe_capacity, 2048);
        assert_eq!(config.write_buffer_size, 128 * 1024);
        assert_eq!(config.segment_size, 64 * 1024 * 1024);
        assert_eq!(config.segments_to_retain, 10);
        assert_eq!(config.flush_interval_ms, 20);
    }

    #[test]
    fn test_wal_config_with_validated_invalid() {
        // Test that invalid values are caught
        let result = WalConfigBuilder::new().with_validated(
            0, // invalid: 0 stripes
            2048,
            128 * 1024,
            64 * 1024 * 1024,
            10,
            20,
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_defaults() {
        let config = HistoricalConfig::default();
        assert_eq!(config.max_versions_per_entity, 1000);
        assert_eq!(config.max_reconstruction_depth, 100);
        assert_eq!(config.reconstruction_cache_size, 10000);
        assert_eq!(config.anchor_interval, 10);
        assert_eq!(config.max_delta_chain, 20);
        assert_eq!(config.max_schema_as_of_entities, 50_000);
    }

    #[test]
    fn test_historical_config_builder() {
        let config = HistoricalConfigBuilder::new()
            .max_versions_per_entity(5000)
            .unwrap()
            .max_reconstruction_depth(200)
            .unwrap()
            .reconstruction_cache_size(20000)
            .unwrap()
            .anchor_interval(5)
            .unwrap()
            .max_delta_chain(10)
            .unwrap()
            .max_schema_as_of_entities(100_000)
            .build();

        assert_eq!(config.max_versions_per_entity, 5000);
        assert_eq!(config.max_reconstruction_depth, 200);
        assert_eq!(config.reconstruction_cache_size, 20000);
        assert_eq!(config.anchor_interval, 5);
        assert_eq!(config.max_delta_chain, 10);
        assert_eq!(config.max_schema_as_of_entities, 100_000);
    }

    #[test]
    fn test_vector_config_defaults() {
        let config = VectorIndexConfig::default();
        assert_eq!(config.max_k, 10000);
        assert_eq!(config.max_layer, 16);
    }

    #[test]
    fn test_vector_config_builder() {
        let config = VectorIndexConfigBuilder::new()
            .max_k(20000)
            .unwrap()
            .max_layer(32)
            .unwrap()
            .build();

        assert_eq!(config.max_k, 20000);
        assert_eq!(config.max_layer, 32);
    }

    #[test]
    fn test_unified_config_defaults() {
        let config = AletheiaDBConfig::default();
        assert_eq!(config.wal, WalConfig::default());
        assert_eq!(config.historical, HistoricalConfig::default());
        assert_eq!(config.vector, VectorIndexConfig::default());
    }

    #[test]
    fn test_unified_config_builder() {
        let config = AletheiaDBConfig::builder()
            .wal(WalConfigBuilder::new().num_stripes(32).unwrap().build())
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(5000)
                    .unwrap()
                    .build(),
            )
            .vector(
                VectorIndexConfigBuilder::new()
                    .max_k(20000)
                    .unwrap()
                    .build(),
            )
            .build();

        assert_eq!(config.wal.num_stripes, 32);
        assert_eq!(config.historical.max_versions_per_entity, 5000);
        assert_eq!(config.vector.max_k, 20000);
    }

    #[test]
    fn test_wal_config_fluent_api() {
        // Test that builder methods return self for chaining
        let config = WalConfigBuilder::new()
            .num_stripes(8)
            .unwrap()
            .stripe_capacity(512)
            .unwrap()
            .build();

        assert_eq!(config.num_stripes, 8);
        assert_eq!(config.stripe_capacity, 512);
    }

    #[test]
    fn test_embedded_system_config() {
        // Embedded systems need smaller buffers
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .num_stripes(4)
                    .unwrap()
                    .stripe_capacity(256)
                    .unwrap()
                    .write_buffer_size(16 * 1024)
                    .unwrap()
                    .segment_size(16 * 1024 * 1024)
                    .unwrap()
                    .build(),
            )
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(100)
                    .unwrap()
                    .reconstruction_cache_size(1000)
                    .unwrap()
                    .build(),
            )
            .build();

        assert_eq!(config.wal.num_stripes, 4);
        assert_eq!(config.wal.write_buffer_size, 16 * 1024);
        assert_eq!(config.historical.max_versions_per_entity, 100);
    }

    #[test]
    fn test_cloud_deployment_config() {
        // Cloud deployments can afford larger capacities
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .num_stripes(64)
                    .unwrap()
                    .stripe_capacity(4096)
                    .unwrap()
                    .write_buffer_size(256 * 1024)
                    .unwrap()
                    .segment_size(256 * 1024 * 1024)
                    .unwrap()
                    .build(),
            )
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(10000)
                    .unwrap()
                    .reconstruction_cache_size(100000)
                    .unwrap()
                    .build(),
            )
            .build();

        assert_eq!(config.wal.num_stripes, 64);
        assert_eq!(config.wal.write_buffer_size, 256 * 1024);
        assert_eq!(config.historical.max_versions_per_entity, 10000);
    }

    #[test]
    fn test_batch_processing_config() {
        // Batch processing needs different flush intervals
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .flush_interval_ms(100)
                    .unwrap() // Longer interval for batching
                    .build(),
            )
            .build();

        assert_eq!(config.wal.flush_interval_ms, 100);
    }

    // TOML configuration tests

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_serialization() {
        let config = AletheiaDBConfig::default();
        let toml_string = config.to_toml_string().unwrap();

        // Should contain all sections
        assert!(toml_string.contains("[wal]"));
        assert!(toml_string.contains("[historical]"));
        assert!(toml_string.contains("[vector]"));

        // Should contain some key values
        assert!(toml_string.contains("num_stripes"));
        assert!(toml_string.contains("max_versions_per_entity"));
        assert!(toml_string.contains("max_k"));
        assert!(toml_string.contains("anchor_interval"));
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_chain_section_round_trips() {
        // The opt-in provenance hash chain (Issue #3351) must round-trip
        // through TOML via the `[chain]` section so `aletheia verify` can be
        // driven by an on-disk config that enables the chain.
        use crate::provenance_chain::ChainFsyncMode;

        let toml_str = r#"
[chain]
enabled = true
fsync = "per_transaction"
dir = "/var/lib/aletheia/chain"
        "#;

        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
        assert!(config.chain.enabled, "[chain] enabled must deserialize");
        assert_eq!(config.chain.fsync, ChainFsyncMode::PerTransaction);
        assert_eq!(
            config.chain.dir,
            Some(std::path::PathBuf::from("/var/lib/aletheia/chain"))
        );

        // Serialize back out and confirm the section is present and re-parses
        // to an identical config (full round-trip, no field loss).
        let rendered = config.to_toml_string().unwrap();
        assert!(rendered.contains("[chain]"), "rendered TOML: {rendered}");
        assert!(rendered.contains("enabled = true"));
        let reparsed = AletheiaDBConfig::from_toml_str(&rendered).unwrap();
        assert_eq!(reparsed.chain, config.chain);

        // Omitting the section keeps the chain disabled by default (byte-
        // identical behavior for existing configs).
        let no_chain = AletheiaDBConfig::from_toml_str("[wal]\nnum_stripes = 16\n").unwrap();
        assert!(!no_chain.chain.enabled);
        assert_eq!(no_chain.chain.fsync, ChainFsyncMode::Batched);
        assert!(no_chain.chain.dir.is_none());
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_deserialization_partial() {
        // Test partial config - only override WAL settings
        let toml_str = r#"
[wal]
num_stripes = 32
stripe_capacity = 2048

[historical]
max_versions_per_entity = 1000
max_reconstruction_depth = 100
reconstruction_cache_size = 10000

[vector]
max_k = 10000
max_layer = 16
        "#;

        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

        assert_eq!(config.wal.num_stripes, 32);
        assert_eq!(config.wal.stripe_capacity, 2048);
        // Other values should have defaults
        assert_eq!(config.wal.write_buffer_size, 64 * 1024);
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_deserialization_complete() {
        let toml_str = r#"
[wal]
num_stripes = 32
stripe_capacity = 2048
write_buffer_size = 131072
segment_size = 134217728
flush_interval_ms = 20

[historical]
max_versions_per_entity = 5000
max_reconstruction_depth = 200
reconstruction_cache_size = 20000
anchor_interval = 5
max_delta_chain = 10

[vector]
max_k = 20000
max_layer = 32
        "#;

        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

        // WAL config
        assert_eq!(config.wal.num_stripes, 32);
        assert_eq!(config.wal.stripe_capacity, 2048);
        assert_eq!(config.wal.write_buffer_size, 131072);
        assert_eq!(config.wal.segment_size, 134217728);
        assert_eq!(config.wal.flush_interval_ms, 20);

        // Historical config
        assert_eq!(config.historical.max_versions_per_entity, 5000);
        assert_eq!(config.historical.max_reconstruction_depth, 200);
        assert_eq!(config.historical.reconstruction_cache_size, 20000);
        assert_eq!(config.historical.anchor_interval, 5);
        assert_eq!(config.historical.max_delta_chain, 10);

        // Vector config
        assert_eq!(config.vector.max_k, 20000);
        assert_eq!(config.vector.max_layer, 32);
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_round_trip() {
        // Create config with custom values
        let original = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .num_stripes(32)
                    .unwrap()
                    .stripe_capacity(2048)
                    .unwrap()
                    .build(),
            )
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(5000)
                    .unwrap()
                    .build(),
            )
            .vector(
                VectorIndexConfigBuilder::new()
                    .max_k(20000)
                    .unwrap()
                    .build(),
            )
            .build();

        // Serialize to TOML
        let toml_string = original.to_toml_string().unwrap();

        // Deserialize back
        let deserialized = AletheiaDBConfig::from_toml_str(&toml_string).unwrap();

        // Should be equal
        assert_eq!(original, deserialized);
    }

    /// Issue #3798 review round: the two WAL stall bounds are documented with
    /// a `0 = restore the legacy unbounded behavior` escape hatch, which is
    /// only real if an operator can actually set them from a config file.
    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_carries_the_wal_stall_bounds() {
        let toml_str = r#"
[wal]
max_append_block_ms = 0
acquire_timeout_ms = 250
        "#;

        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(
            config.wal.max_append_block_ms, 0,
            "`0` must survive as the documented unbounded escape hatch"
        );
        assert_eq!(config.wal.acquire_timeout_ms, 250);

        // Omitting them keeps the shipped defaults...
        let defaults = AletheiaDBConfig::from_toml_str("[wal]\nnum_stripes = 16\n").unwrap();
        assert_eq!(defaults.wal.max_append_block_ms, 30_000);
        assert_eq!(defaults.wal.acquire_timeout_ms, 120_000);

        // ...and both survive a serialize/parse round trip.
        let rendered = config.to_toml_string().unwrap();
        let reparsed = AletheiaDBConfig::from_toml_str(&rendered).unwrap();
        assert_eq!(reparsed.wal, config.wal);
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_file_save_and_load() {
        use tempfile::NamedTempFile;

        let config = AletheiaDBConfig::builder()
            .wal(WalConfigBuilder::new().num_stripes(64).unwrap().build())
            .build();

        // Create a temporary file
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Save to file
        config.to_toml_file(path).unwrap();

        // Load from file
        let loaded = AletheiaDBConfig::from_toml_file(path).unwrap();

        // Should be equal
        assert_eq!(config, loaded);
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_embedded_system_example() {
        let toml_str = r#"
# Embedded system configuration
[wal]
num_stripes = 4
stripe_capacity = 256
write_buffer_size = 16384
segment_size = 16777216

[historical]
max_versions_per_entity = 100
max_reconstruction_depth = 50
reconstruction_cache_size = 1000

[vector]
max_k = 1000
max_layer = 8
        "#;

        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

        assert_eq!(config.wal.num_stripes, 4);
        assert_eq!(config.wal.stripe_capacity, 256);
        assert_eq!(config.historical.max_versions_per_entity, 100);
        assert_eq!(config.vector.max_k, 1000);
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_cloud_deployment_example() {
        let toml_str = r#"
# Cloud deployment configuration
[wal]
num_stripes = 64
stripe_capacity = 4096
write_buffer_size = 262144
segment_size = 268435456

[historical]
max_versions_per_entity = 10000
max_reconstruction_depth = 200
reconstruction_cache_size = 100000

[vector]
max_k = 50000
max_layer = 24
        "#;

        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

        assert_eq!(config.wal.num_stripes, 64);
        assert_eq!(config.wal.stripe_capacity, 4096);
        assert_eq!(config.historical.max_versions_per_entity, 10000);
        assert_eq!(config.vector.max_k, 50000);
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_parse_error() {
        let invalid_toml = "this is not valid toml {]";
        let result = AletheiaDBConfig::from_toml_str(invalid_toml);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::ParseError(_)));
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_durability_mode_group_commit() {
        let toml_str = r#"
[wal]
num_stripes = 32

[wal.durability_mode.GroupCommit]
max_delay_ms = 10
max_batch_size = 200
        "#;
        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(config.wal.num_stripes, 32);
        match config.wal.durability_mode {
            crate::storage::wal::DurabilityMode::GroupCommit {
                max_delay_ms,
                max_batch_size,
            } => {
                assert_eq!(max_delay_ms, 10);
                assert_eq!(max_batch_size, 200);
            }
            _ => panic!("Expected GroupCommit durability mode"),
        }
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_durability_mode_async() {
        let toml_str = r#"
[wal]
[wal.durability_mode.Async]
flush_interval_ms = 100
        "#;
        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
        match config.wal.durability_mode {
            crate::storage::wal::DurabilityMode::Async { flush_interval_ms } => {
                assert_eq!(flush_interval_ms, 100);
            }
            _ => panic!("Expected Async durability mode"),
        }
    }

    #[test]
    #[cfg(feature = "config-toml")]
    fn test_toml_wal_dir() {
        use std::path::PathBuf;
        let toml_str = r#"
[wal]
wal_dir = "/custom/path/to/wal"
        "#;
        let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(config.wal.wal_dir, PathBuf::from("/custom/path/to/wal"));
    }

    /// Issue #3388: with `#[serde(default)]`, a TOML `[persistence]` section
    /// that sets other fields but omits `enabled` must inherit the disabled
    /// default — it must not silently re-enable persistence.
    #[test]
    #[cfg(feature = "config-toml")]
    fn toml_persistence_omitting_enabled_stays_disabled() {
        let c = AletheiaDBConfig::from_toml_str("[persistence]\ndata_dir = \"custom\"\n").unwrap();
        assert!(
            !c.persistence.enabled,
            "TOML [persistence] without `enabled` must inherit the disabled default (Issue #3388)"
        );
        let c = AletheiaDBConfig::from_toml_str("").unwrap();
        assert!(!c.persistence.enabled);
    }

    /// Configurable interner cap: the default `max_interned_strings` is 10M and
    /// matches the interner's own default constant (they move in lockstep).
    #[test]
    fn persistence_default_max_interned_strings_is_ten_million() {
        use crate::storage::index_persistence::PersistenceConfig;
        assert_eq!(
            PersistenceConfig::default().max_interned_strings,
            10_000_000
        );
        assert_eq!(PersistenceConfig::DEFAULT_MAX_INTERNED_STRINGS, 10_000_000);
        assert_eq!(
            PersistenceConfig::DEFAULT_MAX_INTERNED_STRINGS,
            crate::core::interning::DEFAULT_MAX_INTERNED_STRINGS,
        );
    }

    /// Configurable interner cap: `max_interned_strings` round-trips through the
    /// unified config builder.
    #[test]
    fn persistence_max_interned_strings_builder_round_trip() {
        use crate::storage::index_persistence::PersistenceConfig;
        let config = AletheiaDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                max_interned_strings: 1234,
                ..Default::default()
            })
            .build();
        assert_eq!(config.persistence.max_interned_strings, 1234);
    }

    /// Configurable interner cap: `max_interned_strings` round-trips through
    /// TOML, and a partial `[persistence]` table that omits it inherits the 10M
    /// default (field-level serde default).
    #[test]
    #[cfg(feature = "config-toml")]
    fn persistence_max_interned_strings_toml_round_trip() {
        let c = AletheiaDBConfig::from_toml_str(
            "[persistence]\nenabled = true\nmax_interned_strings = 1234\n",
        )
        .unwrap();
        assert_eq!(c.persistence.max_interned_strings, 1234);

        // Omitted field → 10M default (not 0).
        let c = AletheiaDBConfig::from_toml_str("[persistence]\ndata_dir = \"custom\"\n").unwrap();
        assert_eq!(c.persistence.max_interned_strings, 10_000_000);
    }

    /// The canonical durable entry point must keep persistence enabled with
    /// explicit directories, unaffected by the Issue #3388 default flip.
    #[test]
    fn durable_config_keeps_persistence_enabled() {
        let c = durable_config_for_data_dir("/some/root");
        assert!(c.persistence.enabled);
        assert_eq!(
            c.persistence.data_dir,
            std::path::Path::new("/some/root/indexes")
        );
        assert!(c.persistence.load_on_startup);
    }

    // Validation error tests

    #[test]
    fn test_wal_config_zero_num_stripes() {
        let result = WalConfigBuilder::new().num_stripes(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_wal_config_zero_stripe_capacity() {
        let result = WalConfigBuilder::new().stripe_capacity(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_wal_config_zero_write_buffer_size() {
        let result = WalConfigBuilder::new().write_buffer_size(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_wal_config_zero_segment_size() {
        let result = WalConfigBuilder::new().segment_size(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_wal_config_segment_size_too_small() {
        let result = WalConfigBuilder::new().segment_size(256); // Less than 512 bytes
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_wal_config_zero_segments_to_retain() {
        let result = WalConfigBuilder::new().segments_to_retain(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_wal_config_zero_flush_interval() {
        let result = WalConfigBuilder::new().flush_interval_ms(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_zero_max_versions() {
        let result = HistoricalConfigBuilder::new().max_versions_per_entity(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_zero_max_reconstruction_depth() {
        let result = HistoricalConfigBuilder::new().max_reconstruction_depth(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_max_reconstruction_depth_too_large() {
        let result = HistoricalConfigBuilder::new().max_reconstruction_depth(2000);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_zero_cache_size() {
        let result = HistoricalConfigBuilder::new().reconstruction_cache_size(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_zero_anchor_interval() {
        let result = HistoricalConfigBuilder::new().anchor_interval(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_zero_max_delta_chain() {
        let result = HistoricalConfigBuilder::new().max_delta_chain(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_build_checked_cold_storage_missing_path() {
        let result = HistoricalConfigBuilder::new()
            .enable_cold_storage(true)
            .build_checked();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_historical_config_build_checked_cold_storage_valid_path() {
        use std::path::PathBuf;
        let result = HistoricalConfigBuilder::new()
            .enable_cold_storage(true)
            .cold_storage_path(PathBuf::from("/tmp/test"))
            .build_checked();
        assert!(result.is_ok());
    }

    #[test]
    fn test_vector_config_zero_max_k() {
        let result = VectorIndexConfigBuilder::new().max_k(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_vector_config_max_k_too_large() {
        let result = VectorIndexConfigBuilder::new().max_k(200_000);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_vector_config_zero_max_layer() {
        let result = VectorIndexConfigBuilder::new().max_layer(0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }

    #[test]
    fn test_vector_config_max_layer_too_large() {
        let result = VectorIndexConfigBuilder::new().max_layer(64);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
    }
}
