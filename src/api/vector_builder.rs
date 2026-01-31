use crate::db::GallifreyDB;
use crate::index::vector::hnsw::HnswConfig;
use crate::index::vector::temporal::TemporalVectorConfig;
use crate::utils::error::Result;

/// Builder for configuring and enabling a vector index on a property.
///
/// This builder provides a fluent API for setting up vector indexes with
/// HNSW configuration and optional temporal indexing. The builder pattern
/// ensures that required configuration (HNSW) is provided before enabling.
///
/// # Example
///
/// ```rust
/// use gallifreydb::GallifreyDB;
/// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
/// use gallifreydb::index::vector::temporal::TemporalVectorConfig;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let db = GallifreyDB::new()?;
///
/// // Create a basic vector index
/// db.vector_index("embedding")
///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
///     .enable()?;
///
/// // Create a vector index with temporal support
/// // Note: Using a different property since "embedding" is already used
/// db.vector_index("content_embedding")
///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
///     .temporal(TemporalVectorConfig::default())
///     .enable()?;
/// # Ok(())
/// # }
/// ```
pub struct VectorIndexBuilder<'a> {
    db: &'a GallifreyDB,
    property_name: String,
    hnsw_config: Option<HnswConfig>,
    temporal_config: Option<TemporalVectorConfig>,
}

impl<'a> VectorIndexBuilder<'a> {
    /// Create a new builder for the specified property.
    pub fn new(db: &'a GallifreyDB, property_name: String) -> Self {
        VectorIndexBuilder {
            db,
            property_name,
            hnsw_config: None,
            temporal_config: None,
        }
    }

    /// Set the HNSW index configuration.
    ///
    /// This is required before calling `enable()`. The configuration specifies
    /// the vector dimensions, distance metric, and HNSW parameters.
    ///
    /// # Arguments
    ///
    /// * `config` - The HNSW index configuration
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::GallifreyDB;
    /// # use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = GallifreyDB::new()?;
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(10000))
    ///     .enable()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn hnsw(mut self, config: HnswConfig) -> Self {
        self.hnsw_config = Some(config);
        self
    }

    /// Enable temporal vector indexing.
    ///
    /// When temporal indexing is enabled, vector snapshots are created
    /// periodically to enable point-in-time vector queries and semantic
    /// drift tracking. This automatically enables the current index as well.
    ///
    /// # Arguments
    ///
    /// * `config` - The temporal vector index configuration
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::GallifreyDB;
    /// # use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    /// use gallifreydb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = GallifreyDB::new()?;
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .temporal(TemporalVectorConfig {
    ///         snapshot_strategy: SnapshotStrategy::TransactionInterval(100),
    ///         ..Default::default()
    ///     })
    ///     .enable()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn temporal(mut self, config: TemporalVectorConfig) -> Self {
        self.temporal_config = Some(config);
        self
    }

    /// Enable the vector index with the configured settings.
    ///
    /// This method validates the configuration and enables the index.
    /// If temporal configuration was provided, both the current and
    /// temporal indexes are enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - HNSW configuration was not provided (call `.hnsw()` first)
    /// - A vector index already exists for this property
    /// - The temporal index configuration is invalid
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::GallifreyDB;
    /// # use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = GallifreyDB::new()?;
    /// // This will fail - no HNSW config
    /// let result = db.vector_index("embedding").enable();
    /// assert!(result.is_err());
    ///
    /// // This will succeed
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .enable()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn enable(self) -> Result<()> {
        let hnsw_config = self.hnsw_config.ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "HNSW configuration is required. Call .hnsw() before .enable()".to_string(),
            ))
        })?;

        // If temporal config is provided, use enable_temporal_vector_index which
        // automatically enables the current index as well (fixing #386)
        if let Some(temporal_config) = self.temporal_config {
            // Merge HNSW config into temporal config
            let temporal_config_with_hnsw = TemporalVectorConfig {
                hnsw_config: Some(hnsw_config),
                ..temporal_config
            };
            self.db
                .enable_temporal_vector_index(&self.property_name, temporal_config_with_hnsw)
        } else {
            // Just enable the current index
            self.db
                .enable_vector_index(&self.property_name, hnsw_config)
        }
    }
}
