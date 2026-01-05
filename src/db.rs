//! Main GallifreyDB database API.
//!
//! This module provides the primary interface to the database, coordinating
//! between current storage (fast path) and historical storage (temporal path).

use crate::api::transaction::{
    ReadTransaction, TxIdGenerator, TxVisibilityManager, WriteOps, WriteTransaction,
};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId};
use crate::core::property::PropertyMap;
use crate::core::temporal::{Timestamp, time};
use crate::index::temporal::TemporalIndexes;
use crate::index::vector::hnsw::HnswConfig;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::version::AnchorConfig;
use crate::storage::wal::{WalConfig, WriteAheadLog};
use crate::utils::error::{Result, StorageError};
use crate::utils::lock::MutexExt;
use std::sync::{Arc, Mutex};

/// Main GallifreyDB database.
///
/// This is the primary entry point for interacting with the database.
/// It coordinates between current storage (for fast current-state queries)
/// and historical storage (for temporal queries).
pub struct GallifreyDB {
    /// Current state storage (hot path) - Arc-wrapped for sharing across transactions
    current: Arc<CurrentStorage>,
    /// Historical version storage (temporal path) - Mutex-protected for write safety
    historical: Arc<Mutex<HistoricalStorage>>,
    /// Temporal indexes for efficient time-based queries - Mutex-protected for write safety
    temporal_indexes: Arc<Mutex<TemporalIndexes>>,
    /// Write-Ahead Log for durability - Mutex-protected for write safety
    wal: Arc<Mutex<WriteAheadLog>>,
    /// Current logical timestamp for transaction time - Mutex-protected for thread-safe increment
    current_timestamp: Arc<Mutex<Timestamp>>,
    /// Transaction ID generator for MVCC
    tx_id_gen: Arc<TxIdGenerator>,
    /// Transaction visibility manager for Snapshot Isolation
    visibility_manager: Arc<TxVisibilityManager>,
    /// ID generators for nodes, edges, and versions (shared with transactions)
    node_id_gen: Arc<Mutex<IdGenerator>>,
    edge_id_gen: Arc<Mutex<IdGenerator>>,
    version_id_gen: Arc<Mutex<IdGenerator>>,
}

impl GallifreyDB {
    /// Create a new empty database with default configuration.
    pub fn new() -> Self {
        Self::with_config(AnchorConfig::default())
    }

    /// Create a new database with custom anchor configuration.
    pub fn with_config(config: AnchorConfig) -> Self {
        // Create WAL with default config (can be made configurable later)
        let wal = WriteAheadLog::new(WalConfig::default()).expect("Failed to create WAL");

        GallifreyDB {
            current: Arc::new(CurrentStorage::new()),
            historical: Arc::new(Mutex::new(HistoricalStorage::with_config(config))),
            temporal_indexes: Arc::new(Mutex::new(TemporalIndexes::new())),
            wal: Arc::new(Mutex::new(wal)),
            current_timestamp: Arc::new(Mutex::new(time::now())),
            tx_id_gen: Arc::new(TxIdGenerator::new()),
            visibility_manager: Arc::new(TxVisibilityManager::new()),
            node_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            edge_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            version_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
        }
    }

    /// Open an existing database from a checkpoint.
    ///
    /// This method loads the most recent checkpoint and restores the database state,
    /// including vector index configuration if it was enabled.
    ///
    /// # Arguments
    ///
    /// * `checkpoint_path` - Path to the checkpoint file to load
    ///
    /// # Returns
    ///
    /// Returns a `GallifreyDB` instance with restored configuration, or an error
    /// if the checkpoint cannot be loaded or the vector index cannot be restored.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::GallifreyDB;
    /// use std::path::Path;
    ///
    /// let db = GallifreyDB::open(Path::new("gallifreydb/checkpoints/latest.gfry"))?;
    /// ```
    pub fn open<P: AsRef<std::path::Path>>(checkpoint_path: P) -> Result<Self> {
        use crate::storage::persistence::Checkpoint;

        // Load checkpoint
        let checkpoint = Checkpoint::load(checkpoint_path.as_ref())?;

        // Create new database with default config
        let db = Self::new();

        // Restore vector index if it was enabled
        if let Some(ref vector_config) = checkpoint.metadata.vector_index_config
            && vector_config.enabled
        {
            db.current
                .enable_vector_index(&vector_config.property_name, vector_config.config.clone())?;
        }

        Ok(db)
    }

    /// Create a new read-only transaction.
    ///
    /// Read-only transactions are lightweight and have zero overhead:
    /// - No write buffer
    /// - No WAL logging
    /// - Snapshot-based reads for consistency
    /// - No commit overhead
    ///
    /// # Errors
    ///
    /// Returns an error if the timestamp lock is poisoned.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let tx = db.read_transaction()?;
    /// let node = tx.get_node(node_id)?;
    /// // No commit needed - transaction is read-only
    /// ```
    pub fn read_transaction(&self) -> Result<ReadTransaction> {
        let tx_id = self.tx_id_gen.next();
        let snapshot_timestamp = *self.current_timestamp.lock_or_err()?;

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        Ok(ReadTransaction::new(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.visibility_manager),
        ))
    }

    /// Execute a read-only operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically cleaned up after the closure completes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let name = db.read(|tx| {
    ///     let node = tx.get_node(node_id)?;
    ///     Ok(node.get_property("name").cloned())
    /// })?;
    /// ```
    pub fn read<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&ReadTransaction) -> Result<T>,
    {
        let tx = self.read_transaction()?;
        f(&tx)
    }

    /// Create a new write transaction.
    ///
    /// Write transactions provide full ACID guarantees:
    /// - **Atomicity**: All-or-nothing commit via write buffering
    /// - **Consistency**: Referential integrity validation before commit
    /// - **Isolation**: Snapshot Isolation with write-write conflict detection
    /// - **Durability**: WAL with fsync for true durability
    ///
    /// # Errors
    ///
    /// Returns an error if the timestamp lock is poisoned.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node("Person", props)?;
    /// tx.create_edge(node_id, other, "KNOWS", edge_props)?;
    /// tx.commit()?;  // or tx.rollback()
    /// ```
    pub fn write_transaction(&self) -> Result<WriteTransaction> {
        let tx_id = self.tx_id_gen.next();
        let snapshot_timestamp = *self.current_timestamp.lock_or_err()?;

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        Ok(WriteTransaction::new(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.historical),
            Arc::clone(&self.temporal_indexes),
            Arc::clone(&self.wal),
            Arc::clone(&self.current_timestamp),
            Arc::clone(&self.visibility_manager),
            Arc::clone(&self.node_id_gen),
            Arc::clone(&self.edge_id_gen),
            Arc::clone(&self.version_id_gen),
        ))
    }

    /// Execute a write operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically committed on Ok, or rolled back on Err.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let node_id = db.write(|tx| {
    ///     let id = tx.create_node("Person", props)?;
    ///     tx.create_edge(id, other, "KNOWS", edge_props)?;
    ///     Ok(id)
    /// })?;
    /// ```
    pub fn write<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut WriteTransaction) -> Result<T>,
    {
        let mut tx = self.write_transaction()?;
        let result = f(&mut tx)?;
        tx.commit()?;
        Ok(result)
    }

    /// Create a node with the given label and properties.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        self.write(|tx| tx.create_node(label, properties))
    }

    /// Create an edge between two nodes.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    pub fn create_edge(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        self.write(|tx| tx.create_edge(source, target, label, properties))
    }

    /// Get the current state of a node.
    ///
    /// This uses the fast path (current storage) for O(1) lookup.
    pub fn get_node(&self, node_id: NodeId) -> Result<Node> {
        self.current.get_node(node_id)
    }

    /// Get the current state of an edge.
    pub fn get_edge(&self, edge_id: EdgeId) -> Result<Edge> {
        self.current.get_edge(edge_id)
    }

    /// Get outgoing edges from a node (current state).
    pub fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_outgoing_edges(node_id)
    }

    /// Get incoming edges to a node (current state).
    pub fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_incoming_edges(node_id)
    }

    /// Get outgoing edges with a specific label (current state).
    pub fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    /// Get a node as it existed at a specific point in bi-temporal space.
    ///
    /// This uses the slow path (historical storage + version reconstruction).
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        let historical = self.historical.lock_or_err()?;

        // Find the version valid at this time
        let version_id = historical
            .find_node_version_at_time(node_id, valid_time, transaction_time)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Get the version
        let version = historical
            .get_node_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Reconstruct properties
        let properties = historical.reconstruct_node_properties(version_id)?;

        // Build node from version
        Ok(Node::new(
            version.node_id,
            version.label,
            properties,
            version.id,
        ))
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        let historical = self.historical.lock_or_err()?;

        let version_id = historical
            .find_edge_version_at_time(edge_id, valid_time, transaction_time)
            .ok_or(StorageError::EdgeNotFound(edge_id))?;

        let version = historical
            .get_edge_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = historical.reconstruct_edge_properties(version_id)?;

        Ok(Edge::new(
            version.edge_id,
            version.label,
            version.source,
            version.target,
            properties,
            version.id,
        ))
    }

    // ========================================================================
    // Vector Indexing API (VS-030)
    // ========================================================================

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
    /// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// db.enable_vector_index("embedding", config)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if vector indexing is already enabled.
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()> {
        self.current.enable_vector_index(property_name, config)
    }

    /// Check if vector indexing is enabled.
    pub fn is_vector_index_enabled(&self) -> bool {
        self.current.is_vector_index_enabled()
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
        self.current
            .find_similar_by_embedding_with_label(embedding, label, k)
    }

    // ========================================================================
    // Temporal Vector Search (Phase 3)
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
    /// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
    /// db.enable_temporal_vector_index("embedding", temporal_config)?;
    /// ```
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: crate::index::vector::temporal::TemporalVectorConfig,
    ) -> Result<()> {
        self.current
            .enable_temporal_vector_index(property_name, config)
    }

    /// Check if temporal vector indexing is enabled.
    pub fn is_temporal_vector_index_enabled(&self) -> bool {
        self.current.is_temporal_vector_index_enabled()
    }

    /// Find k most similar nodes at a specific point in time.
    ///
    /// Returns nodes similar to the query embedding as they existed at the given timestamp.
    /// This enables "semantic time travel" - understanding what was semantically similar
    /// at different points in the database's history.
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
    /// let results = db.find_similar_as_of(&query_embedding, 10, timestamp)?;
    /// for (node_id, similarity) in results {
    ///     println!("Historical similarity: {:?} -> {}", node_id, similarity);
    /// }
    /// ```
    pub fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.current.find_similar_as_of(embedding, k, timestamp)
    }

    /// Find k most similar nodes across a time range.
    ///
    /// Returns results for each snapshot within the time range, showing how
    /// semantic similarity evolved over time. This is useful for semantic drift
    /// tracking and understanding how the meaning of concepts changed.
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
    /// let results = db.find_similar_in_range(&query, 10, time_range)?;
    /// for (timestamp, similar_nodes) in results {
    ///     println!("At {}: found {} similar nodes", timestamp, similar_nodes.len());
    /// }
    /// ```
    pub fn find_similar_in_range(
        &self,
        embedding: &[f32],
        k: usize,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(Timestamp, Vec<(NodeId, f32)>)>> {
        self.current.find_similar_in_range(embedding, k, time_range)
    }

    /// Get the number of nodes in the current state.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.current.node_count()
    }

    /// Get the number of edges in the current state.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.current.edge_count()
    }

    /// Get the out-degree of a node (current state).
    #[inline]
    pub fn out_degree(&self, node_id: NodeId) -> usize {
        self.current.out_degree(node_id)
    }

    /// Get the in-degree of a node (current state).
    #[inline]
    pub fn in_degree(&self, node_id: NodeId) -> usize {
        self.current.in_degree(node_id)
    }

    /// Get statistics about the historical storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the historical storage lock is poisoned.
    pub fn historical_stats(&self) -> Result<crate::storage::historical::HistoricalStats> {
        Ok(self.historical.lock_or_err()?.stats())
    }
}

impl Default for GallifreyDB {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::ReadOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_node() {
        let db = GallifreyDB::new();

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = db.create_node("Person", props).unwrap();

        assert_eq!(db.node_count(), 1);

        let node = db.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_create_edge() {
        let db = GallifreyDB::new();

        let alice = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = db
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        assert_eq!(db.edge_count(), 1);

        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, alice);
        assert_eq!(edge.target, bob);
    }

    #[test]
    fn test_time_travel_query() {
        let db = GallifreyDB::new();

        // Create a node at time T1
        let props_v1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = db.create_node("Person", props_v1).unwrap();
        let t1 = *db.current_timestamp.lock().unwrap() - 1; // Timestamp when created

        // In a real implementation, we'd create a second version here with an update_node method
        // For now, just verify we can query at T1

        // Query at time T1
        let historical_node = db.get_node_at_time(node_id, t1, t1).unwrap();
        assert_eq!(
            historical_node.get_property("age").and_then(|v| v.as_int()),
            Some(30)
        );

        // Query current state
        let current_node = db.get_node(node_id).unwrap();
        assert_eq!(
            current_node.get_property("age").and_then(|v| v.as_int()),
            Some(30)
        );
    }

    #[test]
    fn test_time_travel_after_deletion() {
        let db = GallifreyDB::new();

        // Create a node
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = db.create_node("Person", props).unwrap();

        // Record timestamp after creation
        let t_after_create = *db.current_timestamp.lock().unwrap();

        // Delete the node
        db.write(|tx| {
            tx.delete_node(node_id)?;
            Ok(())
        })
        .unwrap();

        // Record timestamp after deletion
        let t_after_delete = *db.current_timestamp.lock().unwrap();

        // Query BEFORE creation - should fail (node didn't exist)
        // Note: We can't easily test this without more control over timestamps

        // Query AFTER deletion - should fail (node was deleted)
        // This is the critical test: time-travel query after deletion should NOT
        // return the deleted node's data
        let result = db.get_node_at_time(node_id, t_after_delete, t_after_delete);
        assert!(
            result.is_err(),
            "Expected NodeNotFound after deletion, but got: {:?}",
            result
        );

        // Query BEFORE deletion - should succeed (node existed)
        let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
        assert!(
            result.is_ok(),
            "Expected to find node before deletion, but got: {:?}",
            result
        );
        let node = result.unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_graph_traversal() {
        let db = GallifreyDB::new();

        let n0 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        db.create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let outgoing = db.get_outgoing_edges(n0);
        assert_eq!(outgoing.len(), 2);

        let knows_edges = db.get_outgoing_edges_with_label(n0, "KNOWS");
        assert_eq!(knows_edges.len(), 2);
    }

    #[test]
    fn test_historical_stats() {
        let db = GallifreyDB::new();

        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let stats = db.historical_stats().unwrap();
        assert_eq!(stats.total_node_versions, 2);
        assert_eq!(stats.node_anchor_count, 2); // First versions are always anchors
    }

    // ==================== Transaction API Tests ====================

    #[test]
    fn test_closure_based_write_api() {
        let db = GallifreyDB::new();

        // Use closure-based API for multiple operations
        let (node_id, edge_id) = db
            .write(|tx| {
                let n1 = tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )?;
                let n2 = tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )?;
                let e = tx.create_edge(
                    n1,
                    n2,
                    "KNOWS",
                    PropertyMapBuilder::new().insert("since", 2024i64).build(),
                )?;
                Ok((n1, e))
            })
            .unwrap();

        // Verify changes are visible
        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);

        let node = db.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );

        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, node_id);
    }

    #[test]
    fn test_closure_based_read_api() {
        let db = GallifreyDB::new();

        let node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Charlie").build(),
            )
            .unwrap();

        // Use closure-based read API
        let name = db
            .read(|tx| {
                let node = tx.get_node(node_id)?;
                Ok(node
                    .get_property("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()))
            })
            .unwrap();

        assert_eq!(name, Some("Charlie".to_string()));
    }

    #[test]
    fn test_explicit_write_transaction() {
        let db = GallifreyDB::new();

        let mut tx = db.write_transaction().unwrap();
        let n1 = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "David").build(),
            )
            .unwrap();
        let n2 = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Eve").build(),
            )
            .unwrap();
        tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Changes not visible before commit
        assert_eq!(db.node_count(), 0);

        // Commit
        tx.commit().unwrap();

        // Now visible
        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);
    }

    #[test]
    fn test_explicit_read_transaction() {
        let db = GallifreyDB::new();

        let node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("age", 42i64).build(),
            )
            .unwrap();

        let tx = db.read_transaction().unwrap();
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(42));

        // Read transactions don't need commit
    }

    #[test]
    fn test_transaction_atomicity() {
        let db = GallifreyDB::new();

        // Create a valid node first
        let valid_node = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Try to create multiple operations, one of which will fail
        let result = db.write(|tx| {
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            // This should fail validation (non-existent target)
            tx.create_edge(
                valid_node,
                NodeId::new(9999).unwrap(),
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )?;
            Ok(())
        });

        // Transaction should fail
        assert!(result.is_err());

        // No partial changes should be visible (atomicity)
        // We started with 1 node, should still have 1 node
        assert_eq!(db.node_count(), 1);
        assert_eq!(db.edge_count(), 0);
    }

    #[test]
    fn test_transaction_rollback_on_error() {
        let db = GallifreyDB::new();

        // Closure returns an error - should auto-rollback
        let result: Result<()> = db.write(|tx| {
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            // Manually return an error
            Err(crate::utils::error::Error::Storage(
                crate::utils::error::StorageError::InconsistentState {
                    reason: "test error".to_string(),
                },
            ))
        });

        assert!(result.is_err());

        // All changes rolled back
        assert_eq!(db.node_count(), 0);
    }

    #[test]
    fn test_multiple_transactions() {
        let db = GallifreyDB::new();

        // Transaction 1
        let n1 = db
            .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
            .unwrap();

        // Transaction 2
        let n2 = db
            .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
            .unwrap();

        // Transaction 3
        db.write(|tx| tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build()))
            .unwrap();

        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);
    }

    #[test]
    fn test_snapshot_isolation() {
        let db = GallifreyDB::new();

        let node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("version", 1i64).build(),
            )
            .unwrap();

        // Start a read transaction - captures snapshot
        let tx1 = db.read_transaction().unwrap();
        let node_v1 = tx1.get_node(node_id).unwrap();
        assert_eq!(
            node_v1.get_property("version").and_then(|v| v.as_int()),
            Some(1)
        );

        // Another write commits a change (creates a new node)
        let new_node_id = db
            .write(|tx| {
                tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("version", 2i64).build(),
                )
            })
            .unwrap();

        // Snapshot Isolation: tx1 should NOT see the new node
        // because it was created and committed after tx1's snapshot
        assert!(tx1.get_node(new_node_id).is_err());

        // Verify tx1 still sees the original node
        let node_v1_again = tx1.get_node(node_id).unwrap();
        assert_eq!(
            node_v1_again
                .get_property("version")
                .and_then(|v| v.as_int()),
            Some(1)
        );
    }

    // ==================== Vector Index API Tests ====================

    #[test]
    fn test_enable_vector_index() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Trying to enable again should fail
        let config2 = HnswConfig::new(3, DistanceMetric::Cosine);
        assert!(db.enable_vector_index("embedding", config2).is_err());
    }

    #[test]
    fn test_find_similar_basic() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes with vector embeddings
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Programming")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Advanced")
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        let doc3 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Python Basics")
                    .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Find similar to doc1
        let similar = db.find_similar(doc1, 2).unwrap();

        // Should return 2 results (excluding doc1 itself)
        assert_eq!(similar.len(), 2);

        // doc2 should be most similar (both about Rust)
        assert_eq!(similar[0].0, doc2);
        assert!(similar[0].1 > 0.9); // High similarity

        // doc3 should be less similar
        assert_eq!(similar[1].0, doc3);
        assert!(similar[1].1 < 0.5); // Lower similarity
    }

    #[test]
    fn test_find_similar_with_label() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create Document nodes
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create Person nodes with similar embeddings
        let _person1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.95f32, 0.05, 0.0])
                    .build(),
            )
            .unwrap();

        // Find similar Documents only (should exclude Person nodes)
        let similar = db.find_similar_with_label(doc1, "Document", 5).unwrap();

        // Should only return doc2 (not person1)
        assert_eq!(similar.len(), 1);
        assert_eq!(similar[0].0, doc2);
    }

    #[test]
    fn test_vector_index_not_enabled() {
        let db = GallifreyDB::new();

        // Create node with vector
        let node_id = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Try to search without enabling index - should fail
        assert!(db.find_similar(node_id, 5).is_err());
    }

    #[test]
    fn test_vector_index_with_euclidean_distance() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index with Euclidean distance
        let config = HnswConfig::new(3, DistanceMetric::Euclidean).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes with vector embeddings
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc3 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[10.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Find similar to doc1
        let similar = db.find_similar(doc1, 2).unwrap();

        assert_eq!(similar.len(), 2);

        // With Euclidean distance, doc2 (distance 1.0) should be closer than doc3 (distance 10.0)
        assert_eq!(similar[0].0, doc2);
        assert_eq!(similar[1].0, doc3);
    }

    #[test]
    fn test_vector_index_with_large_k() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create 5 nodes
        let mut node_ids = Vec::new();
        for i in 0..5 {
            let node_id = db
                .create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[i as f32, 0.0, 0.0])
                        .build(),
                )
                .unwrap();
            node_ids.push(node_id);
        }

        // Request k=10 (more than available)
        let similar = db.find_similar(node_ids[0], 10).unwrap();

        // Should return at most 4 results (5 total - 1 query node)
        assert!(similar.len() <= 4);
    }

    /// Regression test for VS-030 bug: nodes created via write transactions
    /// must be indexed for vector search.
    ///
    /// Prior to fix: insert_node_direct() only called indexes.insert_node(),
    /// skipping try_index_vector(). This meant all transaction-created nodes
    /// were missing from the HNSW index, causing find_similar to return empty results.
    #[test]
    fn test_transaction_nodes_are_indexed() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes via write transaction (not convenience method)
        let (doc1, doc2, _doc3) = db
            .write(|tx| {
                let d1 = tx.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                        .build(),
                )?;
                let d2 = tx.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                        .build(),
                )?;
                let d3 = tx.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                        .build(),
                )?;
                Ok((d1, d2, d3))
            })
            .unwrap();

        // CRITICAL: These nodes were created via transaction, not db.create_node()
        // Before the fix, insert_node_direct() didn't index vectors, so this would fail
        let similar = db.find_similar(doc1, 2).unwrap();

        // Should find doc2 and doc3
        assert_eq!(similar.len(), 2);
        assert_eq!(similar[0].0, doc2); // Most similar
        assert!(similar[0].1 > 0.9); // High similarity
    }

    #[test]
    fn test_find_similar_by_embedding() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes with vector embeddings
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Programming")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Advanced")
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        let _doc3 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Python Basics")
                    .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Search with an external query embedding (similar to doc1)
        let query_embedding = [0.95f32, 0.05, 0.0];
        let similar = db.find_similar_by_embedding(&query_embedding, 2).unwrap();

        // Should return doc1 first (most similar to query), then doc2
        assert_eq!(similar.len(), 2);
        assert_eq!(similar[0].0, doc1); // Most similar
        assert!(similar[0].1 > 0.99); // Very high similarity
        assert_eq!(similar[1].0, doc2);
    }

    #[test]
    fn test_find_similar_by_embedding_with_label() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create Document nodes
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create Person nodes with similar embeddings
        let _person1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.95f32, 0.05, 0.0])
                    .build(),
            )
            .unwrap();

        // Search for Documents only with query embedding
        let query_embedding = [1.0f32, 0.0, 0.0];
        let similar = db
            .find_similar_by_embedding_with_label(&query_embedding, "Document", 5)
            .unwrap();

        // Should only return Documents (doc1 and doc2), not person1
        assert_eq!(similar.len(), 2);
        assert!(similar.iter().any(|(id, _)| *id == doc1));
        assert!(similar.iter().any(|(id, _)| *id == doc2));
    }

    #[test]
    fn test_find_similar_by_embedding_dimension_mismatch() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index with 3 dimensions
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create a node
        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

        // Try to search with wrong dimensions (4 instead of 3)
        let wrong_embedding = [1.0f32, 0.0, 0.0, 0.0];
        let result = db.find_similar_by_embedding(&wrong_embedding, 5);

        // Should fail with dimension mismatch error
        assert!(result.is_err());
    }

    #[test]
    fn test_find_similar_empty_database() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index but don't add any nodes
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Search should return empty results, not error
        let query_embedding = [1.0f32, 0.0, 0.0];
        let results = db.find_similar_by_embedding(&query_embedding, 10).unwrap();

        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_find_similar_k_zero() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index and add some nodes
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

        // Search with k=0 should return empty results, not error
        let query_embedding = [1.0f32, 0.0, 0.0];
        let results = db.find_similar_by_embedding(&query_embedding, 0).unwrap();

        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_concurrent_vector_indexing() {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use std::sync::Arc;
        use std::thread;

        let db = Arc::new(GallifreyDB::new());

        // Enable vector index
        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(1000);
        db.enable_vector_index("embedding", config).unwrap();

        // Spawn multiple threads that create nodes with vectors concurrently
        let mut handles = vec![];
        for i in 0..10 {
            let db_clone = Arc::clone(&db);
            let handle = thread::spawn(move || {
                // Use non-zero vectors to avoid issues with cosine similarity
                let base = (i as f32 + 1.0) / 10.0;
                db_clone
                    .create_node(
                        "Document",
                        PropertyMapBuilder::new()
                            .insert_vector("embedding", &[base, base, base, base])
                            .build(),
                    )
                    .unwrap()
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Verify all nodes were indexed - search should return results
        // (Note: find_similar excludes the query node, so we check for OTHER nodes)
        for node_id in &node_ids {
            let results = db.find_similar(*node_id, 5).unwrap();
            // With 10 nodes and k=5, we should get at least 4 results (excluding query node)
            // HNSW is approximate, so we allow for slight variation
            assert!(
                results.len() >= 4,
                "Expected >=4 results for node {:?}, got {}",
                node_id,
                results.len()
            );
            // Verify results don't include the query node (it's excluded by design)
            assert!(
                results.iter().all(|(id, _)| *id != *node_id),
                "Query node {:?} should not appear in its own results",
                node_id
            );
            // Verify similarity scores are reasonable (between 0 and 1)
            for (_, score) in &results {
                assert!(
                    (0.0..=1.0).contains(score),
                    "Similarity score {} out of range",
                    score
                );
            }
        }

        // Verify total count
        assert_eq!(db.node_count(), 10);
    }

    #[test]
    fn test_find_similar_with_missing_property() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new();

        // Enable vector index on "embedding" property
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create some nodes with the indexed property
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let _doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create a node WITHOUT the indexed property (should be ignored in searches)
        let _doc_no_vector = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "No embedding")
                    .build(),
            )
            .unwrap();

        // Search should only find nodes with the property
        let results = db.find_similar(doc1, 5).unwrap();

        // Should find doc2 but not doc_no_vector
        assert_eq!(results.len(), 1); // Only doc2 (doc1 is excluded as query node)
    }
}
