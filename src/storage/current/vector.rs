use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::index::current::CurrentIndexes;
use crate::index::vector::hnsw::{HnswConfig, HnswIndex};
use crate::index::vector::temporal::{TemporalVectorConfig, TemporalVectorIndex};
use crate::index::vector::{DistanceMetric, TemporalSearchResults, VectorIndex};
use crate::utils::error::{Result, StorageError};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use parking_lot::RwLock;
use std::sync::Arc;

use super::stats::FilterStats;

/// Maximum number of vector-indexed properties allowed per database.
///
/// This limit prevents resource exhaustion from enabling too many vector indexes,
/// as each index consumes significant memory (1.5-2x the raw vector data size
/// due to HNSW graph overhead).
///
/// Override via configuration if your use case requires more properties.
pub const DEFAULT_MAX_VECTOR_PROPERTIES: usize = 10;

/// Entry for a single vector index on a specific property.
///
/// Each property can have its own HNSW index with independent configuration
/// (dimensions, distance metric, etc.). This enables multi-property vector
/// indexing for use cases like separate title/body embeddings.
pub(crate) struct VectorIndexEntry {
    /// The HNSW index for this property
    pub(crate) index: Arc<HnswIndex>,
    /// Configuration used to create this index
    pub(crate) config: HnswConfig,
}

/// Information about a configured vector index.
///
/// Returned by [`crate::storage::CurrentStorage::list_vector_indexes`] to provide
/// metadata about all enabled vector indexes.
#[derive(Debug, Clone)]
pub struct VectorIndexInfo {
    /// The property name this index is configured for
    pub property_name: String,
    /// Number of dimensions in the vectors
    pub dimensions: usize,
    /// Distance metric used for similarity calculations
    pub distance_metric: DistanceMetric,
}

/// Entry for a temporal vector index (multi-property support).
pub(crate) struct TemporalVectorIndexEntry {
    /// The temporal vector index for this property
    pub(crate) index: Arc<TemporalVectorIndex>,
    /// Configuration used to create this index
    #[allow(dead_code)]
    pub(crate) config: TemporalVectorConfig,
}

/// Legacy internal state for temporal vector indexing.
/// Kept for backward compatibility with existing code paths.
pub(crate) struct TemporalVectorIndexState {
    pub(crate) index: Option<Arc<TemporalVectorIndex>>,
    pub(crate) property_name: Option<String>,
    pub(crate) config: Option<TemporalVectorConfig>,
}

impl TemporalVectorIndexState {
    pub(crate) fn new() -> Self {
        TemporalVectorIndexState {
            index: None,
            property_name: None,
            config: None,
        }
    }

    #[allow(dead_code)] // Kept for backward compatibility with legacy single-property API
    pub(crate) fn is_enabled(&self) -> bool {
        self.index.is_some()
    }
}

/// Manager for vector indexes in the current storage.
///
/// Handles lifecycle, updates, and searches for both HNSW and temporal vector indexes.
/// This separates vector indexing concerns from the main graph storage logic.
pub struct VectorIndexManager {
    /// Multi-property vector indexes (Issue #389)
    /// Maps property name -> VectorIndexEntry
    vector_indexes: DashMap<String, VectorIndexEntry>,
    /// Multi-property temporal vector indexes (Issue #389 fix)
    /// Maps property name -> TemporalVectorIndexEntry
    temporal_vector_indexes: DashMap<String, TemporalVectorIndexEntry>,
    /// Legacy temporal vector index state (for backward compatibility)
    temporal_vector_index_state: Arc<RwLock<TemporalVectorIndexState>>,
    /// Adaptive over-fetch statistics per label (Issue #334)
    /// Maps label -> FilterStats for tracking label-specific filter pass rates
    filter_stats: DashMap<String, Arc<FilterStats>>,
}

impl VectorIndexManager {
    /// Create a new vector index manager.
    pub fn new() -> Self {
        VectorIndexManager {
            vector_indexes: DashMap::new(),
            temporal_vector_indexes: DashMap::new(),
            temporal_vector_index_state: Arc::new(RwLock::new(TemporalVectorIndexState::new())),
            filter_stats: DashMap::new(),
        }
    }

    /// Enable vector indexing for a specific property.
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()> {
        // Check property limit before attempting to add
        if self.vector_indexes.len() >= DEFAULT_MAX_VECTOR_PROPERTIES {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::IndexError(format!(
                    "Maximum number of vector-indexed properties ({}) reached. \
                     Cannot enable index for property '{}'",
                    DEFAULT_MAX_VECTOR_PROPERTIES, property_name
                )),
            ));
        }

        // Use atomic entry() API to avoid TOCTOU race condition
        match self.vector_indexes.entry(property_name.to_string()) {
            Entry::Occupied(_) => {
                return Err(crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::IndexError(format!(
                        "Vector index already enabled for property '{}'",
                        property_name
                    )),
                ));
            }
            Entry::Vacant(vacant) => {
                // Create the HNSW index
                let index = HnswIndex::new(config.clone())?;
                let entry = VectorIndexEntry {
                    index: Arc::new(index),
                    config: config.clone(),
                };
                vacant.insert(entry);
            }
        }

        Ok(())
    }

    /// Check if any vector indexing is enabled.
    pub fn is_vector_index_enabled(&self) -> bool {
        !self.vector_indexes.is_empty()
    }

    /// Check if vector indexing is enabled for a specific property.
    pub fn is_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.vector_indexes.contains_key(property_name)
    }

    /// Get the first/default property name that is currently indexed.
    pub fn get_indexed_property_name(&self) -> Option<String> {
        self.get_default_vector_property_name()
    }

    /// List all configured vector indexes.
    pub fn list_vector_indexes(&self) -> Vec<VectorIndexInfo> {
        self.vector_indexes
            .iter()
            .map(|entry| VectorIndexInfo {
                property_name: entry.key().clone(),
                dimensions: entry.value().config.dimensions,
                distance_metric: entry.value().config.metric,
            })
            .collect()
    }

    /// Check if a vector index is enabled for a specific property.
    pub fn has_vector_index(&self, property_name: &str) -> bool {
        self.vector_indexes.contains_key(property_name)
    }

    /// Get the HNSW configuration for a specific property's vector index.
    pub fn get_hnsw_config_for(&self, property_name: &str) -> Option<HnswConfig> {
        self.vector_indexes
            .get(property_name)
            .map(|entry| entry.config.clone())
    }

    /// Get vector index configuration for checkpoint persistence.
    pub fn get_vector_index_config(
        &self,
    ) -> crate::storage::persistence::VectorIndexCheckpointData {
        use crate::storage::persistence::VectorIndexCheckpointData;

        self.get_default_vector_property_name()
            .and_then(|property_name| {
                let entry = self.vector_indexes.get(&property_name)?;
                Some(VectorIndexCheckpointData::enabled(
                    property_name,
                    entry.value().config.clone(),
                ))
            })
            .unwrap_or_else(VectorIndexCheckpointData::disabled)
    }

    /// Register a vector index (used during index loading from disk).
    pub(crate) fn register_vector_index(
        &self,
        property_name: &str,
        index: crate::index::vector::HnswIndex,
        config: crate::index::vector::HnswConfig,
    ) {
        let index_arc = Arc::new(index);

        // Insert into multi-property map
        self.vector_indexes.insert(
            property_name.to_string(),
            VectorIndexEntry {
                index: Arc::clone(&index_arc),
                config: config.clone(),
            },
        );
    }

    /// Get a reference to the HNSW index and its config for a specific property.
    #[allow(clippy::type_complexity)]
    pub(crate) fn get_vector_index_for_persistence(
        &self,
        property_name: &str,
    ) -> Option<(
        Arc<crate::index::vector::HnswIndex>,
        crate::index::vector::HnswConfig,
        usize,
        Vec<(u64, u64)>,
    )> {
        use crate::index::vector::VectorIndex;
        self.vector_indexes.get(property_name).map(|entry| {
            let index = entry.value().index.clone();
            let config = entry.value().config.clone();
            let count = index.len();

            // Extract ID mappings from the index
            let mappings = index.get_id_mappings();

            (index, config, count, mappings)
        })
    }

    /// Try to add a node's vectors to all enabled indexes.
    pub fn try_index_vector(&self, node_id: NodeId, properties: &PropertyMap) -> Result<bool> {
        let mut indexed_any = false;

        // Index in all multi-property indexes
        for entry in self.vector_indexes.iter() {
            let prop_name = entry.key();
            if let Some(vector) = properties.get(prop_name).and_then(|v| v.as_vector()) {
                let index = Arc::clone(&entry.value().index);
                index.add(node_id, vector)?;
                indexed_any = true;
            }
        }

        Ok(indexed_any)
    }

    /// Try to remove a node from all vector indexes.
    pub fn try_remove_from_index(&self, node_id: NodeId) -> Result<bool> {
        let mut removed_any = false;

        // Remove from all multi-property indexes
        for entry in self.vector_indexes.iter() {
            let index = Arc::clone(&entry.value().index);
            // Remove may fail if node wasn't in this index, which is OK
            if index.remove(node_id).is_ok() {
                removed_any = true;
            }
        }

        Ok(removed_any)
    }

    /// Update the vector indexes when node properties change.
    pub fn update_vector_index(
        &self,
        node_id: NodeId,
        new_props: &PropertyMap,
        old_props: &PropertyMap,
    ) -> Result<()> {
        // Update all multi-property indexes
        for entry in self.vector_indexes.iter() {
            let prop_name = entry.key();
            let index = Arc::clone(&entry.value().index);

            let old_vec = old_props.get(prop_name).and_then(|v| v.as_vector());
            let new_vec = new_props.get(prop_name).and_then(|v| v.as_vector());

            match (old_vec, new_vec) {
                (None, None) => {} // No change for this property
                (None, Some(v)) => {
                    index.add(node_id, v)?;
                }
                (Some(_), None) => {
                    let _ = index.remove(node_id); // May not exist, ignore error
                }
                (Some(o), Some(n)) => {
                    if o != n {
                        // Note: HnswIndex::add() is an upsert operation (remove + add internally)
                        index.add(node_id, n)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the default property name for vector indexing.
    fn get_default_vector_property_name(&self) -> Option<String> {
        if self.vector_indexes.is_empty() {
            return None;
        }

        // Optimization: if len == 1, just take it (no sorting needed)
        if self.vector_indexes.len() == 1 {
            return self.vector_indexes.iter().next().map(|r| r.key().clone());
        }

        // Find min key alphabetically
        // Note: DashMap iteration order is not guaranteed, so we must scan all keys
        self.vector_indexes.iter().map(|r| r.key().clone()).min()
    }

    /// Helper method to prepare for vector search.
    /// Returns the `Arc<HnswIndex>` and the query vector.
    fn prepare_vector_search(
        &self,
        indexes: &CurrentIndexes,
        query_node_id: NodeId,
    ) -> Result<(Arc<HnswIndex>, Arc<[f32]>)> {
        // Get the default property name
        let prop_name = self.get_default_vector_property_name().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Vector index is not enabled".to_string(),
            ))
        })?;

        // Get the index from DashMap (where actual data is stored)
        let entry = self.vector_indexes.get(&prop_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!("Vector index not found for property '{}'", prop_name),
            ))
        })?;

        let query_node = indexes
            .get_node(query_node_id)
            .ok_or(StorageError::NodeNotFound(query_node_id))?;

        // Use as_arc_vector() to get an Arc clone (O(1)) instead of to_vec() (O(n))
        // This avoids unnecessary memory allocation - Issue #188
        let query_vector = query_node
            .properties
            .get(&prop_name)
            .ok_or_else(|| StorageError::PropertyNotFound(prop_name.clone()))?
            .as_arc_vector()
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::InvalidVector {
                        reason: "Property is not a vector".to_string(),
                    },
                )
            })?;

        // Explicitly drop query_node before cloning the index Arc.
        drop(query_node);

        let index = Arc::clone(&entry.value().index);

        Ok((index, query_vector))
    }

    /// Get or create filter statistics for a label (Issue #334).
    fn get_or_create_filter_stats(&self, label: &str) -> Arc<FilterStats> {
        self.filter_stats
            .entry(label.to_string())
            .or_insert_with(|| Arc::new(FilterStats::new()))
            .value()
            .clone()
    }

    /// Calculate adaptive over-fetch candidates for filtered search (Issue #334).
    fn calculate_adaptive_candidates(&self, k: usize, label: &str) -> (usize, Arc<FilterStats>) {
        const MIN_ABSOLUTE_OVERFETCH: usize = 20;
        const MAX_ABSOLUTE_OVERFETCH: usize = 1000;

        let stats = self.get_or_create_filter_stats(label);
        let multiplier = stats.get_adaptive_multiplier();
        let candidates = ((k as f64 * multiplier) as usize)
            .max(k + MIN_ABSOLUTE_OVERFETCH)
            .min(k + MAX_ABSOLUTE_OVERFETCH);

        (candidates, stats)
    }

    /// Get filter statistics for a label (test-only helper).
    pub(crate) fn get_filter_stats(&self, label: &str) -> Option<(u64, u64, u64)> {
        use std::sync::atomic::Ordering;

        self.filter_stats.get(label).map(|entry| {
            let stats = entry.value();
            (
                stats.search_count.load(Ordering::Relaxed),
                stats.total_candidates.load(Ordering::Relaxed),
                stats.total_results.load(Ordering::Relaxed),
            )
        })
    }

    /// Find k most similar nodes to the query node based on vector similarity.
    pub fn find_similar(
        &self,
        indexes: &CurrentIndexes,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let (index, query_vector) = self.prepare_vector_search(indexes, query_node_id)?;

        let mut results = index.search(&query_vector, k + 1)?;
        results.retain(|(id, _)| *id != query_node_id);
        results.truncate(k);
        Ok(results)
    }

    /// Find k most similar nodes with a specific label.
    pub fn find_similar_with_label(
        &self,
        indexes: &CurrentIndexes,
        query_node_id: NodeId,
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let (index, query_vector) = self.prepare_vector_search(indexes, query_node_id)?;
        let label_id = GLOBAL_INTERNER.intern(label)?;

        let (candidates_to_fetch, stats) = self.calculate_adaptive_candidates(k, label);

        let mut results =
            index.search_with_filter(&query_vector, candidates_to_fetch, |node_id| {
                indexes
                    .get_node(*node_id)
                    .map(|n| n.label == label_id)
                    .unwrap_or(false)
            })?;

        results.retain(|(id, _)| *id != query_node_id);
        let results_count = results.len();
        results.truncate(k);

        // Record search statistics for adaptive learning (Issue #334)
        stats.record_search(candidates_to_fetch, results_count);

        Ok(results)
    }

    /// Find k most similar nodes to a raw embedding vector.
    pub fn find_similar_by_embedding(
        &self,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let index = self.prepare_vector_search_raw(embedding)?;
        let results = index.search(embedding, k)?;
        Ok(results)
    }

    /// Find k most similar nodes with a specific label to a raw embedding vector.
    pub fn find_similar_by_embedding_with_label(
        &self,
        indexes: &CurrentIndexes,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let index = self.prepare_vector_search_raw(embedding)?;

        // Intern the label for efficient comparison
        let label_id = GLOBAL_INTERNER.intern(label)?;

        let (candidates_to_fetch, stats) = self.calculate_adaptive_candidates(k, label);

        // Filter during HNSW traversal for better performance
        let mut results = index.search_with_filter(embedding, candidates_to_fetch, |node_id| {
            indexes
                .get_node(*node_id)
                .is_some_and(|n| n.label == label_id)
        })?;

        let results_count = results.len();
        // Truncate to requested k (search_with_filter may return more)
        results.truncate(k);

        // Record search statistics for adaptive learning (Issue #334)
        stats.record_search(candidates_to_fetch, results_count);

        Ok(results)
    }

    /// Find k most similar nodes to a raw embedding in a specific property's index.
    pub fn find_similar_by_embedding_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Validate embedding dimensions
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        let results = entry.value().index.search(embedding, k)?;
        Ok(results)
    }

    /// Find k most similar nodes with a label to a raw embedding in a specific property's index.
    pub fn find_similar_by_embedding_in_with_label(
        &self,
        indexes: &CurrentIndexes,
        property_name: &str,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Validate embedding dimensions
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        let label_id = GLOBAL_INTERNER.intern(label)?;

        let (candidates_to_fetch, stats) = self.calculate_adaptive_candidates(k, label);

        let mut results =
            entry
                .value()
                .index
                .search_with_filter(embedding, candidates_to_fetch, |node_id| {
                    indexes
                        .get_node(*node_id)
                        .is_some_and(|n| n.label == label_id)
                })?;

        let results_count = results.len();
        results.truncate(k);

        // Record search statistics for adaptive learning (Issue #334)
        stats.record_search(candidates_to_fetch, results_count);

        Ok(results)
    }

    /// Find k most similar nodes in a specific property's vector index.
    pub fn find_similar_in(
        &self,
        indexes: &CurrentIndexes,
        property_name: &str,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // Get the index for this property
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Get the query node and its vector
        let query_node = indexes
            .get_node(query_node_id)
            .ok_or(StorageError::NodeNotFound(query_node_id))?;

        let query_vec = query_node
            .properties
            .get(property_name)
            .and_then(|v| v.as_vector())
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "Node {} does not have vector property '{}'",
                        query_node_id.as_u64(),
                        property_name
                    ),
                ))
            })?;

        // Perform the search
        let index = Arc::clone(&entry.value().index);
        let mut results = index.search(query_vec, k + 1)?;
        results.retain(|(id, _)| *id != query_node_id);
        results.truncate(k);
        Ok(results)
    }

    /// Search a specific property's vector index with a raw embedding.
    pub fn search_vectors_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // Get the index for this property
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Validate embedding dimensions
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        // Perform the search
        let index = Arc::clone(&entry.value().index);
        let results = index.search(embedding, k)?;
        Ok(results)
    }

    /// Helper method to prepare for raw embedding vector search.
    fn prepare_vector_search_raw(&self, embedding: &[f32]) -> Result<Arc<HnswIndex>> {
        // Get the default property name
        let prop_name = self.get_default_vector_property_name().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Vector index is not enabled. Call enable_vector_index() first.".to_string(),
            ))
        })?;

        // Get the index from DashMap (where actual data is stored)
        let entry = self.vector_indexes.get(&prop_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!("Vector index not found for property '{}'", prop_name),
            ))
        })?;

        // Validate embedding dimensions match index
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        let index = Arc::clone(&entry.value().index);

        Ok(index)
    }

    /// Enable temporal vector indexing for a specific property.
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: TemporalVectorConfig,
    ) -> Result<()> {
        // Check if already enabled for this specific property
        if self.temporal_vector_indexes.contains_key(property_name) {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::IndexError(format!(
                    "Temporal vector index is already enabled for property '{}'",
                    property_name
                )),
            ));
        }

        // Create temporal vector index wrapped in Arc for sharing
        let index = Arc::new(TemporalVectorIndex::new(config.clone())?);

        // Insert into multi-property DashMap
        self.temporal_vector_indexes.insert(
            property_name.to_string(),
            TemporalVectorIndexEntry {
                index: Arc::clone(&index),
                config: config.clone(),
            },
        );

        // Also update legacy state for backward compatibility
        // (uses the most recently added index)
        let mut state = self.temporal_vector_index_state.write();
        state.index = Some(index);
        state.property_name = Some(property_name.to_string());
        state.config = Some(config);

        Ok(())
    }

    /// Check if temporal vector indexing is enabled for any property.
    pub fn is_temporal_vector_index_enabled(&self) -> bool {
        !self.temporal_vector_indexes.is_empty()
    }

    /// Check if temporal vector indexing is enabled for a specific property.
    pub fn is_temporal_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.temporal_vector_indexes.contains_key(property_name)
    }

    /// List all property names that have temporal vector indexes enabled.
    pub fn list_temporal_vector_indexes(&self) -> Vec<String> {
        self.temporal_vector_indexes
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get a reference to the temporal vector index for a specific property.
    pub(crate) fn get_temporal_vector_index_for(
        &self,
        property_name: &str,
    ) -> Option<Arc<TemporalVectorIndex>> {
        self.temporal_vector_indexes
            .get(property_name)
            .map(|entry| Arc::clone(&entry.index))
    }

    /// Get a reference to the temporal vector index if enabled (legacy single-property API).
    pub(crate) fn get_temporal_vector_index(&self) -> Option<Arc<TemporalVectorIndex>> {
        let state = self.temporal_vector_index_state.read();
        state.index.clone()
    }

    /// Find k most similar nodes at a specific point in time.
    pub fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        let state = self.temporal_vector_index_state.read();
        let index = state.index.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Temporal vector index is not enabled. Call enable_temporal_vector_index() first."
                    .to_string(),
            ))
        })?;

        index.find_similar_as_of(embedding, k, timestamp)
    }

    /// Find k most similar nodes at a specific point in time for a specific property.
    pub fn find_similar_as_of_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
        timestamp: crate::core::temporal::Timestamp,
    ) -> Result<Vec<(crate::core::NodeId, f32)>> {
        // Look up the temporal index for this specific property
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.find_similar_as_of(embedding, k, timestamp)
    }

    /// Track semantic drift for a node over time in a specific property's temporal index.
    pub fn track_drift_in(
        &self,
        property_name: &str,
        node_id: crate::core::NodeId,
        reference_embedding: &[f32],
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(crate::core::temporal::Timestamp, f32)>> {
        // Look up the temporal index for this specific property (multi-property support)
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.track_semantic_drift(node_id, reference_embedding, time_range)
    }

    /// Get the semantic evolution of a node's embedding over time in a specific property.
    pub fn semantic_evolution_in(
        &self,
        property_name: &str,
        node_id: crate::core::NodeId,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(crate::core::temporal::Timestamp, std::sync::Arc<[f32]>)>> {
        // Look up the temporal index for this specific property (multi-property support)
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.semantic_evolution(node_id, time_range)
    }

    /// Find all nodes with semantic drift above a threshold in a specific property.
    pub fn find_drift_in(
        &self,
        property_name: &str,
        threshold: f32,
        time_range: crate::core::temporal::TimeRange,
        metric: crate::index::vector::temporal::DriftMetric,
    ) -> Result<Vec<(crate::core::NodeId, f32)>> {
        // Look up the temporal index for this specific property (multi-property support)
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.find_semantic_drift(threshold, time_range, metric)
    }

    /// Find k most similar nodes across a time range.
    pub fn find_similar_in_range(
        &self,
        embedding: &[f32],
        k: usize,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<TemporalSearchResults> {
        let state = self.temporal_vector_index_state.read();
        let index = state.index.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Temporal vector index is not enabled. Call enable_temporal_vector_index() first."
                    .to_string(),
            ))
        })?;

        index.find_similar_in_range(embedding, k, time_range)
    }

    /// Notify the temporal vector index of a transaction.
    pub fn on_temporal_vector_transaction(&self) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index {
            index.on_transaction()?;
        }
        Ok(())
    }

    /// Helper to index a vector in the temporal index.
    pub(crate) fn try_index_temporal_vector(
        &self,
        node_id: NodeId,
        properties: &PropertyMap,
        timestamp: Timestamp,
    ) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index
            && let Some(prop_name) = &state.property_name
            && let Some(vector) = properties.get(prop_name).and_then(|v| v.as_vector())
        {
            index.add(node_id, vector, timestamp)?;
        }
        Ok(())
    }

    /// Helper to remove a vector from the temporal index.
    pub(crate) fn try_remove_temporal_vector(&self, node_id: NodeId, timestamp: Timestamp) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index {
            index.remove(node_id, timestamp)?;
        }
        Ok(())
    }

    /// Get the name of the property used for vector indexing.
    pub fn get_vector_property_name(&self) -> Option<String> {
        self.get_default_vector_property_name()
    }

    /// Get the number of vectors in the HNSW index.
    pub fn vector_count(&self) -> usize {
        self.get_default_vector_property_name()
            .and_then(|prop_name| self.vector_indexes.get(&prop_name))
            .map(|entry| entry.value().index.len())
            .unwrap_or(0)
    }
}

impl Default for VectorIndexManager {
    fn default() -> Self {
        Self::new()
    }
}
