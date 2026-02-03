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
use crate::index::vector::hnsw::{HnswConfig, HnswIndex};
use crate::index::vector::temporal::{TemporalVectorConfig, TemporalVectorIndex};
use crate::index::vector::{TemporalSearchResults, VectorIndex};
use crate::utils::error::{Result, StorageError};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use parking_lot::RwLock;
use std::sync::Arc;

mod iterators;
mod stats;
mod vector;

pub use iterators::*;
pub use stats::CurrentStats;
use stats::FilterStats;
pub use vector::VectorIndexInfo;
use vector::{TemporalVectorIndexEntry, TemporalVectorIndexState, VectorIndexEntry};

/// Maximum number of vector-indexed properties allowed per database.
///
/// This limit prevents resource exhaustion from enabling too many vector indexes,
/// as each index consumes significant memory (1.5-2x the raw vector data size
/// due to HNSW graph overhead).
///
/// Override via configuration if your use case requires more properties.
pub const DEFAULT_MAX_VECTOR_PROPERTIES: usize = 10;

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
    ///
    /// **Memory Considerations**: This map grows unbounded as new labels are encountered.
    /// For workloads with bounded label sets (typical case), memory usage is negligible
    /// (~100 bytes per unique label). For unbounded labels (e.g., using UUIDs as labels),
    /// memory usage scales linearly with unique label count (~50MB per 1M labels).
    ///
    /// **Recommendation**: Use a bounded set of labels for optimal performance.
    /// Future enhancement could add LRU eviction for unbounded scenarios.
    filter_stats: DashMap<String, Arc<FilterStats>>,
}

impl CurrentStorage {
    /// Create a new empty current storage.
    pub fn new() -> Self {
        CurrentStorage {
            indexes: CurrentIndexes::new(),
            node_id_gen: IdGenerator::new(),
            edge_id_gen: IdGenerator::new(),
            version_id_gen: IdGenerator::new(),
            vector_indexes: DashMap::new(),
            temporal_vector_indexes: DashMap::new(),
            temporal_vector_index_state: Arc::new(RwLock::new(TemporalVectorIndexState::new())),
            filter_stats: DashMap::new(),
        }
    }

    /// Initialize the node ID generator with a specific starting value.
    ///
    /// This is used during recovery to ensure the ID generator continues from
    /// the maximum ID found in the WAL, preventing ID conflicts.
    ///
    /// # Arguments
    ///
    /// * `start` - The next ID to generate (typically max_id + 1)
    pub(crate) fn init_node_id_generator(&self, start: u64) {
        self.node_id_gen.reset_to(start);
    }

    /// Initialize the edge ID generator with a specific starting value.
    ///
    /// This is used during recovery to ensure the ID generator continues from
    /// the maximum ID found in the WAL, preventing ID conflicts.
    ///
    /// # Arguments
    ///
    /// * `start` - The next ID to generate (typically max_id + 1)
    pub(crate) fn init_edge_id_generator(&self, start: u64) {
        self.edge_id_gen.reset_to(start);
    }

    /// Initialize the version ID generator with a specific starting value.
    ///
    /// This is used during recovery to ensure the ID generator continues from
    /// the maximum ID found in the WAL, preventing ID conflicts.
    ///
    /// # Arguments
    ///
    /// * `start` - The next ID to generate (typically max_id + 1)
    #[inline]
    pub(crate) fn init_version_id_generator(&self, start: u64) {
        self.version_id_gen.reset_to(start);
    }

    /// Ensure the version ID generator's next value is at least the specified minimum.
    ///
    /// This is used during recovery when we need to account for version IDs from multiple
    /// sources (e.g., current storage and historical storage) without overwriting
    /// a higher value that was already set.
    ///
    /// # Arguments
    ///
    /// * `min_value` - The minimum next version ID to generate
    #[inline]
    pub(crate) fn ensure_version_id_generator_at_least(&self, min_value: u64) {
        self.version_id_gen.ensure_at_least(min_value);
    }

    /// Enable vector indexing for a specific property.
    ///
    /// Multiple properties can be indexed simultaneously with different configurations.
    /// Each property can have its own dimensions and distance metric.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - HNSW index configuration
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - This specific property already has a vector index enabled
    /// - The maximum number of indexed properties ([`DEFAULT_MAX_VECTOR_PROPERTIES`]) is reached
    ///
    /// # Memory Usage
    ///
    /// Each vector index consumes approximately **1.5-2x** the raw vector data size due to
    /// HNSW graph overhead (neighbor lists, layer structure). For example:
    ///
    /// | Vectors | Dimensions | Raw Size | Index Size |
    /// |---------|------------|----------|------------|
    /// | 100K    | 384        | ~150 MB  | ~225-300 MB |
    /// | 1M      | 768        | ~3 GB    | ~4.5-6 GB   |
    /// | 1M      | 1536       | ~6 GB    | ~9-12 GB    |
    ///
    /// Plan capacity accordingly when enabling multiple property indexes.
    ///
    /// # Persistence Limitation
    ///
    /// **Warning**: Currently, the checkpoint format only supports persisting a single
    /// vector index configuration (the "default" one, which is alphabetically first).
    /// If you enable multiple vector indexes, **only one will be persisted** in checkpoints.
    /// The others will be lost upon recovery from a checkpoint. This limitation will be
    /// addressed in a future update to the checkpoint format.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Index title embeddings (384 dimensions)
    /// storage.enable_vector_index("title_embedding", config_384)?;
    /// // Index body embeddings (1536 dimensions) - different property, OK!
    /// storage.enable_vector_index("body_embedding", config_1536)?;
    /// ```
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
    ///
    /// Returns `true` if at least one property has a vector index.
    pub fn is_vector_index_enabled(&self) -> bool {
        !self.vector_indexes.is_empty()
    }

    /// Check if vector indexing is enabled for a specific property.
    ///
    /// Returns `true` if the property has a vector index configured.
    pub fn is_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.vector_indexes.contains_key(property_name)
    }

    /// Get the first/default property name that is currently indexed.
    ///
    /// Returns `Some(property_name)` if a vector index is enabled,
    /// or `None` if no index is configured.
    ///
    /// Note: For multi-property setups, use [`list_vector_indexes`](Self::list_vector_indexes) instead.
    pub fn get_indexed_property_name(&self) -> Option<String> {
        self.get_default_vector_property_name()
    }

    /// List all configured vector indexes.
    ///
    /// Returns information about each enabled vector index including
    /// property name, dimensions, and distance metric.
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
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name to check
    ///
    /// # Returns
    ///
    /// `true` if a vector index exists for this property, `false` otherwise.
    pub fn has_vector_index(&self, property_name: &str) -> bool {
        self.vector_indexes.contains_key(property_name)
    }

    /// Get the HNSW configuration for a specific property's vector index.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name to get configuration for
    ///
    /// # Returns
    ///
    /// `Some(HnswConfig)` if a vector index exists for this property, `None` otherwise.
    pub fn get_hnsw_config_for(&self, property_name: &str) -> Option<HnswConfig> {
        self.vector_indexes
            .get(property_name)
            .map(|entry| entry.config.clone())
    }

    /// Get vector index configuration for checkpoint persistence.
    ///
    /// Returns the current vector index configuration if enabled, or disabled
    /// checkpoint data if no index is active.
    ///
    /// # Limitation
    ///
    /// Currently, this only returns the configuration for the "default" vector index
    /// (alphabetically first property name). If multiple vector indexes are enabled,
    /// only the default one will be included in the checkpoint.
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
    ///
    /// This directly inserts the index without any initialization logic.
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
    ///
    /// Used for persistence operations. Returns (index, config, vector_count, mappings).
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
    ///
    /// For each enabled vector index, checks if the node has that property
    /// and adds it to the index if so.
    ///
    /// Returns Ok(true) if any vector was indexed, Ok(false) if none applicable,
    /// Err on failure (will have already done partial work).
    fn try_index_vector(&self, node_id: NodeId, properties: &PropertyMap) -> Result<bool> {
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
    ///
    /// Returns Ok(true) if removed from any index, Ok(false) if not applicable.
    fn try_remove_from_index(&self, node_id: NodeId) -> Result<bool> {
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
    ///
    /// For each enabled vector index, handles add/remove/update based on
    /// whether the property changed.
    fn update_vector_index(
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

        // Adjacency indexes are now rebuilt lazily on first access (see CurrentIndexes)
        // This eliminates O(n² log n) performance regression for batch operations

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
    // Zero-copy access methods (Issue #190)
    //
    // These methods provide efficient access to node/edge data without cloning
    // the entire structure. Use these for hot paths where you only need to
    // read specific fields.
    // ========================================================================

    /// Get the target node of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the target NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_target(&self, id: EdgeId) -> Result<NodeId> {
        self.indexes
            .get_edge_target(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the source node of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the source NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_source(&self, id: EdgeId) -> Result<NodeId> {
        self.indexes
            .get_edge_source(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the endpoints (source, target) of an edge without cloning.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns two NodeIds (16 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_endpoints(&self, id: EdgeId) -> Result<(NodeId, NodeId)> {
        self.indexes
            .get_edge_endpoints(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the label of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the label (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_label(&self, id: EdgeId) -> Result<InternedString> {
        self.indexes
            .get_edge_label(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Get the label of a node without cloning the entire node.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the label (8 bytes)
    /// - **No allocation**: Does not clone Node or PropertyMap
    #[inline]
    pub fn get_node_label(&self, id: NodeId) -> Result<InternedString> {
        self.indexes
            .get_node_label(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Delete a node.
    ///
    /// # Important
    ///
    /// This method does NOT delete edges connected to the node. This may leave
    /// orphaned edges in the graph. For most use cases, prefer using
    /// [`crate::api::transaction::WriteOps::delete_node_cascade`] which automatically removes
    /// all connected edges to maintain referential integrity.
    ///
    /// Only use this method if you explicitly need to preserve edges for some
    /// specialized use case (e.g., maintaining edge history for audit purposes).
    pub fn delete_node(&self, id: NodeId) -> Result<Node> {
        let node = self
            .indexes
            .remove_node(id)
            .ok_or(StorageError::NodeNotFound(id))?;

        // Best-effort vector index removal (ignore errors) - Issue #323
        // This prevents memory leaks and ensures deleted nodes don't appear in similarity searches
        // Note: Temporal vector index cleanup is only available in delete_node_direct()
        // which has timestamp context from the transaction. Non-transactional deletes
        // via this method don't have temporal semantics.
        let _ = self.try_remove_from_index(id);

        Ok(node)
    }

    /// Delete an edge.
    pub fn delete_edge(&self, id: EdgeId) -> Result<Edge> {
        let edge = self
            .indexes
            .remove_edge(id)
            .ok_or(StorageError::EdgeNotFound(id))?;

        // Adjacency indexes are now rebuilt lazily on first access (see CurrentIndexes)
        // This eliminates O(n² log n) performance regression for batch operations

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
    /// Compact adjacency indexes to merge delta buffer into frozen CSR.
    ///
    /// With incremental adjacency, edges are immediately visible without compaction.
    /// This method improves read performance by moving delta edges to frozen CSR.
    ///
    /// # When to Call
    ///
    /// - After bulk inserts (to move many edges from delta to frozen)
    /// - To reduce delta size before persistence
    /// - Usually not needed if background compaction is enabled
    ///
    /// # Performance
    ///
    /// Complexity: O(D log D) where D is the number of delta edges.
    /// Typically much faster than the old O(E log E) rebuild where E is total edges.
    pub fn compact_adjacency(&self) {
        self.indexes.compact_adjacency();
    }

    /// Rebuild adjacency indexes (deprecated - use `compact_adjacency` instead).
    ///
    /// This method is kept for backward compatibility and simply calls `compact_adjacency`.
    /// Edges are always immediately visible without calling this method.
    #[deprecated(since = "0.1.0", note = "Use compact_adjacency() instead")]
    pub fn rebuild_adjacency(&self) {
        self.compact_adjacency();
    }

    /// Get a frozen view for outgoing adjacency (read transaction hot path).
    ///
    /// Returns `Some(FrozenAdjacencyView)` if the index is in a clean state
    /// (no pending delta edges or tombstones), allowing direct slice access.
    /// Returns `None` if there are pending delta operations.
    ///
    /// # Performance
    ///
    /// When available, frozen view provides ~8-14ns access vs ~16-17ns for
    /// the merged path. Use this for read-only workloads.
    #[inline]
    pub fn frozen_outgoing_view(
        &self,
    ) -> Option<crate::index::incremental_adjacency::FrozenAdjacencyView> {
        self.indexes.frozen_outgoing_view()
    }

    /// Get a frozen view for incoming adjacency (read transaction hot path).
    ///
    /// Same as `frozen_outgoing_view()` but for incoming edges.
    #[inline]
    pub fn frozen_incoming_view(
        &self,
    ) -> Option<crate::index::incremental_adjacency::FrozenAdjacencyView> {
        self.indexes.frozen_incoming_view()
    }

    /// Get all outgoing edges from a node.
    ///
    /// This is the critical "hot path" operation that must be fast.
    /// Uses frozen view (~8-14ns) when available, falls back to merged guard (~16-17ns).
    #[inline]
    pub fn get_outgoing_edges(&self, source: NodeId) -> Vec<EdgeId> {
        // HOT PATH: Use frozen view for direct slice access when no delta/tombstones
        if let Some(frozen) = self.indexes.frozen_outgoing_view() {
            return frozen
                .get_adjacency(source)
                .iter()
                .map(|entry| entry.edge_id)
                .collect();
        }

        // SLOW PATH: Use merged guard when delta/tombstones exist.
        // Pre-allocate using capacity hint to avoid multiple reallocations.
        let guard = self.indexes.get_outgoing(source);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(guard.iter().map(|entry| entry.edge_id));
        result
    }

    /// Get all incoming edges to a node.
    ///
    /// Uses frozen view (~8-14ns) when available, falls back to merged guard (~16-17ns).
    #[inline]
    pub fn get_incoming_edges(&self, target: NodeId) -> Vec<EdgeId> {
        // HOT PATH: Use frozen view for direct slice access when no delta/tombstones
        if let Some(frozen) = self.indexes.frozen_incoming_view() {
            return frozen
                .get_adjacency(target)
                .iter()
                .map(|entry| entry.edge_id)
                .collect();
        }

        // SLOW PATH: Use merged guard when delta/tombstones exist.
        // Pre-allocate using capacity hint to avoid multiple reallocations.
        let guard = self.indexes.get_incoming(target);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(guard.iter().map(|entry| entry.edge_id));
        result
    }

    /// Get outgoing edges with a specific label.
    #[inline]
    pub fn get_outgoing_edges_with_label(&self, source: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(), // Label doesn't exist
        };

        // Optimized to avoid intermediate Vec allocation and pre-allocate result
        let guard = self.indexes.get_outgoing(source);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(
            guard
                .iter()
                .filter(|entry| entry.label == label_id)
                .map(|entry| entry.edge_id),
        );
        result
    }

    /// Get incoming edges with a specific label.
    #[inline]
    pub fn get_incoming_edges_with_label(&self, target: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };

        // Optimized to avoid intermediate Vec allocation and pre-allocate result
        let guard = self.indexes.get_incoming(target);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(
            guard
                .iter()
                .filter(|entry| entry.label == label_id)
                .map(|entry| entry.edge_id),
        );
        result
    }

    /// Get all outgoing edges from a node as an iterator.
    ///
    /// This is a zero-allocation alternative to [`Self::get_outgoing_edges`] that returns
    /// an iterator instead of collecting into a `Vec`. Use this for performance-critical
    /// traversals where you don't need to store all edges.
    ///
    /// # Performance
    ///
    /// - No `Vec` allocation (saves 100-500ns per call)
    /// - Lazy evaluation - only computes what you consume
    /// - Can be short-circuited with `.take()`, `.find()`, etc.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get first outgoing edge without allocating a Vec
    /// let first_edge = storage.get_outgoing_edges_iter(node).next();
    ///
    /// // Count edges without allocation
    /// let count = storage.get_outgoing_edges_iter(node).count();
    /// ```
    #[inline]
    pub fn get_outgoing_edges_iter(&self, source: NodeId) -> OutgoingEdgesIter<'_> {
        OutgoingEdgesIter::new(self.indexes.get_outgoing(source))
    }

    /// Get all incoming edges to a node as an iterator.
    ///
    /// This is a zero-allocation alternative to [`Self::get_incoming_edges`] that returns
    /// an iterator instead of collecting into a `Vec`.
    ///
    /// See [`Self::get_outgoing_edges_iter`] for performance details and usage examples.
    #[inline]
    pub fn get_incoming_edges_iter(&self, target: NodeId) -> IncomingEdgesIter<'_> {
        IncomingEdgesIter::new(self.indexes.get_incoming(target))
    }

    /// Get outgoing edges with a specific label as an iterator.
    ///
    /// This is a zero-allocation alternative to [`Self::get_outgoing_edges_with_label`].
    ///
    /// Returns an empty iterator if the label doesn't exist.
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
    ///
    /// This is a zero-allocation alternative to [`Self::get_incoming_edges_with_label`].
    ///
    /// Returns an empty iterator if the label doesn't exist.
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
    ///
    /// This bypasses the need to rebuild adjacency structures from scratch.
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

    /// Get the default property name for vector indexing.
    ///
    /// This is used to maintain backward compatibility for legacy APIs that
    /// don't specify a property name. It deterministically selects the
    /// alphabetically first property name from the enabled vector indexes.
    ///
    /// Returns `None` if no vector indexes are enabled.
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

    /// Get or create filter statistics for a label (Issue #334).
    ///
    /// Returns an Arc to the FilterStats for the given label, creating it if needed.
    /// This enables adaptive over-fetch multiplier calculation based on historical
    /// filter pass rates.
    fn get_or_create_filter_stats(&self, label: &str) -> Arc<FilterStats> {
        self.filter_stats
            .entry(label.to_string())
            .or_insert_with(|| Arc::new(FilterStats::new()))
            .value()
            .clone()
    }

    /// Calculate adaptive over-fetch candidates for filtered search (Issue #334).
    ///
    /// Returns the number of candidates to fetch and the FilterStats for recording results.
    /// This method centralizes the adaptive over-fetch logic used by all filtered search methods.
    ///
    /// # Arguments
    ///
    /// * `k` - Number of results requested by the user
    /// * `label` - Label to filter by
    ///
    /// # Returns
    ///
    /// Tuple of (candidates_to_fetch, stats) where:
    /// - `candidates_to_fetch` is the adaptive number of candidates to retrieve from HNSW
    /// - `stats` is the FilterStats for recording search results
    ///
    /// # Algorithm
    ///
    /// - Uses adaptive multiplier based on historical pass rate (5x to 50x)
    /// - Guarantees minimum of k + 20 candidates
    /// - Caps maximum at k + 1000 to prevent excessive memory usage
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
    ///
    /// Returns the current statistics (search_count, total_candidates, total_results)
    /// for the given label, or None if no searches have been performed yet.
    ///
    /// This is used for testing to verify that adaptive learning is working correctly.
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
        let (prop_name, index, _) = self.get_vector_index_internal(None)?;
        let query_vector = self.get_node_vector(query_node_id, &prop_name)?;
        self.run_vector_search(&index, &query_vector, k, None, Some(query_node_id))
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
        let (prop_name, index, _) = self.get_vector_index_internal(None)?;
        let query_vector = self.get_node_vector(query_node_id, &prop_name)?;
        self.run_vector_search(&index, &query_vector, k, Some(label), Some(query_node_id))
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
        let (_, index, config) = self.get_vector_index_internal(None)?;

        if embedding.len() != config.dimensions {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: config.dimensions,
                    actual: embedding.len(),
                },
            ));
        }

        self.run_vector_search(&index, embedding, k, None, None)
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
        let (_, index, config) = self.get_vector_index_internal(None)?;

        if embedding.len() != config.dimensions {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: config.dimensions,
                    actual: embedding.len(),
                },
            ));
        }

        self.run_vector_search(&index, embedding, k, Some(label), None)
    }

    /// Find k most similar nodes to a raw embedding in a specific property's index.
    ///
    /// This is the property-specific version of `find_similar_by_embedding()`.
    /// Use this when you have multiple vector indexes and need to search a specific one.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property with the vector index to search
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
    /// - No vector index is enabled for the specified property
    /// - Embedding dimensions don't match the indexed property
    pub fn find_similar_by_embedding_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let (_, index, config) = self.get_vector_index_internal(Some(property_name))?;

        if embedding.len() != config.dimensions {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: config.dimensions,
                    actual: embedding.len(),
                },
            ));
        }

        self.run_vector_search(&index, embedding, k, None, None)
    }

    /// Find k most similar nodes with a label to a raw embedding in a specific property's index.
    ///
    /// This is the property-specific version of `find_similar_by_embedding_with_label()`.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property with the vector index to search
    /// * `embedding` - The query embedding vector
    /// * `label` - Only return nodes with this label
    /// * `k` - Maximum number of results to return
    pub fn find_similar_by_embedding_in_with_label(
        &self,
        property_name: &str,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let (_, index, config) = self.get_vector_index_internal(Some(property_name))?;

        if embedding.len() != config.dimensions {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: config.dimensions,
                    actual: embedding.len(),
                },
            ));
        }

        self.run_vector_search(&index, embedding, k, Some(label), None)
    }

    // ========================================================================
    // Multi-Property Vector Search Methods (Issue #389)
    // ========================================================================

    /// Find k most similar nodes in a specific property's vector index.
    ///
    /// Use this method when you have multiple vector indexes and need to search
    /// a specific one. The property must have a vector index enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The indexed property to search
    /// * `query_node_id` - The node to find similar nodes for
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    /// The query node itself is excluded from results.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Query node is not found
    /// - Query node does not have the specified property
    /// - The property value is not a vector
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search title embeddings for similar nodes
    /// let similar = storage.find_similar_in("title_embedding", node_id, 10)?;
    ///
    /// // Search body embeddings (different property, potentially different results)
    /// let similar_body = storage.find_similar_in("body_embedding", node_id, 10)?;
    /// ```
    pub fn find_similar_in(
        &self,
        property_name: &str,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let (_, index, _) = self.get_vector_index_internal(Some(property_name))?;
        let query_vector = self.get_node_vector(query_node_id, property_name)?;
        self.run_vector_search(&index, &query_vector, k, None, Some(query_node_id))
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
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Embedding dimensions don't match the index configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search with external embedding
    /// let query = embed_text("search query");
    /// let results = storage.search_vectors_in("title_embedding", &query, 10)?;
    /// ```
    pub fn search_vectors_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.find_similar_by_embedding_in(property_name, embedding, k)
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
    ///
    /// Returns a vector of property names that have temporal vector indexing configured.
    pub fn list_temporal_vector_indexes(&self) -> Vec<String> {
        self.temporal_vector_indexes
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get a reference to the temporal vector index for a specific property.
    ///
    /// Returns `None` if temporal vector indexing is not enabled for that property.
    pub(crate) fn get_temporal_vector_index_for(
        &self,
        property_name: &str,
    ) -> Option<Arc<TemporalVectorIndex>> {
        self.temporal_vector_indexes
            .get(property_name)
            .map(|entry| Arc::clone(&entry.index))
    }

    /// Get a reference to the temporal vector index if enabled (legacy single-property API).
    ///
    /// Returns `None` if temporal vector indexing is not enabled.
    /// For multi-property support, use `get_temporal_vector_index_for` instead.
    pub(crate) fn get_temporal_vector_index(&self) -> Option<Arc<TemporalVectorIndex>> {
        let state = self.temporal_vector_index_state.read();
        state.index.clone()
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

    /// Find k most similar nodes at a specific point in time for a specific property.
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
    /// * `timestamp` - The point in time to query
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
    /// // Find similar documents as they existed in the past for a specific property
    /// let query_embedding = vec![0.1; 384];
    /// let timestamp = 1234567890000000;
    /// let results = storage.find_similar_as_of_in("content_embedding", &query_embedding, 10, timestamp)?;
    /// ```
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
    ///
    /// Returns the actual embedding vectors at each snapshot timestamp.
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
    ///
    /// Scans all nodes and identifies those whose embeddings have changed
    /// by more than the specified threshold over the time range.
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
        if let Some(index) = &state.index
            && let Some(prop_name) = &state.property_name
            && let Some(vector) = properties.get(prop_name).and_then(|v| v.as_vector())
        {
            index.add(node_id, vector, timestamp)?;
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

    // ========================================================================
    // Query Executor Support Methods (VS-060)
    // ========================================================================

    /// Get all node IDs in the current storage.
    ///
    /// This is used by the query executor for full node scans.
    /// For large graphs, prefer using label-filtered scans instead.
    pub fn get_all_node_ids(&self) -> Vec<NodeId> {
        self.indexes.iter_node_ids().collect()
    }

    /// Get all edge IDs in the current storage.
    ///
    /// This is used by recovery tests and query executor for full edge scans.
    /// For large graphs, prefer using filtered scans instead.
    pub fn get_all_edge_ids(&self) -> Vec<EdgeId> {
        self.indexes.iter_edge_ids().collect()
    }

    /// Get all nodes in the current storage.
    ///
    /// Used by recovery property tests to verify invariants.
    pub fn get_all_nodes(&self) -> Vec<crate::Node> {
        self.indexes.iter_nodes().map(|n| n.clone()).collect()
    }

    /// Get all edges in the current storage.
    ///
    /// Used by recovery property tests to verify invariants.
    pub fn get_all_edges(&self) -> Vec<crate::Edge> {
        self.indexes.iter_edges().map(|e| e.clone()).collect()
    }

    /// Get nodes by label.
    ///
    /// Returns an iterator over all nodes with the given label.
    /// This is more efficient than scanning all nodes and filtering.
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
    ///
    /// Returns `None` if vector indexing is not enabled.
    /// This is used by the query executor for vector reranking operations.
    pub fn get_vector_property_name(&self) -> Option<String> {
        self.get_default_vector_property_name()
    }

    /// Get node counts grouped by label.
    ///
    /// Returns an iterator of (interned_label, count) pairs.
    /// Used by the query planner for cost estimation and cardinality estimation.
    pub fn label_counts(&self) -> Vec<(crate::core::interning::InternedString, usize)> {
        use std::collections::HashMap;
        let mut counts: HashMap<crate::core::interning::InternedString, usize> = HashMap::new();
        for node in self.indexes.iter_nodes() {
            *counts.entry(node.label).or_insert(0) += 1;
        }
        counts.into_iter().collect()
    }

    /// Get average out-degree across all nodes.
    ///
    /// Used by the query planner for cost estimation.
    pub fn avg_out_degree(&self) -> f64 {
        let node_count = self.node_count();
        if node_count == 0 {
            return 0.0;
        }
        self.edge_count() as f64 / node_count as f64
    }

    /// Get target node IDs from outgoing edges (used for traversal iterators).
    ///
    /// Returns the target node IDs of all outgoing edges from the source node.
    pub fn get_outgoing_targets(&self, source: NodeId) -> Vec<NodeId> {
        let guard = self.indexes.get_outgoing(source);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(guard.iter().map(|entry| entry.target));
        result
    }

    /// Get target node IDs from outgoing edges with a specific label.
    pub fn get_outgoing_targets_with_label(&self, source: NodeId, label: &str) -> Vec<NodeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };
        // Optimized to avoid intermediate Vec allocation and pre-allocate result
        let guard = self.indexes.get_outgoing(source);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(
            guard
                .iter()
                .filter(|entry| entry.label == label_id)
                .map(|entry| entry.target),
        );
        result
    }

    /// Get source node IDs from incoming edges (used for traversal iterators).
    ///
    /// Returns the source node IDs of all incoming edges to the target node.
    /// Note: For incoming edges, the "target" field in AdjacencyEntry represents
    /// the source node (the node the edge is coming from).
    pub fn get_incoming_sources(&self, target: NodeId) -> Vec<NodeId> {
        let guard = self.indexes.get_incoming(target);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(guard.iter().map(|entry| entry.target));
        result
    }

    /// Get source node IDs from incoming edges with a specific label.
    pub fn get_incoming_sources_with_label(&self, target: NodeId, label: &str) -> Vec<NodeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };
        // Optimized to avoid intermediate Vec allocation and pre-allocate result
        let guard = self.indexes.get_incoming(target);
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(
            guard
                .iter()
                .filter(|entry| entry.label == label_id)
                .map(|entry| entry.target),
        );
        result
    }

    /// Get the number of vectors in the HNSW index.
    ///
    /// Used by the query planner for statistics.
    pub fn vector_count(&self) -> usize {
        self.get_default_vector_property_name()
            .and_then(|prop_name| self.vector_indexes.get(&prop_name))
            .map(|entry| entry.value().index.len())
            .unwrap_or(0)
    }

    /// Iterate over all nodes (for persistence).
    ///
    /// This is a helper method for index persistence that provides
    /// access to all nodes in the current storage.
    ///
    /// Returns an iterator to avoid allocating a Vec for large graphs,
    /// improving memory efficiency during persistence operations.
    pub(crate) fn all_nodes(&self) -> impl Iterator<Item = Node> + '_ {
        self.indexes.iter_nodes().map(|n| n.clone())
    }

    /// Iterate over all edges (for persistence).
    ///
    /// This is a helper method for index persistence that provides
    /// access to all edges in the current storage.
    ///
    /// Returns an iterator to avoid allocating a Vec for large graphs,
    /// improving memory efficiency during persistence operations.
    pub(crate) fn all_edges(&self) -> impl Iterator<Item = Edge> + '_ {
        self.indexes.iter_edges().map(|e| e.clone())
    }

    /// Create an MVCC snapshot at the specified LSN.
    ///
    /// This provides snapshot isolation for checkpoint operations, preventing
    /// fuzzy checkpointing (mixed state from different LSNs) that can lead to
    /// data corruption.
    ///
    /// # Snapshot Isolation
    ///
    /// The snapshot captures Arc references to all nodes and edges at the time
    /// of snapshot creation. Concurrent modifications after snapshot creation
    /// do NOT affect the snapshot's iteration.
    ///
    /// # Memory Overhead
    ///
    /// - Does ONE iteration over DashMap to collect Arc references
    /// - Memory: ~8 bytes per entity (just Arc pointers, not full clones)
    /// - For 10M nodes: ~80MB overhead
    ///
    /// # Performance
    ///
    /// - Snapshot creation: ~100ms for 10M nodes (DashMap iteration)
    /// - Concurrent writes: Unaffected, continue during snapshot iteration
    ///
    /// # Arguments
    ///
    /// * `lsn` - LSN at which snapshot is taken (for tracking)
    ///
    /// # Returns
    ///
    /// A snapshot that provides isolated iteration over nodes and edges.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let snapshot = current.create_snapshot(LSN(100));
    ///
    /// // Concurrent writes after this point don't affect snapshot
    /// for node in snapshot.iter_nodes() {
    ///     // Stream to disk without loading entire DB
    ///     persist_node(&node)?;
    /// }
    /// ```
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

    /// Internal helper to get vector index and configuration.
    ///
    /// If `property_name` is Some, looks up that specific property.
    /// If None, looks up the default property.
    ///
    /// Returns (property_name, index, config).
    fn get_node_vector(&self, node_id: NodeId, prop_name: &str) -> Result<Arc<[f32]>> {
        let node = self
            .indexes
            .get_node(node_id)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        node.properties
            .get(prop_name)
            .ok_or_else(|| StorageError::PropertyNotFound(prop_name.to_string()))?
            .as_arc_vector()
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::InvalidVector {
                        reason: "Property is not a vector".to_string(),
                    },
                )
            })
    }

    fn get_vector_index_internal(
        &self,
        property_name: Option<&str>,
    ) -> Result<(String, Arc<HnswIndex>, HnswConfig)> {
        let prop_name = if let Some(name) = property_name {
            name.to_string()
        } else {
            self.get_default_vector_property_name().ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    "Vector index is not enabled. Call enable_vector_index() first.".to_string(),
                ))
            })?
        };

        let entry = self.vector_indexes.get(&prop_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!("Vector index not found for property '{}'", prop_name),
            ))
        })?;

        Ok((
            prop_name,
            entry.value().index.clone(),
            entry.value().config.clone(),
        ))
    }

    /// Internal helper to execute vector search with common logic.
    ///
    /// Handles:
    /// - Adaptive over-fetch candidate calculation
    /// - Filtering by label
    /// - Excluding specific nodes
    /// - Recording statistics
    fn run_vector_search(
        &self,
        index: &Arc<HnswIndex>,
        query: &[f32],
        k: usize,
        label_filter: Option<&str>,
        exclude_node: Option<NodeId>,
    ) -> Result<Vec<(NodeId, f32)>> {
        let mut results = if let Some(label) = label_filter {
            let label_id = GLOBAL_INTERNER.intern(label)?;
            let (candidates, stats) = self.calculate_adaptive_candidates(k, label);

            let mut results = index.search_with_filter(query, candidates, |node_id| {
                self.indexes
                    .get_node(*node_id)
                    .map(|n| n.label == label_id)
                    .unwrap_or(false)
            })?;

            if let Some(exclude_id) = exclude_node {
                results.retain(|(id, _)| *id != exclude_id);
            }

            stats.record_search(candidates, results.len());
            results
        } else {
            // If we need to exclude a node, we might need one more result
            let search_k = if exclude_node.is_some() { k + 1 } else { k };
            let mut results = index.search(query, search_k)?;

            if let Some(exclude_id) = exclude_node {
                results.retain(|(id, _)| *id != exclude_id);
            }
            results
        };

        results.truncate(k);
        Ok(results)
    }
}

impl Default for CurrentStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
