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
                NodeId::new(9999),
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
}
