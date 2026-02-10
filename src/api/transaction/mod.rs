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
//! ```rust
//! use aletheiadb::{AletheiaDB, PropertyMapBuilder, properties};
//! use aletheiadb::core::NodeId;
//! use aletheiadb::api::transaction::{ReadOps, WriteOps};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//!
//! // Write transaction (auto-commit on Ok, auto-rollback on Err)
//! let (alice_id, bob_id) = db.write(|tx| {
//!     let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
//!     let bob = tx.create_node("Person", properties! { "name" => "Bob" })?;
//!     tx.create_edge(alice, bob, "KNOWS", properties! { "since" => 2024 })?;
//!     Ok((alice, bob))
//! })?;
//!
//! // Read-only transaction
//! db.read(|tx| {
//!     let alice = tx.get_node(alice_id)?;
//!     assert_eq!(alice.get_property("name").and_then(|v| v.as_str()), Some("Alice"));
//!     Ok(())
//! })?;
//! # Ok(())
//! # }
//! ```
//!
//! **Explicit handles**:
//! ```rust
//! use aletheiadb::{AletheiaDB, PropertyMapBuilder, properties};
//! use aletheiadb::api::transaction::WriteOps;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//!
//! let mut tx = db.write_transaction()?;
//! let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
//! let bob = tx.create_node("Person", properties! { "name" => "Bob" })?;
//! tx.create_edge(alice, bob, "KNOWS", properties! { "since" => 2024 })?;
//!
//! // Must explicitly commit!
//! tx.commit()?;
//! # Ok(())
//! # }
//! ```

pub mod read_tx;
pub mod types;
pub mod visibility;
pub mod write;
pub mod write_buffer;

pub use read_tx::ReadTransaction;
pub use types::{TxId, TxIdGenerator, TxMetadata, TxState};
pub use visibility::{CompressionStats, TransactionSnapshot, TxVisibilityManager};
pub use write::WriteTransaction;
pub use write_buffer::{BufferedWrite, WriteBuffer};

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::Timestamp;
use crate::utils::error::Result;

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
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::{ReadOps, WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node_id = db.write(|tx| tx.create_node("Person", properties! {}))?;
    /// db.read(|tx| {
    ///     let edges = tx.get_outgoing_edges(node_id);
    ///     for edge_id in edges {
    ///         let edge = tx.get_edge(edge_id)?;
    ///         println!("-> {}", edge.target);
    ///     }
    ///     Ok(())
    /// })?;
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
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::WriteOps;
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
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let alice_id = tx.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let bob_id = tx.create_node("Person", properties! { "name" => "Bob" })?;
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
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let node_id = tx.create_node("Person", properties! { "name" => "Alice", "age" => 30 })?;
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
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// # let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let bob = tx.create_node("Person", properties! { "name" => "Bob" })?;
    /// # let edge_id = tx.create_edge(alice, bob, "KNOWS", properties! { "strength" => 0.5 })?;
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
    fn delete_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()>;

    /// Delete a node (leaves connected edges).
    ///
    /// **Warning**: This leaves orphaned edges. Use [`delete_node_cascade`](Self::delete_node_cascade)
    /// for safe deletion.
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
    /// ```rust
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
mod tests;
