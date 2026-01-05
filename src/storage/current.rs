//! Current-state storage engine.
//!
//! This module implements the "hot path" storage for the current state of the
//! graph. It provides O(1) lookups and cache-friendly traversals optimized for
//! non-temporal queries.

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::index::current::CurrentIndexes;
use crate::index::vector::VectorIndex;
use crate::index::vector::hnsw::{HnswConfig, HnswIndex};
use crate::index::vector::temporal::{TemporalVectorConfig, TemporalVectorIndex};
use crate::utils::error::{Result, StorageError};
use parking_lot::RwLock;
use std::sync::Arc;

/// Statistics about current storage
#[derive(Debug, Clone)]
pub struct CurrentStats {
    /// Number of nodes
    pub node_count: usize,
    /// Number of edges
    pub edge_count: usize,
}

/// Internal state for vector indexing.
struct VectorIndexState {
    index: Option<Arc<HnswIndex>>,
    property_name: Option<String>,
    config: Option<HnswConfig>,
}

impl VectorIndexState {
    fn new() -> Self {
        VectorIndexState {
            index: None,
            property_name: None,
            config: None,
        }
    }

    fn is_enabled(&self) -> bool {
        self.index.is_some()
    }
}

/// Internal state for temporal vector indexing.
struct TemporalVectorIndexState {
    index: Option<Arc<TemporalVectorIndex>>,
    property_name: Option<String>,
    config: Option<TemporalVectorConfig>,
}

impl TemporalVectorIndexState {
    fn new() -> Self {
        TemporalVectorIndexState {
            index: None,
            property_name: None,
            config: None,
        }
    }

    fn is_enabled(&self) -> bool {
        self.index.is_some()
    }
}

/// Current-state storage engine.
///
/// This storage engine maintains the current version of all nodes and edges,
/// optimized for fast queries without temporal overhead. This is the "fast path"
/// that should achieve <1µs single-hop traversals.
pub struct CurrentStorage {
    /// Indexes for nodes and edges
    indexes: CurrentIndexes,
    /// ID generator for nodes
    node_id_gen: IdGenerator,
    /// ID generator for edges
    edge_id_gen: IdGenerator,
    /// ID generator for versions
    version_id_gen: IdGenerator,
    /// Vector index state
    vector_index_state: Arc<RwLock<VectorIndexState>>,
    /// Temporal vector index state (Phase 3)
    temporal_vector_index_state: Arc<RwLock<TemporalVectorIndexState>>,
}

impl CurrentStorage {
    /// Create a new empty current storage.
    pub fn new() -> Self {
        CurrentStorage {
            indexes: CurrentIndexes::new(),
            node_id_gen: IdGenerator::new(),
            edge_id_gen: IdGenerator::new(),
            version_id_gen: IdGenerator::new(),
            vector_index_state: Arc::new(RwLock::new(VectorIndexState::new())),
            temporal_vector_index_state: Arc::new(RwLock::new(
                TemporalVectorIndexState::new(),
            )),
        }
    }

    /// Enable vector indexing for a specific property.
    ///
    /// Once enabled, nodes with the specified property will be automatically
    /// indexed for similarity search. The property must contain vector values.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - HNSW index configuration
    ///
    /// # Errors
    ///
    /// Returns an error if vector indexing is already enabled.
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()> {
        let mut state = self.vector_index_state.write();
        if state.is_enabled() {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::IndexError(
                    "Vector index is already enabled".to_string(),
                ),
            ));
        }
        let index = HnswIndex::new(config.clone())?;
        state.index = Some(Arc::new(index));
        state.property_name = Some(property_name.to_string());
        state.config = Some(config);
        Ok(())
    }

    /// Check if vector indexing is enabled.
    pub fn is_vector_index_enabled(&self) -> bool {
        self.vector_index_state.read().is_enabled()
    }

    /// Get vector index configuration for checkpoint persistence.
    ///
    /// Returns the current vector index configuration if enabled, or disabled
    /// checkpoint data if no index is active.
    pub fn get_vector_index_config(
        &self,
    ) -> crate::storage::persistence::VectorIndexCheckpointData {
        use crate::storage::persistence::VectorIndexCheckpointData;

        let state = self.vector_index_state.read();

        if let (Some(config), Some(property_name)) =
            (state.config.clone(), state.property_name.clone())
        {
            VectorIndexCheckpointData::enabled(property_name, config)
        } else {
            VectorIndexCheckpointData::disabled()
        }
    }

    /// Try to add a node's vector to the index.
    /// Returns Ok(true) if indexed, Ok(false) if not applicable, Err on failure.
    fn try_index_vector(&self, node_id: NodeId, properties: &PropertyMap) -> Result<bool> {
        let state = self.vector_index_state.read();
        if let (Some(index), Some(prop_name)) = (state.index.as_ref(), state.property_name.as_ref())
            && let Some(vector) = properties.get(prop_name).and_then(|v| v.as_vector())
        {
            let index = Arc::clone(index);
            drop(state); // Drop lock before potentially long operation
            index.add(node_id, vector)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Try to remove a node from the vector index.
    /// Returns Ok(true) if removed, Ok(false) if not applicable, Err on failure.
    fn try_remove_from_index(&self, node_id: NodeId) -> Result<bool> {
        let state = self.vector_index_state.read();
        let Some(ref index) = state.index else {
            return Ok(false);
        };
        let index = Arc::clone(index);
        drop(state);
        index.remove(node_id)?;
        Ok(true)
    }

    /// Update the vector index when node properties change.
    fn update_vector_index(
        &self,
        node_id: NodeId,
        new_props: &PropertyMap,
        old_props: &PropertyMap,
    ) -> Result<()> {
        let state = self.vector_index_state.read();
        let Some(ref index) = state.index else {
            return Ok(());
        };
        let Some(ref prop_name) = state.property_name else {
            return Ok(());
        };
        let old_vec = old_props.get(prop_name).and_then(|v| v.as_vector());
        let new_vec = new_props.get(prop_name).and_then(|v| v.as_vector());
        let index = Arc::clone(index);
        drop(state);
        match (old_vec, new_vec) {
            (None, None) => Ok(()),
            (None, Some(v)) => {
                index.add(node_id, v)?;
                Ok(())
            }
            (Some(_), None) => {
                index.remove(node_id)?;
                Ok(())
            }
            (Some(o), Some(n)) => {
                if o == n {
                    return Ok(());
                }
                // Note: HnswIndex::add() is an upsert operation (remove + add internally)
                index.add(node_id, n)?;
                Ok(())
            }
        }
    }

    /// Create a node with the given label and properties.
    ///
    /// Returns the ID of the newly created node.
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        let node_id = NodeId::new_unchecked(self.node_id_gen.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        let node = Node::new(node_id, label_interned, properties.clone(), version_id);
        self.indexes.insert_node(node.clone());

        // Try to index vector property if enabled
        if let Err(e) = self.try_index_vector(node_id, &properties) {
            // Rollback: remove node from indexes
            self.indexes.remove_node(node_id);
            return Err(e);
        }

        Ok(node_id)
    }

    /// Create an edge between two nodes.
    ///
    /// Returns the ID of the newly created edge.
    pub fn create_edge(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        // Verify nodes exist
        if !self.indexes.contains_node(source) {
            return Err(StorageError::NodeNotFound(source).into());
        }
        if !self.indexes.contains_node(target) {
            return Err(StorageError::NodeNotFound(target).into());
        }

        let edge_id = EdgeId::new_unchecked(self.edge_id_gen.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        let edge = Edge::new(
            edge_id,
            label_interned,
            source,
            target,
            properties,
            version_id,
        );
        self.indexes.insert_edge(edge);

        // Rebuild adjacency indexes
        // TODO: For better performance, batch this or use incremental updates
        self.indexes.rebuild_adjacency();

        Ok(edge_id)
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: NodeId) -> Result<Node> {
        self.indexes
            .get_node(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Get an edge by ID.
    pub fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        self.indexes
            .get_edge(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Delete a node.
    ///
    /// Note: This does not delete edges connected to the node.
    /// TODO: Add cascade delete option.
    pub fn delete_node(&mut self, id: NodeId) -> Result<Node> {
        self.indexes
            .remove_node(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Delete an edge.
    pub fn delete_edge(&mut self, id: EdgeId) -> Result<Edge> {
        let edge = self
            .indexes
            .remove_edge(id)
            .ok_or(StorageError::EdgeNotFound(id))?;

        // Rebuild adjacency indexes
        self.indexes.rebuild_adjacency();

        Ok(edge)
    }

    // Direct insert/update/delete methods for transaction commit
    // These methods are used by WriteTransaction to apply buffered changes

    /// Insert a node directly (used by WriteTransaction).
    /// Does not generate IDs - caller must provide them.
    pub fn insert_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> {
        // CRITICAL: Index vector BEFORE inserting node. If vector indexing fails,
        // we have not modified any graph state, so we can safely return error without rollback.
        // This prevents the VS-030 bug where transaction-created nodes bypassed indexing,
        // causing them to be missing from HNSW index and invisible to find_similar queries.
        self.try_index_vector(node.id, &node.properties)?;

        // Index in temporal vector index if enabled
        self.try_index_temporal_vector(node.id, &node.properties, timestamp)?;

        // Vector indexing succeeded, now insert the node into the main indexes.
        self.indexes.insert_node(node);

        Ok(())
    }

    /// Insert an edge directly (used by WriteTransaction).
    /// Does not generate IDs or rebuild adjacency - caller must handle.
    pub fn insert_edge_direct(&self, edge: Edge) -> Result<()> {
        self.indexes.insert_edge(edge);
        Ok(())
    }

    /// Update a node directly (used by WriteTransaction).
    pub fn update_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> {
        // Save old node for potential rollback
        let old_node = self.indexes.get_node(node.id);

        // Update node
        self.indexes.insert_node(node.clone());

        // Update vector index
        if let Some(ref old) = old_node
            && let Err(e) = self.update_vector_index(node.id, &node.properties, &old.properties)
        {
            // Rollback: restore the original node
            self.indexes.insert_node(old.clone());
            return Err(e);
        }

        // Update temporal vector index if enabled
        self.try_index_temporal_vector(node.id, &node.properties, timestamp)?;

        Ok(())
    }

    /// Update an edge directly (used by WriteTransaction).
    pub fn update_edge_direct(&self, edge: Edge) -> Result<()> {
        // Remove old version and insert new
        self.indexes.insert_edge(edge);
        Ok(())
    }

    /// Delete a node directly (used by WriteTransaction).
    pub fn delete_node_direct(&self, id: NodeId, timestamp: Timestamp) -> Result<()> {
        self.indexes
            .remove_node(id)
            .ok_or(StorageError::NodeNotFound(id))?;

        // Best-effort vector index removal (ignore errors)
        let _ = self.try_remove_from_index(id);

        // Remove from temporal vector index if enabled
        let _ = self.try_remove_temporal_vector(id, timestamp);

        Ok(())
    }

    /// Delete an edge directly (used by WriteTransaction).
    pub fn delete_edge_direct(&self, id: EdgeId) -> Result<()> {
        self.indexes
            .remove_edge(id)
            .ok_or(StorageError::EdgeNotFound(id))?;
        Ok(())
    }

    /// Rebuild adjacency indexes from current edges.
    ///
    /// This should be called after batch edge operations to update the
    /// adjacency indexes for efficient graph traversal.
    ///
    /// # Concurrency Safety
    ///
    /// This method is safe to call concurrently with read operations:
    /// - Uses `RwLock` on adjacency indexes for safe concurrent access
    /// - Readers can continue traversing old indexes while rebuild occurs
    /// - New index is swapped in atomically when rebuild completes
    /// - No stale reads: readers either see old (consistent) or new (consistent) state
    ///
    /// However, concurrent writes should be serialized at a higher level
    /// (e.g., through transaction isolation) to prevent race conditions.
    ///
    /// # Performance
    ///
    /// Complexity: O(E log E) where E is the total number of edges.
    /// This operation acquires a write lock, which will block concurrent
    /// readers for the duration of the rebuild (~microseconds for small graphs,
    /// ~milliseconds for graphs with 10K+ edges).
    pub fn rebuild_adjacency(&self) {
        self.indexes.rebuild_adjacency();
    }

    /// Get all outgoing edges from a node.
    ///
    /// This is the critical "hot path" operation that must be fast.
    pub fn get_outgoing_edges(&self, source: NodeId) -> Vec<EdgeId> {
        self.indexes
            .get_outgoing(source)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get all incoming edges to a node.
    pub fn get_incoming_edges(&self, target: NodeId) -> Vec<EdgeId> {
        self.indexes
            .get_incoming(target)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get outgoing edges with a specific label.
    pub fn get_outgoing_edges_with_label(&self, source: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(), // Label doesn't exist
        };

        self.indexes
            .get_outgoing_with_label(source, label_id)
            .into_iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get incoming edges with a specific label.
    pub fn get_incoming_edges_with_label(&self, target: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };

        self.indexes
            .get_incoming_with_label(target, label_id)
            .into_iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get the number of nodes.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.indexes.node_count()
    }

    /// Get the number of edges.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.indexes.edge_count()
    }

    /// Get the out-degree of a node.
    #[inline]
    pub fn out_degree(&self, node: NodeId) -> usize {
        self.indexes.out_degree(node)
    }

    /// Get the in-degree of a node.
    #[inline]
    pub fn in_degree(&self, node: NodeId) -> usize {
        self.indexes.in_degree(node)
    }

    /// Helper method to prepare for vector search.
    /// Returns the Arc<HnswIndex> and the query vector.
    fn prepare_vector_search(&self, query_node_id: NodeId) -> Result<(Arc<HnswIndex>, Vec<f32>)> {
        let state = self.vector_index_state.read();
        let index = state.index.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Vector index is not enabled".to_string(),
            ))
        })?;
        let prop_name = state.property_name.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Vector property name not set".to_string(),
            ))
        })?;

        let query_node = self
            .indexes
            .get_node(query_node_id)
            .ok_or(StorageError::NodeNotFound(query_node_id))?;
        let query_vec_ref = query_node
            .properties
            .get(prop_name)
            .ok_or_else(|| StorageError::PropertyNotFound(prop_name.clone()))?
            .as_vector()
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::InvalidVector {
                        reason: "Property is not a vector".to_string(),
                    },
                )
            })?;

        let query_vector: Vec<f32> = query_vec_ref.to_vec();
        let index = Arc::clone(index);
        drop(state);
        drop(query_node);

        Ok((index, query_vector))
    }

    /// Find k most similar nodes to the query node based on vector similarity.
    ///
    /// Returns a list of (NodeId, score) pairs sorted by similarity (highest first).
    /// The query node itself is excluded from results.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Query node is not found
    /// - Query node does not have the indexed vector property
    /// - The property is not a vector
    pub fn find_similar(&self, query_node_id: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        let (index, query_vector) = self.prepare_vector_search(query_node_id)?;

        let mut results = index.search(&query_vector, k + 1)?;
        results.retain(|(id, _)| *id != query_node_id);
        results.truncate(k);
        Ok(results)
    }

    /// Find k most similar nodes with a specific label.
    ///
    /// Returns a list of (NodeId, score) pairs sorted by similarity (highest first).
    /// Only nodes with the specified label are returned. The query node is excluded.
    pub fn find_similar_with_label(
        &self,
        query_node_id: NodeId,
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let (index, query_vector) = self.prepare_vector_search(query_node_id)?;
        let label_id = GLOBAL_INTERNER.intern(label)?;

        // Fetch a multiple of k candidates to increase the chance of finding enough matches.
        // Cap the over-fetch to prevent excessive memory usage with large k.
        let candidates_to_fetch = (k * 10).max(k + 20).min(k + 1000);
        let mut results =
            index.search_with_filter(&query_vector, candidates_to_fetch, |node_id| {
                self.indexes
                    .get_node(*node_id)
                    .map(|n| n.label == label_id)
                    .unwrap_or(false)
            })?;

        results.retain(|(id, _)| *id != query_node_id);
        results.truncate(k);
        Ok(results)
    }

    /// Find k most similar nodes to a raw embedding vector.
    ///
    /// This is useful when searching with embeddings that don't correspond to any
    /// existing node in the graph (e.g., query embeddings from external sources).
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
    pub fn find_similar_by_embedding_with_label(
        &self,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let index = self.prepare_vector_search_raw(embedding)?;

        // Intern the label for efficient comparison
        let label_id = GLOBAL_INTERNER.intern(label)?;

        // Use adaptive over-fetch heuristic for filtered search
        let candidates_to_fetch = (k * 10).max(k + 20).min(k + 1000);

        // Filter during HNSW traversal for better performance
        let mut results = index.search_with_filter(embedding, candidates_to_fetch, |node_id| {
            self.indexes
                .get_node(*node_id)
                .is_some_and(|n| n.label == label_id)
        })?;

        // Truncate to requested k (search_with_filter may return more)
        results.truncate(k);
        Ok(results)
    }

    /// Helper method to prepare for raw embedding vector search.
    /// Returns the Arc<HnswIndex> and validates the embedding.
    fn prepare_vector_search_raw(&self, embedding: &[f32]) -> Result<Arc<HnswIndex>> {
        let state = self.vector_index_state.read();
        let index = state.index.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Vector index is not enabled. Call enable_vector_index() first.".to_string(),
            ))
        })?;

        // Validate embedding dimensions match index
        let expected_dims = index.dimensions();
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        let index = Arc::clone(index);
        drop(state);

        Ok(index)
    }

    // ========================================================================
    // Temporal Vector Indexing (Phase 3)
    // ========================================================================

    /// Enable temporal vector indexing for a specific property.
    ///
    /// Once enabled, vector changes will be tracked over time using snapshot-based
    /// indexing, enabling point-in-time vector queries and semantic drift tracking.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - Temporal vector index configuration
    ///
    /// # Errors
    ///
    /// Returns an error if temporal vector indexing is already enabled.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy};
    /// use gallifreydb::index::vector::HnswConfig;
    ///
    /// let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
    /// storage.enable_temporal_vector_index("embedding", temporal_config)?;
    /// ```
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: TemporalVectorConfig,
    ) -> Result<()> {
        let mut state = self.temporal_vector_index_state.write();
        if state.is_enabled() {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::IndexError(
                    "Temporal vector index is already enabled".to_string(),
                ),
            ));
        }

        let index = TemporalVectorIndex::new(config.clone())?;
        state.index = Some(Arc::new(index));
        state.property_name = Some(property_name.to_string());
        state.config = Some(config);

        Ok(())
    }

    /// Check if temporal vector indexing is enabled.
    pub fn is_temporal_vector_index_enabled(&self) -> bool {
        self.temporal_vector_index_state.read().is_enabled()
    }

    /// Find k most similar nodes at a specific point in time.
    ///
    /// Returns nodes similar to the query embedding as they existed at the given timestamp.
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query vector
    /// * `k` - Number of results
    /// * `timestamp` - Point in time to query
    ///
    /// # Returns
    ///
    /// Vector of (NodeId, similarity) pairs sorted by similarity (descending).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - No snapshot exists at or before the timestamp
    /// - Embedding dimensions don't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find similar documents as they existed in the past
    /// let query_embedding = vec![0.1; 384];
    /// let timestamp = 1234567890000000; // microseconds since epoch
    /// let results = storage.find_similar_as_of(&query_embedding, 10, timestamp)?;
    /// ```
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

    /// Find k most similar nodes across a time range.
    ///
    /// Returns results for each snapshot within the time range, showing how
    /// semantic similarity evolved over time.
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query vector
    /// * `k` - Number of results per snapshot
    /// * `time_range` - Time range to query
    ///
    /// # Returns
    ///
    /// Vector of (timestamp, results) pairs where results are Vec<(NodeId, similarity)>.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::core::temporal::TimeRange;
    ///
    /// // Track how similar documents changed over time
    /// let query = vec![0.1; 384];
    /// let time_range = TimeRange::between(start_ts, end_ts);
    /// let results = storage.find_similar_in_range(&query, 10, time_range)?;
    /// for (timestamp, similar_nodes) in results {
    ///     println!("At timestamp {}: {} similar nodes", timestamp, similar_nodes.len());
    /// }
    /// ```
    pub fn find_similar_in_range(
        &self,
        embedding: &[f32],
        k: usize,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(Timestamp, Vec<(NodeId, f32)>)>> {
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
    ///
    /// This should be called after committing a transaction to trigger snapshot
    /// creation based on the configured strategy.
    pub fn on_temporal_vector_transaction(&self) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index {
            index.on_transaction()?;
        }
        Ok(())
    }

    /// Helper to index a vector in the temporal index.
    fn try_index_temporal_vector(
        &self,
        node_id: NodeId,
        properties: &PropertyMap,
        timestamp: Timestamp,
    ) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index {
            if let Some(prop_name) = &state.property_name {
                if let Some(vector) = properties.get(prop_name).and_then(|v| v.as_vector()) {
                    index.add(node_id, vector, timestamp)?;
                }
            }
        }
        Ok(())
    }

    /// Helper to remove a vector from the temporal index.
    fn try_remove_temporal_vector(&self, node_id: NodeId, timestamp: Timestamp) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index {
            index.remove(node_id, timestamp)?;
        }
        Ok(())
    }

    /// Get statistics about the current storage
    pub fn stats(&self) -> CurrentStats {
        CurrentStats {
            node_count: self.node_count(),
            edge_count: self.edge_count(),
        }
    }
}

impl Default for CurrentStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_node() {
        let storage = CurrentStorage::new();

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = storage.create_node("Person", props).unwrap();

        assert_eq!(storage.node_count(), 1);

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_create_edge() {
        let storage = CurrentStorage::new();

        let alice = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = storage
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        assert_eq!(storage.edge_count(), 1);

        let edge = storage.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, alice);
        assert_eq!(edge.target, bob);
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_int()),
            Some(2020)
        );
    }

    #[test]
    fn test_create_edge_invalid_nodes() {
        let storage = CurrentStorage::new();

        let result = storage.create_edge(
            NodeId::new(999).unwrap(),
            NodeId::new(1000).unwrap(),
            "KNOWS",
            PropertyMapBuilder::new().build(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_graph_traversal() {
        let storage = CurrentStorage::new();

        // Create nodes
        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges
        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Test outgoing edges
        let outgoing = storage.get_outgoing_edges(n0);
        assert_eq!(outgoing.len(), 2);

        // Test incoming edges
        let incoming = storage.get_incoming_edges(n2);
        assert_eq!(incoming.len(), 2);

        // Test degree
        assert_eq!(storage.out_degree(n0), 2);
        assert_eq!(storage.in_degree(n2), 2);
    }

    #[test]
    fn test_labeled_edges() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Get only KNOWS edges
        let knows_edges = storage.get_outgoing_edges_with_label(n0, "KNOWS");
        assert_eq!(knows_edges.len(), 1);

        // Get only FOLLOWS edges
        let follows_edges = storage.get_outgoing_edges_with_label(n0, "FOLLOWS");
        assert_eq!(follows_edges.len(), 1);

        // Non-existent label
        let none_edges = storage.get_outgoing_edges_with_label(n0, "LOVES");
        assert_eq!(none_edges.len(), 0);
    }

    #[test]
    fn test_delete_node() {
        let mut storage = CurrentStorage::new();

        let node_id = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.node_count(), 1);

        let deleted = storage.delete_node(node_id).unwrap();
        assert_eq!(deleted.id, node_id);
        assert_eq!(storage.node_count(), 0);

        // Second delete should fail
        assert!(storage.delete_node(node_id).is_err());
    }

    #[test]
    fn test_delete_edge() {
        let mut storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.edge_count(), 1);
        assert_eq!(storage.out_degree(n0), 1);

        storage.delete_edge(edge_id).unwrap();

        assert_eq!(storage.edge_count(), 0);
        assert_eq!(storage.out_degree(n0), 0);
    }

    // ========================================================================
    // Vector Property Tests (VS-011)
    // ========================================================================

    #[test]
    fn test_create_node_with_vector_property() {
        let storage = CurrentStorage::new();

        // Create a node with an embedding vector
        let embedding = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
        let props = PropertyMapBuilder::new()
            .insert("name", "Document")
            .insert_vector("embedding", &embedding)
            .build();

        let node_id = storage.create_node("Document", props).unwrap();

        // Retrieve and verify
        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Document")
        );

        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_high_dimensional_vector() {
        let storage = CurrentStorage::new();

        // Create a 384-dimensional vector (common embedding size)
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 / 384.0).collect();
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding)
            .build();

        let node_id = storage.create_node("Embedding", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_create_edge_with_vector_property() {
        let storage = CurrentStorage::new();

        // Create nodes
        let n1 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edge with a relationship embedding
        let edge_embedding = vec![0.5f32, -0.3, 0.8];
        let props = PropertyMapBuilder::new()
            .insert("weight", 0.95f64)
            .insert_vector("embedding", &edge_embedding)
            .build();

        let edge_id = storage.create_edge(n1, n2, "RELATES_TO", props).unwrap();

        // Retrieve and verify
        let edge = storage.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_float()),
            Some(0.95)
        );

        assert_eq!(
            edge.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&edge_embedding[..])
        );
    }

    #[test]
    fn test_update_node_vector_property() {
        let storage = CurrentStorage::new();

        // Create node with initial embedding
        let initial_embedding = vec![0.1f32, 0.2, 0.3, 0.0];
        let props = PropertyMapBuilder::new()
            .insert("name", "Document")
            .insert_vector("embedding", &initial_embedding)
            .build();

        let node_id = storage.create_node("Document", props).unwrap();

        // Get node, update embedding, and save
        let mut node = storage.get_node(node_id).unwrap();

        // Update with new embedding
        let updated_embedding = vec![0.9f32, 0.8, 0.7, 0.0];
        let new_props = PropertyMapBuilder::new()
            .insert("name", "Document")
            .insert_vector("embedding", &updated_embedding)
            .build();
        node.properties = new_props;

        storage.update_node_direct(node).unwrap();

        // Verify update
        let updated_node = storage.get_node(node_id).unwrap();
        assert_eq!(
            updated_node
                .get_property("embedding")
                .and_then(|v| v.as_vector()),
            Some(&updated_embedding[..])
        );
    }

    #[test]
    fn test_update_edge_vector_property() {
        let storage = CurrentStorage::new();

        let n1 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edge with initial embedding
        let initial_embedding = vec![1.0f32, 0.0];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &initial_embedding)
            .build();

        let edge_id = storage.create_edge(n1, n2, "RELATES_TO", props).unwrap();

        // Update edge embedding
        let mut edge = storage.get_edge(edge_id).unwrap();
        let updated_embedding = vec![0.0f32, 1.0];
        edge.properties = PropertyMapBuilder::new()
            .insert_vector("embedding", &updated_embedding)
            .build();

        storage.update_edge_direct(edge).unwrap();

        // Verify
        let updated_edge = storage.get_edge(edge_id).unwrap();
        assert_eq!(
            updated_edge
                .get_property("embedding")
                .and_then(|v| v.as_vector()),
            Some(&updated_embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_multiple_vector_properties() {
        let storage = CurrentStorage::new();

        // Node with multiple embeddings (e.g., different model embeddings)
        let text_embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let image_embedding = vec![0.5f32, 0.6, 0.7, 0.8];
        let props = PropertyMapBuilder::new()
            .insert("content", "multimodal content")
            .insert_vector("text_embedding", &text_embedding)
            .insert_vector("image_embedding", &image_embedding)
            .build();

        let node_id = storage.create_node("MultimodalDoc", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("text_embedding")
                .and_then(|v| v.as_vector()),
            Some(&text_embedding[..])
        );
        assert_eq!(
            node.get_property("image_embedding")
                .and_then(|v| v.as_vector()),
            Some(&image_embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_empty_vector() {
        let storage = CurrentStorage::new();

        // Empty vector should be allowed
        let empty_embedding: Vec<f32> = vec![];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &empty_embedding)
            .build();

        let node_id = storage.create_node("EmptyVec", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&empty_embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_normalized_vector() {
        let storage = CurrentStorage::new();

        // Vector with normalized values (common for embeddings)
        let normalized_embedding = vec![0.5773503f32, 0.5773503, 0.5773503, 0.0]; // unit vector
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &normalized_embedding)
            .build();

        let node_id = storage.create_node("NormalizedDoc", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        let retrieved = node
            .get_property("embedding")
            .and_then(|v| v.as_vector())
            .expect("Embedding property should exist and be a vector");

        // Verify magnitude is approximately 1.0
        let magnitude: f32 = retrieved.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-5);
    }
    // ========================================================================
    // Vector Index Integration Tests (VS-030)
    // ========================================================================

    #[test]
    fn test_enable_vector_index() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();
        assert!(!storage.is_vector_index_enabled());

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();
        assert!(storage.is_vector_index_enabled());
    }

    #[test]
    fn test_enable_vector_index_twice_fails() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("embedding", config.clone())
            .unwrap();

        let result = storage.enable_vector_index("embedding", config);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_index_on_create() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding)
            .build();
        let node_id = storage.create_node("Document", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_auto_index_dimension_mismatch_rolls_back() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        // Wrong dimension - 2D instead of 3D
        let wrong_embedding = vec![1.0f32, 0.0];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &wrong_embedding)
            .build();

        let result = storage.create_node("Document", props);
        assert!(result.is_err());
        assert_eq!(storage.node_count(), 0); // Rollback worked
    }

    #[test]
    fn test_find_similar() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let v3 = vec![0.0f32, 1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();
        let _node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v3)
                    .build(),
            )
            .unwrap();

        let results = storage.find_similar(node1, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(id, _)| *id != node1));
        assert_eq!(results[0].0, node2); // Most similar
    }

    #[test]
    fn test_find_similar_with_label() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let v3 = vec![0.8f32, 0.2, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();
        let _node3 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v3)
                    .build(),
            )
            .unwrap();

        let results = storage.find_similar_with_label(node1, "Person", 2).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node2);
    }

    #[test]
    fn test_find_similar_index_not_enabled() {
        let storage = CurrentStorage::new();
        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();

        let result = storage.find_similar(node1, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_find_similar_node_not_found() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let result = storage.find_similar(NodeId::new(999).unwrap(), 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_find_similar_property_not_found() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new().insert("name", "test").build(),
            )
            .unwrap();
        let result = storage.find_similar(node1, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_node_updates_index() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.0f32, 1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();

        // Update node1 to be similar to node2
        let v1_updated = vec![0.1f32, 0.9, 0.0, 0.0];
        let mut node1_obj = storage.get_node(node1).unwrap();
        node1_obj.properties = PropertyMapBuilder::new()
            .insert_vector("embedding", &v1_updated)
            .build();
        storage.update_node_direct(node1_obj).unwrap();

        let results = storage.find_similar(node2, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);
    }

    #[test]
    fn test_delete_node_removes_from_index() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();

        storage.delete_node_direct(node2).unwrap();

        let results = storage.find_similar(node1, 2).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_create_node_without_vector_property() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        // Create node without vector - should succeed
        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new().insert("name", "test").build(),
            )
            .unwrap();
        assert_eq!(storage.node_count(), 1);
        let node = storage.get_node(node1).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("test")
        );
    }
}
