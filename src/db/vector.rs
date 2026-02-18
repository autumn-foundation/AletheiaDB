use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::db::vector_builder::VectorIndexBuilder;
use crate::index::vector::hnsw::HnswConfig;
use crate::index::vector::temporal::{TemporalVectorConfig, VectorIndexObserver};
use std::sync::Arc;

impl AletheiaDB {
    /// Enable vector indexing for a specific property.
    ///
    /// Once enabled, nodes with the specified property will be automatically
    /// indexed for similarity search. The property must contain vector values.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - HNSW index configuration (dimensions, metric, etc.)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// db.enable_vector_index("embedding", config)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if vector indexing is already enabled.
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("enable_vector_index").entered();
        self.current.enable_vector_index(property_name, config)
    }

    /// Check if vector indexing is enabled.
    pub fn is_vector_index_enabled(&self) -> bool {
        self.current.is_vector_index_enabled()
    }

    /// Check if vector indexing is enabled for a specific property.
    pub fn is_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.current.is_vector_index_enabled_for(property_name)
    }

    /// Enable temporal vector indexing for a specific property.
    ///
    /// Once enabled, vector changes will be tracked over time using snapshot-based
    /// indexing, enabling point-in-time vector queries and semantic drift tracking.
    /// This also integrates with the historical storage's observer pattern to create
    /// vector snapshots aligned with graph anchors.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - Temporal vector index configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy};
    /// use aletheiadb::index::vector::HnswConfig;
    ///
    /// let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
    /// db.enable_temporal_vector_index("embedding", temporal_config)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector indexing is already enabled
    /// - The historical storage lock is poisoned
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: TemporalVectorConfig,
    ) -> Result<()> {
        // Resolve hnsw_config: use provided config or get from existing vector index
        let resolved_hnsw_config =
            if let Some(hnsw_config) = config.hnsw_config.clone() {
                // Config was provided explicitly
                hnsw_config
            } else if self.current.is_vector_index_enabled_for(property_name) {
                // No config provided, but vector index exists - use its config
                self.current.get_hnsw_config_for(property_name).ok_or_else(|| {
                crate::core::error::Error::Vector(crate::core::error::VectorError::IndexError(
                    format!(
                        "Vector index exists for '{}' but could not retrieve its configuration",
                        property_name
                    ),
                ))
            })?
            } else {
                // No config provided and no vector index exists - error
                return Err(crate::core::error::Error::Vector(
                    crate::core::error::VectorError::IndexError(
                        "HNSW configuration is required when no vector index exists. \
                     Use TemporalVectorConfig::default_with_hnsw() to provide one, \
                     or enable the vector index first with enable_vector_index()."
                            .to_string(),
                    ),
                ));
            };

        // Enable vector index if it doesn't exist yet
        if !self.current.is_vector_index_enabled_for(property_name) {
            self.enable_vector_index(property_name, resolved_hnsw_config.clone())?;
        }

        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("enable_temporal_vector_index").entered();

        // Create a resolved config with the hnsw_config set
        let resolved_config = TemporalVectorConfig {
            hnsw_config: Some(resolved_hnsw_config),
            ..config
        };

        // Enable temporal vector index in current storage
        self.current
            .enable_temporal_vector_index(property_name, resolved_config)?;

        // Get the temporal vector index from current storage
        let temporal_index = self.current.get_temporal_vector_index().ok_or_else(|| {
            crate::core::error::Error::Vector(crate::core::error::VectorError::IndexError(
                "Temporal vector index not found after enabling".to_string(),
            ))
        })?;

        // Register pre-anchor hooks with historical storage (for strong consistency)
        // Both node and edge hooks perform the same action, so we create one and clone it
        let hook: crate::storage::historical::PreAnchorHook = {
            let index = Arc::clone(&temporal_index);
            Arc::new(move |_entity_type, _entity_id, timestamp, _properties| {
                index.create_snapshot_for_anchor(timestamp)
            })
        };

        let node_hook = Arc::clone(&hook);
        let edge_hook = hook;

        let mut historical = self.historical.write();

        historical.register_pre_node_anchor_hook(node_hook);
        historical.register_pre_edge_anchor_hook(edge_hook);

        // Create observer and register with historical storage (for extensibility)
        let observer = VectorIndexObserver::new(temporal_index);
        historical.add_observer(std::sync::Arc::new(observer));

        Ok(())
    }

    /// Check if temporal vector indexing is enabled.
    pub fn is_temporal_vector_index_enabled(&self) -> bool {
        self.current.is_temporal_vector_index_enabled()
    }

    /// List all property names that have temporal vector indexes enabled.
    ///
    /// Returns a vector of property names that have temporal vector indexing configured.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = AletheiaDB::new();
    /// // Enable temporal indexes for two properties
    /// db.vector_index("embedding1").hnsw(config1).temporal(temporal_config).enable()?;
    /// db.vector_index("embedding2").hnsw(config2).temporal(temporal_config).enable()?;
    ///
    /// let indexes = db.list_temporal_vector_indexes();
    /// assert!(indexes.contains(&"embedding1".to_string()));
    /// assert!(indexes.contains(&"embedding2".to_string()));
    /// ```
    pub fn list_temporal_vector_indexes(&self) -> Vec<String> {
        self.current.list_temporal_vector_indexes()
    }

    /// Create a builder for configuring a vector index on a property.
    ///
    /// This provides a fluent API for enabling vector indexes with optional
    /// temporal configuration. The builder pattern ensures proper configuration
    /// before enabling the index.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name that will contain vector embeddings
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// // Basic vector index
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .enable()?;
    ///
    /// // With temporal indexing for time-travel queries
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .temporal(TemporalVectorConfig::default())
    ///     .enable()?;
    /// ```
    pub fn vector_index(&self, property_name: &str) -> VectorIndexBuilder<'_> {
        VectorIndexBuilder::new(self, property_name.to_string())
    }

    /// Check if a vector index is enabled for a specific property.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name to check
    ///
    /// # Example
    ///
    /// ```ignore
    /// db.vector_index("embedding")
    ///     .hnsw(config)
    ///     .enable()?;
    ///
    /// assert!(db.has_vector_index("embedding"));
    /// assert!(!db.has_vector_index("other_property"));
    /// ```
    pub fn has_vector_index(&self, property_name: &str) -> bool {
        self.current.has_vector_index(property_name)
    }

    /// List all enabled vector indexes.
    ///
    /// Returns information about each configured vector index including
    /// the property name, dimensions, and distance metric.
    ///
    /// # Example
    ///
    /// ```ignore
    /// db.vector_index("title_embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .enable()?;
    ///
    /// db.vector_index("body_embedding")
    ///     .hnsw(HnswConfig::new(768, DistanceMetric::Euclidean))
    ///     .enable()?;
    ///
    /// let indexes = db.list_vector_indexes();
    /// assert_eq!(indexes.len(), 2);
    /// ```
    pub fn list_vector_indexes(&self) -> Vec<crate::storage::VectorIndexInfo> {
        self.current.list_vector_indexes()
    }

    /// Find k most similar nodes in a specific property's vector index.
    ///
    /// Use this method when you have multiple vector indexes and need to
    /// search a specific one. The query node's embedding from the specified
    /// property is used for the search.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The indexed property to search
    /// * `query_node_id` - The node to find similar nodes for
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search title embeddings for similar nodes
    /// let similar = db.find_similar_in("title_embedding", node_id, 10)?;
    ///
    /// // Search body embeddings (different property, potentially different results)
    /// let similar_body = db.find_similar_in("body_embedding", node_id, 10)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Query node is not found
    /// - Query node does not have the specified vector property
    pub fn find_similar_in(
        &self,
        property_name: &str,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_in").entered();
        self.current
            .find_similar_in(property_name, query_node_id, k)
    }

    /// Search a specific property's vector index with a raw embedding.
    ///
    /// Use this method when searching with embeddings that don't correspond to
    /// any existing node in the graph (e.g., query embeddings from external sources).
    ///
    /// # Arguments
    ///
    /// * `property_name` - The indexed property to search
    /// * `embedding` - The query embedding vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search with external embedding
    /// let query = embed_text("search query");
    /// let results = db.search_vectors_in("title_embedding", &query, 10)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Embedding dimensions don't match the index configuration
    pub fn search_vectors_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("search_vectors_in").entered();
        self.current.search_vectors_in(property_name, embedding, k)
    }

    /// Find k most similar nodes to a query node based on vector similarity.
    ///
    /// Returns a list of (NodeId, score) pairs sorted by similarity (highest first).
    /// The query node itself is excluded from results.
    ///
    /// # Arguments
    ///
    /// * `query_node_id` - The node to find similar nodes for
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find the 5 most similar documents to a given document
    /// let results = db.find_similar(doc_id, 5)?;
    /// for (node_id, score) in results {
    ///     println!("Similar node {} with score {}", node_id, score);
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Query node is not found
    /// - Query node does not have the indexed vector property
    pub fn find_similar(&self, query_node_id: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar").entered();
        self.current.find_similar(query_node_id, k)
    }

    /// Find k most similar nodes with a specific label.
    ///
    /// This is useful for finding similar nodes within a category, e.g.,
    /// "find similar documents" or "find similar users".
    ///
    /// # Arguments
    ///
    /// * `query_node_id` - The node to find similar nodes for
    /// * `label` - Only return nodes with this label
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find similar Person nodes only
    /// let similar_people = db.find_similar_with_label(person_id, "Person", 10)?;
    /// ```
    pub fn find_similar_with_label(
        &self,
        query_node_id: NodeId,
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_with_label").entered();
        self.current
            .find_similar_with_label(query_node_id, label, k)
    }

    /// Find k most similar nodes to a raw embedding vector.
    ///
    /// This is useful when searching with embeddings that don't correspond to any
    /// existing node in the graph, such as query embeddings from external sources
    /// or user input.
    ///
    /// # Arguments
    ///
    /// * `embedding` - The query embedding vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Embedding dimensions don't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search with an embedding from external source (e.g., user query)
    /// let query_embedding = get_embedding_from_llm("rust programming");
    /// let similar = db.find_similar_by_embedding(&query_embedding, 10)?;
    /// for (node_id, similarity) in similar {
    ///     println!("Node {:?} has similarity {}", node_id, similarity);
    /// }
    /// ```
    pub fn find_similar_by_embedding(
        &self,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_by_embedding").entered();
        self.current.find_similar_by_embedding(embedding, k)
    }

    /// Find k most similar nodes with a specific label to a raw embedding vector.
    ///
    /// Like `find_similar_by_embedding()`, but filters results to only include
    /// nodes with the specified label.
    ///
    /// # Arguments
    ///
    /// * `embedding` - The query embedding vector
    /// * `label` - Only return nodes with this label
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    /// All results have the specified label.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Embedding dimensions don't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find similar documents only
    /// let query_embedding = get_embedding_from_llm("rust programming");
    /// let similar_docs = db.find_similar_by_embedding_with_label(
    ///     &query_embedding,
    ///     "Document",
    ///     5
    /// )?;
    /// ```
    pub fn find_similar_by_embedding_with_label(
        &self,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_by_embedding_with_label").entered();
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_by_embedding").entered();
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar").entered();
        self.current
            .find_similar_by_embedding_with_label(embedding, label, k)
    }

    /// Find k most similar nodes using a custom predicate for filtering.
    ///
    /// This method allows executing a vector search where candidates are filtered
    /// by an arbitrary predicate closure. This is useful for complex filtering logic
    /// that cannot be expressed as a simple label check.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `query_vector` - The query embedding vector
    /// * `k` - Maximum number of results to return
    /// * `predicate` - A closure that takes a `NodeId` and returns `true` if the node should be included
    pub fn find_similar_with_predicate<F>(
        &self,
        property_name: &str,
        query_vector: &[f32],
        k: usize,
        predicate: F,
    ) -> Result<Vec<(NodeId, f32)>>
    where
        F: Fn(&NodeId) -> bool + Send + Sync,
    {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_with_predicate").entered();
        self.current
            .find_similar_with_predicate(property_name, query_vector, k, predicate)
    }

    /// Find k most similar nodes at a specific point in time.
    ///
    /// This method performs a temporal vector search, finding nodes with embeddings
    /// most similar to the query embedding as they existed at the specified timestamp.
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query embedding vector to search for
    /// * `k` - Maximum number of results to return
    /// * `timestamp` - Point in time to query (in microseconds since epoch)
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, similarity_score) tuples, sorted by similarity in descending order.
    ///
    /// # Errors
    ///
    /// - `Error::Vector(VectorError::IndexError)` if temporal vector index is not enabled
    /// - `Error::Vector(VectorError::*)` if the query embedding is invalid
    /// - `Error::Temporal(*)` if no snapshot exists at the given timestamp
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find documents similar to a query at a specific point in time
    /// let timestamp_2023 = 1672531200000000; // 2023-01-01 in microseconds
    /// let results = db.find_similar_as_of(&query_embedding, 10, timestamp_2023)?;
    /// ```
    pub fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_as_of").entered();
        self.current.find_similar_as_of(embedding, k, timestamp)
    }

    /// Find similar vectors at a specific point in time for a specific property.
    ///
    /// This is the property-specific version of [`find_similar_as_of`](Self::find_similar_as_of).
    /// It validates that the requested property matches the property for which
    /// the temporal vector index was enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `embedding` - Query vector to find similar vectors to
    /// * `k` - Number of results to return
    /// * `timestamp` - The point in time to query (in microseconds since epoch)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Query embedding dimensions don't match
    /// - No snapshot exists at the given timestamp
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find documents similar to a query at a specific point in time
    /// let timestamp_2023 = 1672531200000000; // 2023-01-01 in microseconds
    /// let results = db.find_similar_as_of_in(
    ///     "content_embedding",
    ///     &query_embedding,
    ///     10,
    ///     timestamp_2023
    /// )?;
    /// ```
    pub fn find_similar_as_of_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span =
            tracing::info_span!("find_similar_as_of_in", property = property_name).entered();
        self.current
            .find_similar_as_of_in(property_name, embedding, k, timestamp)
    }

    /// Track semantic drift for a node over time in a specific property's temporal index.
    ///
    /// This method tracks how a node's embedding has changed relative to a reference
    /// embedding over time. It validates that the requested property matches the
    /// property for which the temporal vector index was enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `node_id` - The node to track drift for
    /// * `reference_embedding` - Reference vector to measure drift against
    /// * `time_range` - The time range to search for drift
    ///
    /// # Returns
    ///
    /// A vector of (timestamp, drift_score) pairs showing how the node's embedding
    /// drifted from the reference at each snapshot time.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Reference embedding dimensions don't match
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::core::temporal::TimeRange;
    ///
    /// // Track how a document's embedding changed from its original version
    /// let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    /// let drift = db.track_drift_in(
    ///     "content_embedding",
    ///     node_id,
    ///     &original_embedding,
    ///     time_range
    /// )?;
    ///
    /// for (timestamp, distance) in drift {
    ///     println!("At {}: drift = {:.3}", timestamp, distance);
    /// }
    /// ```
    pub fn track_drift_in(
        &self,
        property_name: &str,
        node_id: NodeId,
        reference_embedding: &[f32],
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(Timestamp, f32)>> {
        #[cfg(feature = "observability")]
        let _span =
            tracing::info_span!("track_drift_in", property = property_name, node = ?node_id)
                .entered();
        self.current
            .track_drift_in(property_name, node_id, reference_embedding, time_range)
    }

    /// Get the semantic evolution of a node's embedding over time in a specific property.
    ///
    /// Returns the actual embedding vectors at each snapshot timestamp, allowing
    /// you to see how the node's semantic representation changed over time.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `node_id` - The node to get evolution for
    /// * `time_range` - The time range to query
    ///
    /// # Returns
    ///
    /// A vector of (timestamp, embedding) pairs showing the node's embedding
    /// at each snapshot time within the range.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::core::temporal::TimeRange;
    ///
    /// let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    /// let evolution = db.semantic_evolution_in("content_embedding", node_id, time_range)?;
    ///
    /// for (timestamp, embedding) in evolution {
    ///     println!("At {}: {} dimensions", timestamp, embedding.len());
    /// }
    /// ```
    pub fn semantic_evolution_in(
        &self,
        property_name: &str,
        node_id: NodeId,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(Timestamp, std::sync::Arc<[f32]>)>> {
        #[cfg(feature = "observability")]
        let _span =
            tracing::info_span!("semantic_evolution_in", property = property_name, node = ?node_id)
                .entered();
        self.current
            .semantic_evolution_in(property_name, node_id, time_range)
    }

    /// Find all nodes with semantic drift above a threshold in a specific property.
    ///
    /// Scans all nodes in the temporal index and identifies those whose embeddings
    /// have changed by more than the specified threshold over the time range.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `threshold` - Minimum drift distance to include in results
    /// * `time_range` - The time range to analyze
    /// * `metric` - The distance metric to use for drift calculation
    ///
    /// # Returns
    ///
    /// A vector of (node_id, drift_score) pairs for nodes exceeding the threshold,
    /// sorted by drift score in descending order.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Threshold is NaN or infinite
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::core::temporal::TimeRange;
    /// use aletheiadb::index::vector::temporal::DriftMetric;
    ///
    /// let time_range = TimeRange::new(start_ts.into(), end_ts.into()).unwrap();
    /// let drifted = db.find_drift_in(
    ///     "content_embedding",
    ///     0.3,  // threshold
    ///     time_range,
    ///     DriftMetric::Cosine
    /// )?;
    ///
    /// for (node_id, drift) in drifted {
    ///     println!("Node {} drifted by {:.3}", node_id, drift);
    /// }
    /// ```
    pub fn find_drift_in(
        &self,
        property_name: &str,
        threshold: f32,
        time_range: crate::core::temporal::TimeRange,
        metric: crate::index::vector::temporal::DriftMetric,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!(
            "find_drift_in",
            property = property_name,
            threshold = threshold
        )
        .entered();
        self.current
            .find_drift_in(property_name, threshold, time_range, metric)
    }
}
