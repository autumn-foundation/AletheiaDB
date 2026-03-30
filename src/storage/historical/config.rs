use crate::config::ConfigError;
use crate::core::version::AnchorConfig;
#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

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

impl Default for HistoricalConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
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
