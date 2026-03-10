//! Transaction support for AletheiaDB
//!
//! This module provides MVCC (Multi-Version Concurrency Control) transactions
//! with Snapshot Isolation level.
//!
//! # Transaction Types
//!
//! - [`ReadTransaction`]: Lightweight read-only transactions with zero overhead
//! - [`WriteTransaction`]: Full ACID write transactions with write buffering and WAL
//!
//! # API Styles
//!
//! **Closure-based (recommended)**:
//! ```rust,no_run
//! # use aletheiadb::{AletheiaDB, PropertyMapBuilder, properties};
//! # use aletheiadb::core::NodeId;
//! # use aletheiadb::api::transaction::{ReadOps, WriteOps};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let db = AletheiaDB::new()?;
//! # let id = NodeId::new(1)?;
//! # let other = NodeId::new(2)?;
//! # let props = PropertyMapBuilder::new().build();
//! # let edge_props = PropertyMapBuilder::new().build();
//! // Read-only
//! let result = db.read(|tx| {
//!     // get_node might fail if node doesn't exist
//!     if let Ok(node) = tx.get_node(id) {
//!         Ok::<_, Box<dyn std::error::Error>>(node.get_property("name").cloned())
//!     } else {
//!         Ok::<_, Box<dyn std::error::Error>>(None)
//!     }
//! })?;
//!
//! // Read-write (auto-commit on Ok, auto-rollback on Err)
//! let node_id = db.write(|tx| {
//!     let node_id = tx.create_node("Person", props)?;
//!     tx.create_edge(node_id, other, "KNOWS", edge_props)?;
//!     Ok::<_, Box<dyn std::error::Error>>(node_id)
//! })?;
//! # Ok(())
//! # }
//! ```
//!
//! **Explicit handles**:
//! ```rust,no_run
//! # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
//! # use aletheiadb::core::NodeId;
//! # use aletheiadb::api::transaction::WriteOps;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let db = AletheiaDB::new()?;
//! # let n1 = NodeId::new(1)?;
//! # let n2 = NodeId::new(2)?;
//! # let props = PropertyMapBuilder::new().build();
//! let mut tx = db.write_transaction()?;
//! tx.create_node("Person", props.clone())?;
//! tx.create_edge(n1, n2, "KNOWS", props)?;
//! tx.commit()?;  // or tx.rollback()
//! # Ok(())
//! # }
//! ```

pub mod read_tx;
pub mod types;
pub mod visibility;
pub mod write;
pub mod write_buffer;

pub use read_tx::ReadTransaction;
pub use types::{TxId, TxMetadata, TxState};
pub use visibility::{CompressionStats, TransactionSnapshot, TxVisibilityManager};
pub use write::WriteTransaction;
pub use write_buffer::{BufferedWrite, WriteBuffer};

use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
#[cfg(test)]
use crate::core::id::MAX_VALID_ID;
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::Timestamp;

/// Common read operations available in all transaction types
pub trait ReadOps {
    /// Get a node by ID.
    ///
    /// This returns the node state visible in the current transaction snapshot.
    ///
    /// # Snapshot Isolation
    ///
    /// If the node has been modified or deleted by another transaction after this
    /// transaction started, `get_node` will return the version visible at the start
    /// of this transaction (Snapshot Isolation).
    ///
    /// # Performance
    ///
    /// - **Fast Path**: O(1) lookup in current storage (hash map)
    /// - **Slow Path**: O(log N) lookup in historical storage if not found in current (or version not visible)
    fn get_node(&self, id: NodeId) -> Result<Node>;

    /// Get an edge by ID.
    ///
    /// This returns the edge state visible in the current transaction snapshot.
    ///
    /// # Snapshot Isolation
    ///
    /// If the edge has been modified or deleted by another transaction after this
    /// transaction started, `get_edge` will return the version visible at the start
    /// of this transaction (Snapshot Isolation).
    fn get_edge(&self, id: EdgeId) -> Result<Edge>;

    /// Get multiple edges by ID.
    ///
    /// This provides a batch-fetching mechanism that can be significantly more
    /// efficient than calling `get_edge` in a loop, especially for historical storage.
    fn get_edges(&self, ids: &[EdgeId]) -> Result<Vec<Edge>> {
        let mut edges = Vec::with_capacity(ids.len());
        for &id in ids {
            if let Ok(edge) = self.get_edge(id) {
                edges.push(edge);
            }
        }
        Ok(edges)
    }

    /// Get outgoing edges from a node.
    ///
    /// Returns all edges where `source == node_id` that are visible in the current snapshot.
    ///
    /// # Ordering
    ///
    /// The order of edges is **not guaranteed**. Do not rely on edges being returned
    /// in insertion order or sorted by ID. The internal storage may reorder edges
    /// during compaction or persistence.
    ///
    /// # Snapshot Isolation
    ///
    /// This method filters edges to ensure only those visible in the current transaction
    /// snapshot are returned. Edges created by concurrent transactions will not be seen.
    ///
    /// # Performance
    ///
    /// - **Time**: O(degree) to collect visible edges
    /// - **Space**: Allocates a new `Vec` containing all edge IDs
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, core::NodeId, api::transaction::ReadOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let tx = db.read_transaction()?;
    /// # let node_id = NodeId::new(1)?;
    /// let edges = tx.get_outgoing_edges(node_id);
    /// for edge_id in edges {
    ///     let edge = tx.get_edge(edge_id)?;
    ///     println!("-> {}", edge.target);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId>;

    /// Get incoming edges to a node.
    ///
    /// Returns all edges where `target == node_id` that are visible in the current snapshot.
    ///
    /// # Ordering
    ///
    /// The order of edges is **not guaranteed**.
    ///
    /// # Snapshot Isolation
    ///
    /// This method filters edges to ensure only those visible in the current transaction
    /// snapshot are returned.
    ///
    /// # Performance
    ///
    /// - **Time**: O(degree) to collect visible edges
    /// - **Space**: Allocates a new `Vec` containing all edge IDs
    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId>;

    /// Get outgoing edges with a specific label.
    ///
    /// Returns all edges where `source == node_id` AND `label == label` that are
    /// visible in the current snapshot.
    ///
    /// # Ordering
    ///
    /// The order of edges is **not guaranteed**.
    ///
    /// # Performance
    ///
    /// - **Time**: O(degree) scan with label filtering
    /// - **Space**: Allocates a new `Vec` containing matching edge IDs
    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId>;

    /// Get the approximate number of nodes in the database.
    ///
    /// # Consistency Note
    ///
    /// This returns the **current** count of committed nodes in the storage engine.
    /// Unlike `get_node()`, this count is **NOT snapshot-isolated**. It may include
    /// nodes created by transactions that committed after this read transaction started.
    ///
    /// This design choice enables O(1) performance without scanning the entire
    /// database to filter visibility for every node.
    fn node_count(&self) -> usize;

    /// Get the approximate number of edges in the database.
    ///
    /// # Consistency Note
    ///
    /// This returns the **current** count of committed edges in the storage engine.
    /// Unlike `get_edge()`, this count is **NOT snapshot-isolated**. It may include
    /// edges created by transactions that committed after this read transaction started.
    fn edge_count(&self) -> usize;

    /// Find nodes by label and property value.
    ///
    /// Returns the IDs of all nodes with the given label whose specified property
    /// equals the given value. Only nodes visible in the current snapshot are returned.
    ///
    /// # Performance
    ///
    /// - **Time**: O(N) where N = nodes with the given label
    /// - **Space**: O(M) where M = number of matching nodes
    fn find_nodes_by_property(
        &self,
        label: &str,
        property_key: &str,
        property_value: &PropertyValue,
    ) -> Vec<NodeId>;
}

/// Write operations (only available in write transactions)
pub trait WriteOps: ReadOps {
    /// Create a new node with optional backdated valid_from time.
    ///
    /// # Arguments
    ///
    /// * `label` - Node label
    /// * `properties` - Node properties
    /// * `valid_from` - When the fact became valid (None = transaction time)
    ///
    /// # Bi-Temporal Semantics
    ///
    /// - If `valid_from` is None, `valid_time` starts at the **transaction start time**.
    /// - `transaction_time` always starts at the **commit time**.
    /// - This means by default, facts are considered valid from the moment the transaction began.
    /// - If `valid_from` is Some(ts), valid_time starts at `ts`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, api::transaction::WriteOps};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// // Backdate a fact to 1 hour ago
    /// let one_hour_ago = time::now().wallclock() - 3_600_000_000;
    /// let valid_from = time::from_secs(one_hour_ago / 1_000_000);
    ///
    /// let node_id = tx.create_node_with_valid_time(
    ///     "Person",
    ///     properties! { "name" => "Alice" },
    ///     Some(valid_from)
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn create_node_with_valid_time(
        &mut self,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<NodeId>;

    /// Create a new node.
    ///
    /// This is a convenience method that sets `valid_from` to the **transaction start time**.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, api::transaction::WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node(
    ///     "Person",
    ///     properties! { "name" => "Alice", "age" => 30 }
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        self.create_node_with_valid_time(label, properties, None)
    }

    /// Create a new edge with optional backdated valid_from time.
    ///
    /// Use this when loading historical data where relationships formed in the past.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, core::NodeId, api::transaction::WriteOps};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let alice = NodeId::new(1)?;
    /// # let bob = NodeId::new(2)?;
    /// let past_time = time::from_secs(1609459200); // 2021-01-01
    ///
    /// let edge_id = tx.create_edge_with_valid_time(
    ///     alice,
    ///     bob,
    ///     "KNOWS",
    ///     properties! { "since" => 2021 },
    ///     Some(past_time)
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn create_edge_with_valid_time(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<EdgeId>;

    /// Create a new edge.
    ///
    /// This is a convenience method that sets `valid_from` to the **transaction start time**.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, core::NodeId, api::transaction::WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let bob_id = NodeId::new(2)?;
    /// let edge_id = tx.create_edge(
    ///     alice_id,
    ///     bob_id,
    ///     "KNOWS",
    ///     properties! { "since" => 2024 }
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn create_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        self.create_edge_with_valid_time(source, target, label, properties, None)
    }

    /// Update a node's properties with optional backdated valid_from time.
    ///
    /// This merges the new properties with existing ones (PATCH semantics).
    /// Existing properties not in the map are preserved.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, core::NodeId, api::transaction::WriteOps};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let alice = NodeId::new(1)?;
    /// // Update address effective from yesterday
    /// let yesterday = time::now().wallclock() - 86_400_000_000;
    /// let valid_from = time::from_secs(yesterday / 1_000_000);
    ///
    /// tx.update_node_with_valid_time(
    ///     alice,
    ///     properties! { "city" => "London" },
    ///     Some(valid_from)
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn update_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Update a node's properties.
    ///
    /// This performs a PATCH update: only the specified properties are updated;
    /// others are preserved. To delete a property, set it to `PropertyValue::Null` (future feature)
    /// or explicit delete (not yet implemented).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, core::NodeId, api::transaction::WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let node_id = NodeId::new(1)?;
    /// // Only updates "age", preserves "name"
    /// tx.update_node(
    ///     node_id,
    ///     properties! { "age" => 31 }
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn update_node(&mut self, node_id: NodeId, properties: PropertyMap) -> Result<()> {
        self.update_node_with_valid_time(node_id, properties, None)
    }

    /// Update an edge's properties with optional backdated valid_from time.
    ///
    /// This merges the new properties with existing ones (PATCH semantics).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, core::EdgeId, api::transaction::WriteOps};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let edge_id = EdgeId::new(1)?;
    /// // Retroactively increase relationship strength
    /// let past_time = time::from_secs(1609459200);
    ///
    /// tx.update_edge_with_valid_time(
    ///     edge_id,
    ///     properties! { "strength" => 0.8 },
    ///     Some(past_time)
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn update_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Update an edge's properties.
    ///
    /// This performs a PATCH update: only the specified properties are updated;
    /// others are preserved.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, core::EdgeId, api::transaction::WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let edge_id = EdgeId::new(1)?;
    /// tx.update_edge(
    ///     edge_id,
    ///     properties! { "strength" => 0.95 }
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn update_edge(&mut self, edge_id: EdgeId, properties: PropertyMap) -> Result<()> {
        self.update_edge_with_valid_time(edge_id, properties, None)
    }

    /// Delete a node with optional backdated valid_from time (without deleting connected edges).
    ///
    /// # Warning
    ///
    /// This method does NOT delete edges connected to the node, which may leave
    /// orphaned edges in the graph. For most use cases, prefer
    /// [`delete_node_cascade`](Self::delete_node_cascade) which automatically
    /// removes all connected edges to maintain referential integrity.
    ///
    /// Only use this method if you explicitly need to preserve edges for some
    /// specialized use case.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, core::NodeId, api::transaction::WriteOps};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let alice = NodeId::new(1)?;
    /// // Mark node as deleted effective from 1 hour ago
    /// let one_hour_ago = time::now().wallclock() - 3_600_000_000;
    /// let valid_from = time::from_secs(one_hour_ago / 1_000_000);
    ///
    /// tx.delete_node_with_valid_time(alice, Some(valid_from))?;
    /// # Ok(())
    /// # }
    /// ```
    fn delete_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Delete a node (leaves connected edges).
    ///
    /// **Warning**: This leaves orphaned edges. Use [`delete_node_cascade`](Self::delete_node_cascade)
    /// for safe deletion.
    ///
    /// ## Examples
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder, properties};
    /// # use aletheiadb::api::transaction::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let mut tx = db.write_transaction()?;
    ///
    /// let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
    ///
    /// // Delete the node
    /// tx.delete_node(alice)?;
    ///
    /// tx.commit()?;
    /// # Ok(())
    /// # }
    /// ```
    fn delete_node(&mut self, node_id: NodeId) -> Result<()> {
        self.delete_node_with_valid_time(node_id, None)
    }

    /// Delete a node and all connected edges (cascade delete).
    ///
    /// This method deletes both the node and all edges where the node
    /// appears as either the source or target. This prevents orphaned edges
    /// and maintains referential integrity in the graph.
    ///
    /// # Performance
    ///
    /// The cascade delete operation is efficient even for highly-connected nodes:
    /// - O(degree) complexity where degree is the number of connected edges
    /// - All edge deletions are buffered and applied atomically on commit
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let properties = properties! { "name" => "DeleteMe" };
    /// let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node("Person", properties)?;
    /// // ... create edges ...
    /// tx.delete_node_cascade(node_id)?; // Deletes node and all connected edges
    /// tx.commit()?;
    /// # Ok(())
    /// # }
    /// ```
    fn delete_node_cascade(&mut self, node_id: NodeId) -> Result<()>;

    /// Delete an edge with optional backdated valid_from time
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, core::EdgeId, api::transaction::WriteOps};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let edge_id = EdgeId::new(1)?;
    /// // Mark edge as deleted in the past
    /// let past_time = time::from_secs(1609459200);
    ///
    /// tx.delete_edge_with_valid_time(edge_id, Some(past_time))?;
    /// # Ok(())
    /// # }
    /// ```
    fn delete_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Delete an edge (delegates to `delete_edge_with_valid_time` with `None`).
    ///
    /// ## Examples
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder, properties};
    /// # use aletheiadb::api::transaction::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let mut tx = db.write_transaction()?;
    ///
    /// let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
    /// let bob = tx.create_node("Person", properties! { "name" => "Bob" })?;
    /// let edge_id = tx.create_edge(alice, bob, "KNOWS", properties! {})?;
    ///
    /// // Delete the edge
    /// tx.delete_edge(edge_id)?;
    ///
    /// tx.commit()?;
    /// # Ok(())
    /// # }
    /// ```
    fn delete_edge(&mut self, edge_id: EdgeId) -> Result<()> {
        self.delete_edge_with_valid_time(edge_id, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_create_node_with_valid_time_trait_method_exists() {
        // This test verifies the trait method signature compiles
        fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
            // Trait bound check - if this compiles, the method exists
        }

        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }

    #[test]
    fn test_create_node_default_delegates_to_with_valid_time() {
        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();

        // Both should work identically when valid_from is None
        let props1 = PropertyMapBuilder::new().insert("name", "Test1").build();
        let props2 = PropertyMapBuilder::new().insert("name", "Test2").build();

        // Both should succeed
        let result1 = tx.create_node("Test", props1);
        assert!(result1.is_ok(), "create_node failed: {:?}", result1.err());
        let id1 = result1.unwrap();

        let result2 = tx.create_node_with_valid_time("Test", props2, None);
        assert!(
            result2.is_ok(),
            "create_node_with_valid_time failed: {:?}",
            result2.err()
        );
        let id2 = result2.unwrap();

        // IDs should be different (sequential generation)
        assert_ne!(id1, id2, "IDs should be unique");

        // Both methods should work - IDs are generated successfully
        // Note: First ID may be 0 due to IdGenerator starting at 0 (known issue)
        assert!(id1.as_u64() < id2.as_u64(), "IDs should increment");
    }

    #[test]
    fn test_create_node_with_backdated_valid_time() {
        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();

        // Create node with valid_time = 1 hour ago
        let one_hour_ago = time::now().wallclock() - 3_600_000_000;
        let valid_from = crate::core::hlc::HybridTimestamp::new(one_hour_ago, 0).unwrap();

        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let node_id = tx
            .create_node_with_valid_time("Person", props, Some(valid_from))
            .unwrap();

        // Verify node was created with a valid ID (0 is valid!)
        assert!(node_id.as_u64() <= MAX_VALID_ID);
    }

    #[test]
    fn test_create_edge_with_valid_time_trait_method_exists() {
        fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
            // Trait bound check
        }

        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }

    #[test]
    fn test_update_node_with_valid_time_trait_method_exists() {
        fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
            // Trait bound check
        }

        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }

    #[test]
    fn test_delete_node_with_valid_time_trait_method_exists() {
        fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
            // Trait bound check
        }

        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }
}
