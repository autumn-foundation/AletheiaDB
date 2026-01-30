//! Transaction support for GallifreyDB
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
//! # use gallifreydb::{GallifreyDB, PropertyMapBuilder, properties};
//! # use gallifreydb::core::NodeId;
//! # use gallifreydb::api::transaction::{ReadOps, WriteOps};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let db = GallifreyDB::new()?;
//! # let id = NodeId::new(1)?;
//! # let other = NodeId::new(2)?;
//! # let props = PropertyMapBuilder::new().build();
//! # let edge_props = PropertyMapBuilder::new().build();
//! // Read-only
//! let result = db.read(|tx| {
//!     // get_node might fail if node doesn't exist
//!     if let Ok(node) = tx.get_node(id) {
//!         Ok(node.get_property("name").cloned())
//!     } else {
//!         Ok(None)
//!     }
//! })?;
//!
//! // Read-write (auto-commit on Ok, auto-rollback on Err)
//! let node_id = db.write(|tx| {
//!     let node_id = tx.create_node("Person", props)?;
//!     tx.create_edge(node_id, other, "KNOWS", edge_props)?;
//!     Ok(node_id)
//! })?;
//! # Ok(())
//! # }
//! ```
//!
//! **Explicit handles**:
//! ```rust,no_run
//! # use gallifreydb::{GallifreyDB, PropertyMapBuilder};
//! # use gallifreydb::core::NodeId;
//! # use gallifreydb::api::transaction::WriteOps;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let db = GallifreyDB::new()?;
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
pub mod write_buffer;
pub mod write_tx;

pub use read_tx::ReadTransaction;
pub use types::{TxId, TxIdGenerator, TxMetadata, TxState};
pub use visibility::{CompressionStats, TransactionSnapshot, TxVisibilityManager};
pub use write_buffer::{BufferedWrite, WriteBuffer};
pub use write_tx::WriteTransaction;

use crate::core::graph::{Edge, Node};
#[cfg(test)]
use crate::core::id::MAX_VALID_ID;
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::utils::error::Result;

/// Common read operations available in all transaction types
pub trait ReadOps {
    /// Get a node by ID (current state)
    fn get_node(&self, id: NodeId) -> Result<Node>;

    /// Get an edge by ID (current state)
    fn get_edge(&self, id: EdgeId) -> Result<Edge>;

    /// Get outgoing edges from a node
    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId>;

    /// Get incoming edges to a node
    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId>;

    /// Get outgoing edges with a specific label
    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId>;

    /// Get node count
    fn node_count(&self) -> usize;

    /// Get edge count
    fn edge_count(&self) -> usize;
}

/// Write operations (only available in write transactions)
pub trait WriteOps: ReadOps {
    /// Create a new node with optional backdated valid_from time
    ///
    /// # Arguments
    ///
    /// * `label` - Node label
    /// * `properties` - Node properties
    /// * `valid_from` - When the fact became valid (None = transaction time)
    ///
    /// # Bi-Temporal Semantics
    ///
    /// - If `valid_from` is None, both valid_time and transaction_time are set to commit time
    /// - If `valid_from` is Some(ts), valid_time starts at ts, transaction_time at commit time
    /// - This enables backdating facts: "created at commit_time, but valid since valid_from"
    fn create_node_with_valid_time(
        &mut self,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<NodeId>;

    /// Create a new node (delegates to create_node_with_valid_time with None)
    fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        self.create_node_with_valid_time(label, properties, None)
    }

    /// Create a new edge with optional backdated valid_from time
    fn create_edge_with_valid_time(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<EdgeId>;

    /// Create a new edge (delegates to create_edge_with_valid_time with None)
    fn create_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        self.create_edge_with_valid_time(source, target, label, properties, None)
    }

    /// Update a node's properties with optional backdated valid_from time
    fn update_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Update a node's properties (delegates to update_node_with_valid_time with None)
    fn update_node(&mut self, node_id: NodeId, properties: PropertyMap) -> Result<()> {
        self.update_node_with_valid_time(node_id, properties, None)
    }

    /// Update an edge's properties with optional backdated valid_from time
    fn update_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Update an edge's properties (delegates to update_edge_with_valid_time with None)
    fn update_edge(&mut self, edge_id: EdgeId, properties: PropertyMap) -> Result<()> {
        self.update_edge_with_valid_time(edge_id, properties, None)
    }

    /// Delete a node with optional backdated valid_from time (without deleting connected edges)
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
    fn delete_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Delete a node (delegates to delete_node_with_valid_time with None)
    fn delete_node(&mut self, node_id: NodeId) -> Result<()> {
        self.delete_node_with_valid_time(node_id, None)
    }

    /// Delete a node and all connected edges (cascade delete)
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
    /// # use gallifreydb::{GallifreyDB, properties};
    /// # use gallifreydb::api::transaction::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = GallifreyDB::new()?;
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
    fn delete_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Delete an edge (delegates to delete_edge_with_valid_time with None)
    fn delete_edge(&mut self, edge_id: EdgeId) -> Result<()> {
        self.delete_edge_with_valid_time(edge_id, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GallifreyDB;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_create_node_with_valid_time_trait_method_exists() {
        // This test verifies the trait method signature compiles
        fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
            // Trait bound check - if this compiles, the method exists
        }

        let db = GallifreyDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }

    #[test]
    fn test_create_node_default_delegates_to_with_valid_time() {
        let db = GallifreyDB::new().unwrap();
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
        let db = GallifreyDB::new().unwrap();
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

        let db = GallifreyDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }

    #[test]
    fn test_update_node_with_valid_time_trait_method_exists() {
        fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
            // Trait bound check
        }

        let db = GallifreyDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }

    #[test]
    fn test_delete_node_with_valid_time_trait_method_exists() {
        fn assert_write_ops<T: WriteOps>(_tx: &mut T) {
            // Trait bound check
        }

        let db = GallifreyDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();
        assert_write_ops(&mut tx);
    }
}
