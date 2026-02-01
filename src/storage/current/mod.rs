//! Current-state storage engine.
//!
//! This module implements the "hot path" storage for the current state of the
//! graph. It provides O(1) lookups and cache-friendly traversals optimized for
//! non-temporal queries.

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::index::current::CurrentIndexes;
use crate::index::vector::hnsw::HnswConfig;
use crate::index::vector::temporal::{TemporalVectorConfig, TemporalVectorIndex};
use crate::index::vector::TemporalSearchResults;
use crate::utils::error::{Result, StorageError};
use std::sync::Arc;

mod iterators;
mod stats;
mod vector;

pub use iterators::*;
pub use stats::CurrentStats;
pub use vector::VectorIndexInfo;
pub use vector::DEFAULT_MAX_VECTOR_PROPERTIES;
use vector::VectorIndexManager;

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
    /// Manager for vector indexes (HNSW and Temporal)
    vector_manager: VectorIndexManager,
}

impl CurrentStorage {
    /// Create a new empty current storage.
    pub fn new() -> Self {
        CurrentStorage {
            indexes: CurrentIndexes::new(),
            node_id_gen: IdGenerator::new(),
            edge_id_gen: IdGenerator::new(),
            version_id_gen: IdGenerator::new(),
            vector_manager: VectorIndexManager::new(),
        }
    }

    /// Initialize the node ID generator with a specific starting value.
    pub(crate) fn init_node_id_generator(&self, start: u64) {
        self.node_id_gen.reset_to(start);
    }

    /// Initialize the edge ID generator with a specific starting value.
    pub(crate) fn init_edge_id_generator(&self, start: u64) {
        self.edge_id_gen.reset_to(start);
    }

    /// Initialize the version ID generator with a specific starting value.
    #[inline]
    pub(crate) fn init_version_id_generator(&self, start: u64) {
        self.version_id_gen.reset_to(start);
    }

    /// Ensure the version ID generator's next value is at least the specified minimum.
    #[inline]
    pub(crate) fn ensure_version_id_generator_at_least(&self, min_value: u64) {
        self.version_id_gen.ensure_at_least(min_value);
    }

    /// Enable vector indexing for a specific property.
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()> {
        self.vector_manager.enable_vector_index(property_name, config)
    }

    /// Check if any vector indexing is enabled.
    pub fn is_vector_index_enabled(&self) -> bool {
        self.vector_manager.is_vector_index_enabled()
    }

    /// Check if vector indexing is enabled for a specific property.
    pub fn is_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.vector_manager
            .is_vector_index_enabled_for(property_name)
    }

    /// Get the first/default property name that is currently indexed.
    pub fn get_indexed_property_name(&self) -> Option<String> {
        self.vector_manager.get_indexed_property_name()
    }

    /// List all configured vector indexes.
    pub fn list_vector_indexes(&self) -> Vec<VectorIndexInfo> {
        self.vector_manager.list_vector_indexes()
    }

    /// Check if a vector index is enabled for a specific property.
    pub fn has_vector_index(&self, property_name: &str) -> bool {
        self.vector_manager.has_vector_index(property_name)
    }

    /// Get the HNSW configuration for a specific property's vector index.
    pub fn get_hnsw_config_for(&self, property_name: &str) -> Option<HnswConfig> {
        self.vector_manager.get_hnsw_config_for(property_name)
    }

    /// Get vector index configuration for checkpoint persistence.
    pub fn get_vector_index_config(
        &self,
    ) -> crate::storage::persistence::VectorIndexCheckpointData {
        self.vector_manager.get_vector_index_config()
    }

    /// Register a vector index (used during index loading from disk).
    pub(crate) fn register_vector_index(
        &self,
        property_name: &str,
        index: crate::index::vector::HnswIndex,
        config: crate::index::vector::HnswConfig,
    ) {
        self.vector_manager
            .register_vector_index(property_name, index, config)
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
        self.vector_manager
            .get_vector_index_for_persistence(property_name)
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
        if let Err(e) = self
            .vector_manager
            .try_index_vector(node_id, &properties)
        {
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

    // ========================================================================
    // Zero-copy access methods
    // ========================================================================

    /// Get the target node of an edge without cloning the entire edge.
    #[inline]
    pub fn get_edge_target(&self, id: EdgeId) -> Result<NodeId> {
        self.indexes
            .get_edge_target(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the source node of an edge without cloning the entire edge.
    #[inline]
    pub fn get_edge_source(&self, id: EdgeId) -> Result<NodeId> {
        self.indexes
            .get_edge_source(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the endpoints (source, target) of an edge without cloning.
    #[inline]
    pub fn get_edge_endpoints(&self, id: EdgeId) -> Result<(NodeId, NodeId)> {
        self.indexes
            .get_edge_endpoints(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the label of an edge without cloning the entire edge.
    #[inline]
    pub fn get_edge_label(&self, id: EdgeId) -> Result<InternedString> {
        self.indexes
            .get_edge_label(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the label of a node without cloning the entire node.
    #[inline]
    pub fn get_node_label(&self, id: NodeId) -> Result<InternedString> {
        self.indexes
            .get_node_label(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Delete a node.
    pub fn delete_node(&self, id: NodeId) -> Result<Node> {
        let node = self
            .indexes
            .remove_node(id)
            .ok_or(StorageError::NodeNotFound(id))?;

        // Best-effort vector index removal (ignore errors)
        let _ = self.vector_manager.try_remove_from_index(id);

        Ok(node)
    }

    /// Delete an edge.
    pub fn delete_edge(&self, id: EdgeId) -> Result<Edge> {
        let edge = self
            .indexes
            .remove_edge(id)
            .ok_or(StorageError::EdgeNotFound(id))?;

        Ok(edge)
    }

    // Direct insert/update/delete methods for transaction commit

    /// Insert a node directly (used by WriteTransaction).
    pub fn insert_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> {
        self.vector_manager
            .try_index_vector(node.id, &node.properties)?;

        self.vector_manager
            .try_index_temporal_vector(node.id, &node.properties, timestamp)?;

        self.indexes.insert_node(node);

        Ok(())
    }

    /// Insert an edge directly (used by WriteTransaction).
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
            && let Err(e) = self.vector_manager.update_vector_index(
                node.id,
                &node.properties,
                &old.properties,
            )
        {
            // Rollback: restore the original node
            self.indexes.insert_node(old.clone());
            return Err(e);
        }

        // Update temporal vector index if enabled
        self.vector_manager
            .try_index_temporal_vector(node.id, &node.properties, timestamp)?;

        Ok(())
    }

    /// Update an edge directly (used by WriteTransaction).
    pub fn update_edge_direct(&self, edge: Edge) -> Result<()> {
        self.indexes.insert_edge(edge);
        Ok(())
    }

    /// Delete a node directly (used by WriteTransaction).
    pub fn delete_node_direct(&self, id: NodeId, timestamp: Timestamp) -> Result<()> {
        self.indexes
            .remove_node(id)
            .ok_or(StorageError::NodeNotFound(id))?;

        let _ = self.vector_manager.try_remove_from_index(id);
        let _ = self.vector_manager.try_remove_temporal_vector(id, timestamp);

        Ok(())
    }

    /// Delete an edge directly (used by WriteTransaction).
    pub fn delete_edge_direct(&self, id: EdgeId) -> Result<()> {
        self.indexes
            .remove_edge(id)
            .ok_or(StorageError::EdgeNotFound(id))?;
        Ok(())
    }

    /// Compact adjacency indexes.
    pub fn compact_adjacency(&self) {
        self.indexes.compact_adjacency();
    }

    /// Rebuild adjacency indexes (deprecated).
    #[deprecated(since = "0.1.0", note = "Use compact_adjacency() instead")]
    pub fn rebuild_adjacency(&self) {
        self.compact_adjacency();
    }

    /// Get a frozen view for outgoing adjacency.
    #[inline]
    pub fn frozen_outgoing_view(
        &self,
    ) -> Option<crate::index::incremental_adjacency::FrozenAdjacencyView> {
        self.indexes.frozen_outgoing_view()
    }

    /// Get a frozen view for incoming adjacency.
    #[inline]
    pub fn frozen_incoming_view(
        &self,
    ) -> Option<crate::index::incremental_adjacency::FrozenAdjacencyView> {
        self.indexes.frozen_incoming_view()
    }

    /// Get all outgoing edges from a node.
    #[inline]
    pub fn get_outgoing_edges(&self, source: NodeId) -> Vec<EdgeId> {
        if let Some(frozen) = self.indexes.frozen_outgoing_view() {
            return frozen
                .get_adjacency(source)
                .iter()
                .map(|entry| entry.edge_id)
                .collect();
        }
        self.indexes
            .get_outgoing(source)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get all incoming edges to a node.
    #[inline]
    pub fn get_incoming_edges(&self, target: NodeId) -> Vec<EdgeId> {
        if let Some(frozen) = self.indexes.frozen_incoming_view() {
            return frozen
                .get_adjacency(target)
                .iter()
                .map(|entry| entry.edge_id)
                .collect();
        }
        self.indexes
            .get_incoming(target)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get outgoing edges with a specific label.
    #[inline]
    pub fn get_outgoing_edges_with_label(&self, source: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };

        self.indexes
            .get_outgoing_with_label(source, label_id)
            .into_iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get incoming edges with a specific label.
    #[inline]
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

    /// Get all outgoing edges from a node as an iterator.
    #[inline]
    pub fn get_outgoing_edges_iter(&self, source: NodeId) -> OutgoingEdgesIter<'_> {
        OutgoingEdgesIter::new(self.indexes.get_outgoing(source))
    }

    /// Get all incoming edges to a node as an iterator.
    #[inline]
    pub fn get_incoming_edges_iter(&self, target: NodeId) -> IncomingEdgesIter<'_> {
        IncomingEdgesIter::new(self.indexes.get_incoming(target))
    }

    /// Get outgoing edges with a specific label as an iterator.
    #[inline]
    pub fn get_outgoing_edges_with_label_iter(
        &self,
        source: NodeId,
        label: &str,
    ) -> OutgoingEdgesWithLabelIter<'_> {
        let label_id = GLOBAL_INTERNER.get_id(label);
        OutgoingEdgesWithLabelIter::new(self.indexes.get_outgoing(source), label_id)
    }

    /// Get incoming edges with a specific label as an iterator.
    #[inline]
    pub fn get_incoming_edges_with_label_iter(
        &self,
        target: NodeId,
        label: &str,
    ) -> IncomingEdgesWithLabelIter<'_> {
        let label_id = GLOBAL_INTERNER.get_id(label);
        IncomingEdgesWithLabelIter::new(self.indexes.get_incoming(target), label_id)
    }

    /// Export outgoing CSR adjacency data for persistence.
    pub fn export_outgoing_csr(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        self.indexes.export_outgoing_csr()
    }

    /// Export incoming CSR adjacency data for persistence.
    pub fn export_incoming_csr(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        self.indexes.export_incoming_csr()
    }

    /// Import CSR adjacency data from persistence.
    pub fn import_csr(
        &self,
        outgoing_node_ids: Vec<u64>,
        outgoing_offsets: Vec<u64>,
        outgoing_edge_ids: Vec<u64>,
        incoming_node_ids: Vec<u64>,
        incoming_offsets: Vec<u64>,
        incoming_edge_ids: Vec<u64>,
    ) {
        self.indexes.import_csr(
            outgoing_node_ids,
            outgoing_offsets,
            outgoing_edge_ids,
            incoming_node_ids,
            incoming_offsets,
            incoming_edge_ids,
        );
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

    /// Get filter statistics for a label (test-only helper).
    pub(crate) fn get_filter_stats(&self, label: &str) -> Option<(u64, u64, u64)> {
        self.vector_manager.get_filter_stats(label)
    }

    /// Find k most similar nodes to the query node based on vector similarity.
    pub fn find_similar(&self, query_node_id: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager
            .find_similar(&self.indexes, query_node_id, k)
    }

    /// Find k most similar nodes with a specific label.
    pub fn find_similar_with_label(
        &self,
        query_node_id: NodeId,
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager
            .find_similar_with_label(&self.indexes, query_node_id, label, k)
    }

    /// Find k most similar nodes to a raw embedding vector.
    pub fn find_similar_by_embedding(
        &self,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager
            .find_similar_by_embedding(embedding, k)
    }

    /// Find k most similar nodes with a specific label to a raw embedding vector.
    pub fn find_similar_by_embedding_with_label(
        &self,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager.find_similar_by_embedding_with_label(
            &self.indexes,
            embedding,
            label,
            k,
        )
    }

    /// Find k most similar nodes to a raw embedding in a specific property's index.
    pub fn find_similar_by_embedding_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager
            .find_similar_by_embedding_in(property_name, embedding, k)
    }

    /// Find k most similar nodes with a label to a raw embedding in a specific property's index.
    pub fn find_similar_by_embedding_in_with_label(
        &self,
        property_name: &str,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager.find_similar_by_embedding_in_with_label(
            &self.indexes,
            property_name,
            embedding,
            label,
            k,
        )
    }

    /// Find k most similar nodes in a specific property's vector index.
    pub fn find_similar_in(
        &self,
        property_name: &str,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager
            .find_similar_in(&self.indexes, property_name, query_node_id, k)
    }

    /// Search a specific property's vector index with a raw embedding.
    pub fn search_vectors_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager
            .search_vectors_in(property_name, embedding, k)
    }

    /// Enable temporal vector indexing for a specific property.
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: TemporalVectorConfig,
    ) -> Result<()> {
        self.vector_manager
            .enable_temporal_vector_index(property_name, config)
    }

    /// Check if temporal vector indexing is enabled.
    pub fn is_temporal_vector_index_enabled(&self) -> bool {
        self.vector_manager.is_temporal_vector_index_enabled()
    }

    /// Check if temporal vector indexing is enabled for a specific property.
    pub fn is_temporal_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.vector_manager
            .is_temporal_vector_index_enabled_for(property_name)
    }

    /// List all property names that have temporal vector indexes enabled.
    pub fn list_temporal_vector_indexes(&self) -> Vec<String> {
        self.vector_manager.list_temporal_vector_indexes()
    }

    pub(crate) fn get_temporal_vector_index(&self) -> Option<Arc<TemporalVectorIndex>> {
        self.vector_manager.get_temporal_vector_index()
    }

    /// Find k most similar nodes at a specific point in time.
    pub fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.vector_manager
            .find_similar_as_of(embedding, k, timestamp)
    }

    /// Find k most similar nodes at a specific point in time for a specific property.
    pub fn find_similar_as_of_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
        timestamp: crate::core::temporal::Timestamp,
    ) -> Result<Vec<(crate::core::NodeId, f32)>> {
        self.vector_manager
            .find_similar_as_of_in(property_name, embedding, k, timestamp)
    }

    /// Track semantic drift for a node over time in a specific property's temporal index.
    pub fn track_drift_in(
        &self,
        property_name: &str,
        node_id: crate::core::NodeId,
        reference_embedding: &[f32],
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(crate::core::temporal::Timestamp, f32)>> {
        self.vector_manager.track_drift_in(
            property_name,
            node_id,
            reference_embedding,
            time_range,
        )
    }

    /// Get the semantic evolution of a node's embedding over time in a specific property.
    pub fn semantic_evolution_in(
        &self,
        property_name: &str,
        node_id: crate::core::NodeId,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(crate::core::temporal::Timestamp, std::sync::Arc<[f32]>)>> {
        self.vector_manager
            .semantic_evolution_in(property_name, node_id, time_range)
    }

    /// Find all nodes with semantic drift above a threshold in a specific property.
    pub fn find_drift_in(
        &self,
        property_name: &str,
        threshold: f32,
        time_range: crate::core::temporal::TimeRange,
        metric: crate::index::vector::temporal::DriftMetric,
    ) -> Result<Vec<(crate::core::NodeId, f32)>> {
        self.vector_manager
            .find_drift_in(property_name, threshold, time_range, metric)
    }

    /// Find k most similar nodes across a time range.
    pub fn find_similar_in_range(
        &self,
        embedding: &[f32],
        k: usize,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<TemporalSearchResults> {
        self.vector_manager
            .find_similar_in_range(embedding, k, time_range)
    }

    /// Notify the temporal vector index of a transaction.
    pub fn on_temporal_vector_transaction(&self) -> Result<()> {
        self.vector_manager.on_temporal_vector_transaction()
    }

    /// Get statistics about the current storage
    pub fn stats(&self) -> CurrentStats {
        CurrentStats {
            node_count: self.node_count(),
            edge_count: self.edge_count(),
        }
    }

    /// Get all node IDs in the current storage.
    pub fn get_all_node_ids(&self) -> Vec<NodeId> {
        self.indexes.iter_node_ids().collect()
    }

    /// Get all edge IDs in the current storage.
    pub fn get_all_edge_ids(&self) -> Vec<EdgeId> {
        self.indexes.iter_edge_ids().collect()
    }

    /// Get all nodes in the current storage.
    pub fn get_all_nodes(&self) -> Vec<crate::Node> {
        self.indexes.iter_nodes().map(|n| n.clone()).collect()
    }

    /// Get all edges in the current storage.
    pub fn get_all_edges(&self) -> Vec<crate::Edge> {
        self.indexes.iter_edges().map(|e| e.clone()).collect()
    }

    /// Get nodes by label.
    pub fn get_nodes_by_label(&self, label: &str) -> Vec<Node> {
        let label_id = match crate::core::interning::GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(), // Label doesn't exist
        };
        self.indexes
            .iter_nodes()
            .filter(|n| n.label == label_id)
            .map(|n| n.clone())
            .collect()
    }

    /// Get the name of the property used for vector indexing.
    pub fn get_vector_property_name(&self) -> Option<String> {
        self.vector_manager.get_vector_property_name()
    }

    /// Get node counts grouped by label.
    pub fn label_counts(&self) -> Vec<(crate::core::interning::InternedString, usize)> {
        use std::collections::HashMap;
        let mut counts: HashMap<crate::core::interning::InternedString, usize> = HashMap::new();
        for node in self.indexes.iter_nodes() {
            *counts.entry(node.label).or_insert(0) += 1;
        }
        counts.into_iter().collect()
    }

    /// Get average out-degree across all nodes.
    pub fn avg_out_degree(&self) -> f64 {
        let node_count = self.node_count();
        if node_count == 0 {
            return 0.0;
        }
        self.edge_count() as f64 / node_count as f64
    }

    /// Get target node IDs from outgoing edges.
    pub fn get_outgoing_targets(&self, source: NodeId) -> Vec<NodeId> {
        self.indexes
            .get_outgoing(source)
            .iter()
            .map(|entry| entry.target)
            .collect()
    }

    /// Get target node IDs from outgoing edges with a specific label.
    pub fn get_outgoing_targets_with_label(&self, source: NodeId, label: &str) -> Vec<NodeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };
        self.indexes
            .get_outgoing_with_label(source, label_id)
            .into_iter()
            .map(|entry| entry.target)
            .collect()
    }

    /// Get source node IDs from incoming edges.
    pub fn get_incoming_sources(&self, target: NodeId) -> Vec<NodeId> {
        self.indexes
            .get_incoming(target)
            .iter()
            .map(|entry| entry.target)
            .collect()
    }

    /// Get source node IDs from incoming edges with a specific label.
    pub fn get_incoming_sources_with_label(&self, target: NodeId, label: &str) -> Vec<NodeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };
        self.indexes
            .get_incoming_with_label(target, label_id)
            .into_iter()
            .map(|entry| entry.target)
            .collect()
    }

    /// Get the number of vectors in the HNSW index.
    pub fn vector_count(&self) -> usize {
        self.vector_manager.vector_count()
    }

    /// Iterate over all nodes (for persistence).
    pub(crate) fn all_nodes(&self) -> impl Iterator<Item = Node> + '_ {
        self.indexes.iter_nodes().map(|n| n.clone())
    }

    /// Iterate over all edges (for persistence).
    pub(crate) fn all_edges(&self) -> impl Iterator<Item = Edge> + '_ {
        self.indexes.iter_edges().map(|e| e.clone())
    }

    /// Create an MVCC snapshot at the specified LSN.
    pub fn create_snapshot(
        &self,
        lsn: crate::storage::wal::LSN,
    ) -> crate::storage::snapshot::CurrentStorageSnapshot {
        use crate::storage::snapshot::CurrentStorageSnapshot;
        use std::sync::Arc;

        // Collect Arc references to all nodes (cheap, ~8 bytes per node)
        let nodes: Vec<Arc<Node>> = self
            .indexes
            .iter_nodes()
            .map(|n| Arc::new(n.clone()))
            .collect();

        // Collect Arc references to all edges (cheap, ~8 bytes per edge)
        let edges: Vec<Arc<Edge>> = self
            .indexes
            .iter_edges()
            .map(|e| Arc::new(e.clone()))
            .collect();

        CurrentStorageSnapshot::new(lsn, nodes, edges)
    }
}

impl Default for CurrentStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
