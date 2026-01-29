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
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::PropertyMap;
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
    /// Create a new node
    fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId>;

    /// Create a new edge
    fn create_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId>;

    /// Update a node's properties (creates new version)
    fn update_node(&mut self, node_id: NodeId, properties: PropertyMap) -> Result<()>;

    /// Update an edge's properties (creates new version)
    fn update_edge(&mut self, edge_id: EdgeId, properties: PropertyMap) -> Result<()>;

    /// Delete a node (without deleting connected edges)
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
    fn delete_node(&mut self, node_id: NodeId) -> Result<()>;

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

    /// Delete an edge
    fn delete_edge(&mut self, edge_id: EdgeId) -> Result<()>;
}
