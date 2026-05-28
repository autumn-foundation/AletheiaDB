#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

use crate::config::ConfigError;

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
