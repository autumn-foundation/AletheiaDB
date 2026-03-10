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

#[cfg(feature = "config-toml")]
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
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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

/// Unified configuration for AletheiaDB.
///
/// This consolidates all configuration settings for the database,
/// making it easy to tune for different deployment scenarios.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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
            .build();

        assert_eq!(config.max_versions_per_entity, 5000);
        assert_eq!(config.max_reconstruction_depth, 200);
        assert_eq!(config.reconstruction_cache_size, 20000);
        assert_eq!(config.anchor_interval, 5);
        assert_eq!(config.max_delta_chain, 10);
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
    #[should_panic(expected = "cold_storage_path must be set when enable_cold_storage is true")]
    fn test_historical_config_build_panics_without_path() {
        HistoricalConfigBuilder::new()
            .enable_cold_storage(true)
            .build();
    }

    #[test]
    fn test_historical_config_enable_cold_storage() {
        let builder = HistoricalConfigBuilder::new().enable_cold_storage(true);
        assert!(builder.config.enable_cold_storage);

        let builder = builder.enable_cold_storage(false);
        assert!(!builder.config.enable_cold_storage);
    }

    #[test]
    fn test_historical_config_cold_storage_path() {
        let path = std::path::PathBuf::from("/test/path");
        let builder = HistoricalConfigBuilder::new().cold_storage_path(path.clone());
        assert_eq!(builder.config.cold_storage_path, Some(path));
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
