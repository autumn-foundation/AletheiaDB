#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "config-toml")]
use std::fs;
#[cfg(feature = "config-toml")]
use std::path::Path;

use crate::config::{ConfigError, HistoricalConfig, VectorIndexConfig, WalConfig};
use crate::storage::index_persistence::PersistenceConfig;

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
    /// Encryption at rest configuration
    pub encryption: crate::encryption::config::EncryptionConfig,
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

    /// Set encryption at rest configuration.
    pub fn encryption(
        mut self,
        encryption_config: crate::encryption::config::EncryptionConfig,
    ) -> Self {
        self.config.encryption = encryption_config;
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
