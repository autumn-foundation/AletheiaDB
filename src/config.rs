//! Unified configuration for GallifreyDB.
//!
//! This module provides a centralized configuration system that consolidates
//! all previously hardcoded values across WAL, historical storage, and vector indexes.
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::config::{GallifreyDBConfig, WalConfigBuilder, HistoricalConfigBuilder};
//!
//! let config = GallifreyDBConfig::builder()
//!     .wal(WalConfigBuilder::new()
//!         .num_stripes(32)
//!         .stripe_capacity(2048)
//!         .build())
//!     .historical(HistoricalConfigBuilder::new()
//!         .max_versions_per_entity(5000)
//!         .max_reconstruction_depth(200)
//!         .build())
//!     .build();
//!
//! let db = GallifreyDB::with_config(config);
//! ```

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Configuration for WAL (Write-Ahead Log) system.
///
/// Controls buffer sizes, stripe configuration, and flush behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WalSystemConfig {
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
}

impl Default for WalSystemConfig {
    fn default() -> Self {
        Self {
            num_stripes: 16,
            stripe_capacity: 1024,
            write_buffer_size: 64 * 1024,   // 64KB
            segment_size: 64 * 1024 * 1024, // 64MB
            flush_interval_ms: 10,
        }
    }
}

/// Builder for WAL system configuration.
pub struct WalSystemConfigBuilder {
    config: WalSystemConfig,
}

impl WalSystemConfigBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            config: WalSystemConfig::default(),
        }
    }

    /// Set the number of stripes (will be rounded to next power of 2).
    pub fn num_stripes(mut self, num_stripes: usize) -> Self {
        self.config.num_stripes = num_stripes.next_power_of_two();
        self
    }

    /// Set the stripe capacity.
    pub fn stripe_capacity(mut self, capacity: usize) -> Self {
        self.config.stripe_capacity = capacity;
        self
    }

    /// Set the write buffer size in bytes.
    pub fn write_buffer_size(mut self, size: usize) -> Self {
        self.config.write_buffer_size = size;
        self
    }

    /// Set the segment size in bytes.
    pub fn segment_size(mut self, size: usize) -> Self {
        self.config.segment_size = size;
        self
    }

    /// Set the flush interval in milliseconds.
    pub fn flush_interval_ms(mut self, ms: u64) -> Self {
        self.config.flush_interval_ms = ms;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> WalSystemConfig {
        self.config
    }
}

impl Default for WalSystemConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for historical storage.
///
/// Controls versioning, reconstruction limits, and caching behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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
}

impl Default for HistoricalConfig {
    fn default() -> Self {
        Self {
            max_versions_per_entity: 1000,
            max_reconstruction_depth: 100,
            reconstruction_cache_size: 10000,
        }
    }
}

/// Builder for historical storage configuration.
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
    pub fn max_versions_per_entity(mut self, max: usize) -> Self {
        self.config.max_versions_per_entity = max;
        self
    }

    /// Set the maximum reconstruction depth.
    pub fn max_reconstruction_depth(mut self, depth: usize) -> Self {
        self.config.max_reconstruction_depth = depth;
        self
    }

    /// Set the reconstruction cache size.
    pub fn reconstruction_cache_size(mut self, size: usize) -> Self {
        self.config.reconstruction_cache_size = size;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> HistoricalConfig {
        self.config
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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
    pub fn max_k(mut self, k: usize) -> Self {
        self.config.max_k = k;
        self
    }

    /// Set the maximum layer depth.
    pub fn max_layer(mut self, layer: usize) -> Self {
        self.config.max_layer = layer;
        self
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

/// Unified configuration for GallifreyDB.
///
/// This consolidates all configuration settings for the database,
/// making it easy to tune for different deployment scenarios.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GallifreyDBConfig {
    /// WAL system configuration
    pub wal: WalSystemConfig,
    /// Historical storage configuration
    pub historical: HistoricalConfig,
    /// Vector index configuration
    pub vector: VectorIndexConfig,
}

/// Builder for unified database configuration.
pub struct GallifreyDBConfigBuilder {
    config: GallifreyDBConfig,
}

impl GallifreyDBConfigBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            config: GallifreyDBConfig::default(),
        }
    }

    /// Set WAL configuration.
    pub fn wal(mut self, wal_config: WalSystemConfig) -> Self {
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

    /// Build the unified configuration.
    pub fn build(self) -> GallifreyDBConfig {
        self.config
    }
}

impl Default for GallifreyDBConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GallifreyDBConfig {
    /// Create a new builder for configuration.
    pub fn builder() -> GallifreyDBConfigBuilder {
        GallifreyDBConfigBuilder::new()
    }

    /// Load configuration from a TOML file.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::config::GallifreyDBConfig;
    ///
    /// let config = GallifreyDBConfig::from_toml_file("config.toml")?;
    /// ```
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
    /// use gallifreydb::config::GallifreyDBConfig;
    ///
    /// let toml_str = r#"
    /// [wal]
    /// num_stripes = 32
    /// stripe_capacity = 2048
    /// "#;
    ///
    /// let config = GallifreyDBConfig::from_toml_str(toml_str)?;
    /// ```
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::ParseError(e.to_string()))
    }

    /// Save configuration to a TOML file.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::config::GallifreyDBConfig;
    ///
    /// let config = GallifreyDBConfig::default();
    /// config.to_toml_file("config.toml")?;
    /// ```
    pub fn to_toml_file<P: AsRef<Path>>(&self, path: P) -> Result<(), ConfigError> {
        let toml_string = self.to_toml_string()?;
        fs::write(path.as_ref(), toml_string).map_err(|e| ConfigError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Convert configuration to a TOML string.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::SerializeError(e.to_string()))
    }
}

/// Errors that can occur when loading or saving configuration.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    /// I/O error when reading or writing file.
    IoError(String),
    /// Error parsing TOML.
    ParseError(String),
    /// Error serializing to TOML.
    SerializeError(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::IoError(msg) => write!(f, "I/O error: {}", msg),
            ConfigError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            ConfigError::SerializeError(msg) => write!(f, "Serialize error: {}", msg),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wal_config_defaults() {
        let config = WalSystemConfig::default();
        assert_eq!(config.num_stripes, 16);
        assert_eq!(config.stripe_capacity, 1024);
        assert_eq!(config.write_buffer_size, 64 * 1024);
        assert_eq!(config.segment_size, 64 * 1024 * 1024);
        assert_eq!(config.flush_interval_ms, 10);
    }

    #[test]
    fn test_wal_config_builder() {
        let config = WalSystemConfigBuilder::new()
            .num_stripes(32)
            .stripe_capacity(2048)
            .write_buffer_size(128 * 1024)
            .segment_size(128 * 1024 * 1024)
            .flush_interval_ms(20)
            .build();

        assert_eq!(config.num_stripes, 32);
        assert_eq!(config.stripe_capacity, 2048);
        assert_eq!(config.write_buffer_size, 128 * 1024);
        assert_eq!(config.segment_size, 128 * 1024 * 1024);
        assert_eq!(config.flush_interval_ms, 20);
    }

    #[test]
    fn test_wal_config_builder_rounds_stripes_to_power_of_two() {
        let config = WalSystemConfigBuilder::new()
            .num_stripes(30) // Not a power of 2
            .build();

        assert_eq!(config.num_stripes, 32); // Rounded up to next power of 2
    }

    #[test]
    fn test_historical_config_defaults() {
        let config = HistoricalConfig::default();
        assert_eq!(config.max_versions_per_entity, 1000);
        assert_eq!(config.max_reconstruction_depth, 100);
        assert_eq!(config.reconstruction_cache_size, 10000);
    }

    #[test]
    fn test_historical_config_builder() {
        let config = HistoricalConfigBuilder::new()
            .max_versions_per_entity(5000)
            .max_reconstruction_depth(200)
            .reconstruction_cache_size(20000)
            .build();

        assert_eq!(config.max_versions_per_entity, 5000);
        assert_eq!(config.max_reconstruction_depth, 200);
        assert_eq!(config.reconstruction_cache_size, 20000);
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
            .max_layer(32)
            .build();

        assert_eq!(config.max_k, 20000);
        assert_eq!(config.max_layer, 32);
    }

    #[test]
    fn test_unified_config_defaults() {
        let config = GallifreyDBConfig::default();
        assert_eq!(config.wal, WalSystemConfig::default());
        assert_eq!(config.historical, HistoricalConfig::default());
        assert_eq!(config.vector, VectorIndexConfig::default());
    }

    #[test]
    fn test_unified_config_builder() {
        let config = GallifreyDBConfig::builder()
            .wal(WalSystemConfigBuilder::new().num_stripes(32).build())
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(5000)
                    .build(),
            )
            .vector(VectorIndexConfigBuilder::new().max_k(20000).build())
            .build();

        assert_eq!(config.wal.num_stripes, 32);
        assert_eq!(config.historical.max_versions_per_entity, 5000);
        assert_eq!(config.vector.max_k, 20000);
    }

    #[test]
    fn test_wal_config_fluent_api() {
        // Test that builder methods return self for chaining
        let config = WalSystemConfigBuilder::new()
            .num_stripes(8)
            .stripe_capacity(512)
            .build();

        assert_eq!(config.num_stripes, 8);
        assert_eq!(config.stripe_capacity, 512);
    }

    #[test]
    fn test_embedded_system_config() {
        // Embedded systems need smaller buffers
        let config = GallifreyDBConfig::builder()
            .wal(
                WalSystemConfigBuilder::new()
                    .num_stripes(4)
                    .stripe_capacity(256)
                    .write_buffer_size(16 * 1024)
                    .segment_size(16 * 1024 * 1024)
                    .build(),
            )
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(100)
                    .reconstruction_cache_size(1000)
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
        let config = GallifreyDBConfig::builder()
            .wal(
                WalSystemConfigBuilder::new()
                    .num_stripes(64)
                    .stripe_capacity(4096)
                    .write_buffer_size(256 * 1024)
                    .segment_size(256 * 1024 * 1024)
                    .build(),
            )
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(10000)
                    .reconstruction_cache_size(100000)
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
        let config = GallifreyDBConfig::builder()
            .wal(
                WalSystemConfigBuilder::new()
                    .flush_interval_ms(100) // Longer interval for batching
                    .build(),
            )
            .build();

        assert_eq!(config.wal.flush_interval_ms, 100);
    }

    // TOML configuration tests

    #[test]
    fn test_toml_serialization() {
        let config = GallifreyDBConfig::default();
        let toml_string = config.to_toml_string().unwrap();

        // Should contain all sections
        assert!(toml_string.contains("[wal]"));
        assert!(toml_string.contains("[historical]"));
        assert!(toml_string.contains("[vector]"));

        // Should contain some key values
        assert!(toml_string.contains("num_stripes"));
        assert!(toml_string.contains("max_versions_per_entity"));
        assert!(toml_string.contains("max_k"));
    }

    #[test]
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

        let config = GallifreyDBConfig::from_toml_str(toml_str).unwrap();

        assert_eq!(config.wal.num_stripes, 32);
        assert_eq!(config.wal.stripe_capacity, 2048);
        // Other values should have defaults
        assert_eq!(config.wal.write_buffer_size, 64 * 1024);
    }

    #[test]
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

[vector]
max_k = 20000
max_layer = 32
        "#;

        let config = GallifreyDBConfig::from_toml_str(toml_str).unwrap();

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

        // Vector config
        assert_eq!(config.vector.max_k, 20000);
        assert_eq!(config.vector.max_layer, 32);
    }

    #[test]
    fn test_toml_round_trip() {
        // Create config with custom values
        let original = GallifreyDBConfig::builder()
            .wal(
                WalSystemConfigBuilder::new()
                    .num_stripes(32)
                    .stripe_capacity(2048)
                    .build(),
            )
            .historical(
                HistoricalConfigBuilder::new()
                    .max_versions_per_entity(5000)
                    .build(),
            )
            .vector(VectorIndexConfigBuilder::new().max_k(20000).build())
            .build();

        // Serialize to TOML
        let toml_string = original.to_toml_string().unwrap();

        // Deserialize back
        let deserialized = GallifreyDBConfig::from_toml_str(&toml_string).unwrap();

        // Should be equal
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_toml_file_save_and_load() {
        use tempfile::NamedTempFile;

        let config = GallifreyDBConfig::builder()
            .wal(WalSystemConfigBuilder::new().num_stripes(64).build())
            .build();

        // Create a temporary file
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Save to file
        config.to_toml_file(path).unwrap();

        // Load from file
        let loaded = GallifreyDBConfig::from_toml_file(path).unwrap();

        // Should be equal
        assert_eq!(config, loaded);
    }

    #[test]
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

        let config = GallifreyDBConfig::from_toml_str(toml_str).unwrap();

        assert_eq!(config.wal.num_stripes, 4);
        assert_eq!(config.wal.stripe_capacity, 256);
        assert_eq!(config.historical.max_versions_per_entity, 100);
        assert_eq!(config.vector.max_k, 1000);
    }

    #[test]
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

        let config = GallifreyDBConfig::from_toml_str(toml_str).unwrap();

        assert_eq!(config.wal.num_stripes, 64);
        assert_eq!(config.wal.stripe_capacity, 4096);
        assert_eq!(config.historical.max_versions_per_entity, 10000);
        assert_eq!(config.vector.max_k, 50000);
    }

    #[test]
    fn test_toml_parse_error() {
        let invalid_toml = "this is not valid toml {]";
        let result = GallifreyDBConfig::from_toml_str(invalid_toml);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::ParseError(_)));
    }
}
