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
pub use visibility::{TransactionSnapshot, TxVisibilityManager};
pub use write::WriteTransaction;
pub use write_buffer::{BufferedWrite, WriteBuffer};

use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
#[cfg(test)]
use crate::core::id::MAX_VALID_ID;
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;

/// Optional per-write settings: backdated `valid_from` and/or a write-time
/// [`Provenance`] bundle (Issue #3224).
///
/// Constructed via [`WriteRequestOptions::new`] (or its [`Default`] impl) and the
/// `with_*` builder methods. Passing `WriteRequestOptions::default()` reproduces the
/// behavior of the plain `create_node`/`update_node`/etc. convenience methods
/// exactly (valid time defaults to transaction start time, no provenance).
#[derive(Debug, Clone, Default)]
pub struct WriteRequestOptions {
    pub(crate) valid_from: Option<Timestamp>,
    pub(crate) provenance: Option<Provenance>,
}

impl WriteRequestOptions {
    /// Create an empty set of options (equivalent to [`Default::default`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set when the fact became valid in reality (`None` = transaction start time).
    #[must_use]
    pub fn with_valid_from(mut self, valid_from: Timestamp) -> Self {
        self.valid_from = Some(valid_from);
        self
    }

    /// Attach a write-time provenance bundle to this write.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = Some(provenance);
        self
    }
}

/// Outcome of a valid-time retraction (Issue #3230).
///
/// Returned by [`WriteTransaction::retract_node`]/[`WriteTransaction::retract_edge`]
/// and the [`AletheiaDB`](crate::db::AletheiaDB) convenience wrappers. The
/// closed valid interval is half-open: `[valid_from, valid_to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetractionResult {
    /// Start of the entity's (now closed) valid-time interval.
    pub valid_from: Timestamp,
    /// End of the entity's valid-time interval (exclusive). For an
    /// idempotent re-retraction this is the *existing* end, regardless of
    /// the `valid_to` passed to the call.
    pub valid_to: Timestamp,
    /// `true` when the entity was already retracted (or deleted) and this
    /// call was a no-op: no new version was appended and no WAL entry was
    /// written.
    pub already_retracted: bool,
    /// Number of connected edges co-retracted alongside a node (only set by
    /// the detach form; `0` otherwise and for edge retractions).
    pub edges_retracted: usize,
}

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
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, core::NodeId, api::transaction::ReadOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let tx = db.read_transaction()?;
    /// # let node_id = NodeId::new(1)?;
    /// let node = tx.get_node(node_id)?;
    /// println!("Node label: {:?}", node.label);
    /// # Ok(())
    /// # }
    /// ```
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
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, core::EdgeId, api::transaction::ReadOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let tx = db.read_transaction()?;
    /// # let edge_id = EdgeId::new(1)?;
    /// let edge = tx.get_edge(edge_id)?;
    /// println!("Edge label: {:?}", edge.label);
    /// # Ok(())
    /// # }
    /// ```
    fn get_edge(&self, id: EdgeId) -> Result<Edge>;

    /// Get outgoing edges from a node.
    ///
    /// Returns all edges where `source == node_id` that are visible in the current snapshot.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The source node to get edges from
    ///
    /// # Returns
    ///
    /// - `Ok(edge_ids)` - The node exists (is visible in this transaction's snapshot).
    ///   The vector is **empty if the node has no outgoing edges** - that is a valid
    ///   state, not an error.
    /// - `Err(NodeNotFound)` - The node does not exist (or is not visible in this
    ///   transaction's snapshot).
    ///
    /// This mirrors [`get_node`](Self::get_node): the same condition (a missing node)
    /// produces an error, so callers can distinguish "node has no edges" from
    /// "node doesn't exist" (Issue #359).
    ///
    /// # Ordering
    ///
    /// The order of edges is **not guaranteed**. Do not rely on edges being returned
    /// in insertion order or sorted by ID. The internal storage may reorder edges
    /// during compaction or persistence.
    ///
    /// # Snapshot Isolation
    ///
    /// In a read transaction, this method filters edges to ensure only those visible
    /// in the current transaction snapshot are returned. Edges created by concurrent
    /// transactions will not be seen.
    ///
    /// # Write transactions
    ///
    /// In a write transaction, only the **node existence check** is buffer-aware;
    /// the returned edge list is read directly from committed current storage
    /// **without snapshot filtering**. Consequently:
    ///
    /// - edges committed by concurrent transactions after this transaction
    ///   started **are** returned,
    /// - edges created in this transaction (still buffered, not yet committed)
    ///   are **not** returned, and
    /// - edges deleted in this transaction are **still** listed until commit.
    ///
    /// # Performance
    ///
    /// - **Time**: O(degree) to collect visible edges, plus a node existence check
    ///   (O(1) fast path; may consult historical storage, O(log N), on a miss)
    /// - **Space**: Allocates a new `Vec` containing all edge IDs
    ///
    /// # Example
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties, core::NodeId};
    /// # use aletheiadb::api::transaction::{ReadOps, WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let (alice, bob) = db.write(|tx| {
    ///     let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
    ///     let bob = tx.create_node("Person", properties! { "name" => "Bob" })?;
    ///     tx.create_edge(alice, bob, "KNOWS", properties! {})?;
    ///     Ok::<_, aletheiadb::Error>((alice, bob))
    /// })?;
    ///
    /// let tx = db.read_transaction()?;
    ///
    /// // Alice has one outgoing edge:
    /// let edges = tx.get_outgoing_edges(alice)?;
    /// assert_eq!(edges.len(), 1);
    /// for edge_id in edges {
    ///     let edge = tx.get_edge(edge_id)?;
    ///     assert_eq!(edge.target, bob);
    /// }
    ///
    /// // Bob exists but has no outgoing edges: Ok(empty), not an error.
    /// assert!(tx.get_outgoing_edges(bob)?.is_empty());
    ///
    /// // A node that doesn't exist is an error, distinguishable from "no edges".
    /// let missing = NodeId::new(999_999)?;
    /// assert!(tx.get_outgoing_edges(missing).is_err());
    /// # Ok(())
    /// # }
    /// ```
    fn get_outgoing_edges(&self, node_id: NodeId) -> Result<Vec<EdgeId>>;

    /// Get incoming edges to a node.
    ///
    /// Returns all edges where `target == node_id` that are visible in the current snapshot.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The target node to get edges to
    ///
    /// # Returns
    ///
    /// - `Ok(edge_ids)` - The node exists (is visible in this transaction's snapshot).
    ///   The vector is **empty if the node has no incoming edges** - that is a valid
    ///   state, not an error.
    /// - `Err(NodeNotFound)` - The node does not exist (or is not visible in this
    ///   transaction's snapshot).
    ///
    /// # Ordering
    ///
    /// The order of edges is **not guaranteed**.
    ///
    /// # Snapshot Isolation
    ///
    /// In a read transaction, this method filters edges to ensure only those visible
    /// in the current transaction snapshot are returned.
    ///
    /// # Write transactions
    ///
    /// In a write transaction, only the **node existence check** is buffer-aware;
    /// the returned edge list is read directly from committed current storage
    /// **without snapshot filtering**. Consequently:
    ///
    /// - edges committed by concurrent transactions after this transaction
    ///   started **are** returned,
    /// - edges created in this transaction (still buffered, not yet committed)
    ///   are **not** returned, and
    /// - edges deleted in this transaction are **still** listed until commit.
    ///
    /// # Performance
    ///
    /// - **Time**: O(degree) to collect visible edges, plus a node existence check
    ///   (O(1) fast path; may consult historical storage, O(log N), on a miss)
    /// - **Space**: Allocates a new `Vec` containing all edge IDs
    ///
    /// # Example
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::{ReadOps, WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let (alice, bob) = db.write(|tx| {
    ///     let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
    ///     let bob = tx.create_node("Person", properties! { "name" => "Bob" })?;
    ///     tx.create_edge(alice, bob, "KNOWS", properties! {})?;
    ///     Ok::<_, aletheiadb::Error>((alice, bob))
    /// })?;
    ///
    /// let tx = db.read_transaction()?;
    ///
    /// // Bob has one incoming edge (from Alice):
    /// assert_eq!(tx.get_incoming_edges(bob)?.len(), 1);
    ///
    /// // Alice exists but has no incoming edges: Ok(empty), not an error.
    /// assert!(tx.get_incoming_edges(alice)?.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    fn get_incoming_edges(&self, node_id: NodeId) -> Result<Vec<EdgeId>>;

    /// Get outgoing edges with a specific label.
    ///
    /// Returns all edges where `source == node_id` AND `label == label` that are
    /// visible in the current snapshot.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The source node
    /// * `label` - Edge label to filter by (e.g., "KNOWS", "CREATED")
    ///
    /// # Returns
    ///
    /// - `Ok(edge_ids)` - The node exists (is visible in this transaction's snapshot).
    ///   The vector is **empty if no outgoing edges carry the given label** - the
    ///   label is a filter, not an existence check, so an unmatched label on an
    ///   existing node is a valid state, not an error.
    /// - `Err(NodeNotFound)` - The node does not exist (or is not visible in this
    ///   transaction's snapshot).
    ///
    /// # Ordering
    ///
    /// The order of edges is **not guaranteed**.
    ///
    /// # Write transactions
    ///
    /// In a write transaction, only the **node existence check** is buffer-aware;
    /// the returned edge list is read directly from committed current storage
    /// **without snapshot filtering**. Consequently:
    ///
    /// - edges committed by concurrent transactions after this transaction
    ///   started **are** returned,
    /// - edges created in this transaction (still buffered, not yet committed)
    ///   are **not** returned, and
    /// - edges deleted in this transaction are **still** listed until commit.
    ///
    /// # Performance
    ///
    /// - **Time**: O(degree) scan with label filtering, plus a node existence check
    ///   (O(1) fast path; may consult historical storage, O(log N), on a miss)
    /// - **Space**: Allocates a new `Vec` containing matching edge IDs
    ///
    /// # Example
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # use aletheiadb::api::transaction::{ReadOps, WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let (alice, bob) = db.write(|tx| {
    ///     let alice = tx.create_node("Person", properties! { "name" => "Alice" })?;
    ///     let bob = tx.create_node("Person", properties! { "name" => "Bob" })?;
    ///     tx.create_edge(alice, bob, "KNOWS", properties! {})?;
    ///     Ok::<_, aletheiadb::Error>((alice, bob))
    /// })?;
    ///
    /// let tx = db.read_transaction()?;
    ///
    /// // One outgoing KNOWS edge:
    /// assert_eq!(tx.get_outgoing_edges_with_label(alice, "KNOWS")?.len(), 1);
    ///
    /// // No FOLLOWS edges, but Alice exists: Ok(empty), not an error.
    /// assert!(tx.get_outgoing_edges_with_label(alice, "FOLLOWS")?.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Result<Vec<EdgeId>>;

    /// Get the approximate number of nodes in the database.
    ///
    /// # Returns
    ///
    /// The count of nodes currently committed in the storage engine. Returns `0`
    /// for an empty database. Deleted nodes are not counted. Nodes created in
    /// this transaction but not yet committed are **not** included. Conversely,
    /// nodes deleted in this transaction but not yet committed are still included.
    ///
    /// # Consistency Note
    ///
    /// This returns the **current** count of committed nodes in the storage engine.
    /// Unlike `get_node()`, this count is **NOT snapshot-isolated**. It may include
    /// nodes created by transactions that committed after this read transaction started.
    ///
    /// This design choice enables O(1) performance without scanning the entire
    /// database to filter visibility for every node.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, api::transaction::ReadOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let tx = db.read_transaction()?;
    /// let count = tx.node_count();
    /// println!("Database contains {} nodes", count);
    /// # Ok(())
    /// # }
    /// ```
    fn node_count(&self) -> usize;

    /// Get the approximate number of edges in the database.
    ///
    /// # Returns
    ///
    /// The count of edges currently committed in the storage engine. Returns `0`
    /// for an empty database. Deleted edges are not counted. Edges created in
    /// this transaction but not yet committed are **not** included. Conversely,
    /// edges deleted in this transaction but not yet committed are still included.
    ///
    /// # Consistency Note
    ///
    /// This returns the **current** count of committed edges in the storage engine.
    /// Unlike `get_edge()`, this count is **NOT snapshot-isolated**. It may include
    /// edges created by transactions that committed after this read transaction started.
    ///
    /// This design choice enables O(1) performance without scanning the entire
    /// database to filter visibility for every edge.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, api::transaction::ReadOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let tx = db.read_transaction()?;
    /// let count = tx.edge_count();
    /// println!("Database contains {} edges", count);
    /// # Ok(())
    /// # }
    /// ```
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
    ) -> Result<NodeId> {
        let options = WriteRequestOptions {
            valid_from,
            provenance: None,
        };
        self.create_node_with_options(label, properties, options)
    }

    /// Create a new node with an optional [`WriteRequestOptions`] bundle (backdated
    /// `valid_from` and/or a write-time [`Provenance`] bundle, Issue #3224).
    ///
    /// This is the most general node-creation method; all other
    /// `create_node*` methods delegate to it. Passing `WriteRequestOptions::default()`
    /// is identical to [`create_node`](Self::create_node).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, Provenance, api::transaction::{WriteOps, WriteRequestOptions}};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let mut tx = db.write_transaction()?;
    /// let provenance = Provenance::builder().source("hr-system").confidence(0.95).build()?;
    /// let node_id = tx.create_node_with_options(
    ///     "Person",
    ///     properties! { "name" => "Alice" },
    ///     WriteRequestOptions::new().with_provenance(provenance),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    fn create_node_with_options(
        &mut self,
        label: &str,
        properties: PropertyMap,
        options: WriteRequestOptions,
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
    ) -> Result<EdgeId> {
        let options = WriteRequestOptions {
            valid_from,
            provenance: None,
        };
        self.create_edge_with_options(source, target, label, properties, options)
    }

    /// Create a new edge with an optional [`WriteRequestOptions`] bundle (backdated
    /// `valid_from` and/or a write-time [`Provenance`] bundle, Issue #3224).
    ///
    /// This is the most general edge-creation method; all other
    /// `create_edge*` methods delegate to it.
    fn create_edge_with_options(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        options: WriteRequestOptions,
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
    ) -> Result<()> {
        let options = WriteRequestOptions {
            valid_from,
            provenance: None,
        };
        self.update_node_with_options(node_id, properties, options)
    }

    /// Update a node's properties with an optional [`WriteRequestOptions`] bundle
    /// (backdated `valid_from` and/or a write-time [`Provenance`] bundle,
    /// Issue #3224).
    ///
    /// This is the most general node-update method; all other
    /// `update_node*` methods delegate to it. The provenance recorded here
    /// describes *this* version only -- it is not inherited from the
    /// version being updated.
    fn update_node_with_options(
        &mut self,
        node_id: NodeId,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<()>;

    /// Compare-and-set a node's properties, conditional on its committed head
    /// still being `expected_version` (Issue #3577).
    ///
    /// This is a conditional **full replace** of the node's property map (not a
    /// PATCH merge): the whole `properties` map becomes the new state, matching
    /// the "write the whole claim state" semantics the lease layer builds on.
    /// The label and node id are preserved.
    ///
    /// # Semantics
    ///
    /// The precondition is enforced at **commit time, under the
    /// commit-serialization guard** — so two claimants opened on the same
    /// snapshot cannot both succeed (the second observes the first's new version
    /// and aborts). On success the new version id is returned. On a lost claim
    /// the whole transaction aborts with
    /// [`TransactionError::CasMismatch`](crate::core::error::TransactionError::CasMismatch)
    /// (a non-retriable precondition failure, distinct from the retriable
    /// `SerializationFailure`) and **nothing is written**. A CAS against a
    /// nonexistent or deleted node fails with `CasMismatch { actual: None }`
    /// rather than a `NodeNotFound`.
    fn compare_and_set_node(
        &mut self,
        node_id: NodeId,
        expected_version: crate::core::id::VersionId,
        properties: PropertyMap,
    ) -> Result<crate::core::id::VersionId> {
        self.compare_and_set_node_with_options(
            node_id,
            expected_version,
            properties,
            WriteRequestOptions::default(),
        )
    }

    /// [`compare_and_set_node`](Self::compare_and_set_node) with a
    /// [`WriteRequestOptions`] bundle (backdated `valid_from` and/or write-time
    /// provenance parity with `update_node`). This is the most general node-CAS
    /// method; `compare_and_set_node` delegates to it.
    fn compare_and_set_node_with_options(
        &mut self,
        node_id: NodeId,
        expected_version: crate::core::id::VersionId,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<crate::core::id::VersionId>;

    /// Compare-and-set an edge's properties, conditional on its committed head
    /// still being `expected_version` (Issue #3577).
    ///
    /// Like [`compare_and_set_node`](Self::compare_and_set_node), but for an
    /// edge: endpoints and type are immutable (only the property map is
    /// conditionally replaced, mirroring `replace_edge`). Returns the new
    /// version id on success; aborts with
    /// [`TransactionError::CasMismatch`](crate::core::error::TransactionError::CasMismatch)
    /// on a version mismatch.
    fn compare_and_set_edge(
        &mut self,
        edge_id: EdgeId,
        expected_version: crate::core::id::VersionId,
        properties: PropertyMap,
    ) -> Result<crate::core::id::VersionId> {
        self.compare_and_set_edge_with_options(
            edge_id,
            expected_version,
            properties,
            WriteRequestOptions::default(),
        )
    }

    /// [`compare_and_set_edge`](Self::compare_and_set_edge) with a
    /// [`WriteRequestOptions`] bundle. The most general edge-CAS method.
    fn compare_and_set_edge_with_options(
        &mut self,
        edge_id: EdgeId,
        expected_version: crate::core::id::VersionId,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<crate::core::id::VersionId>;

    /// Claim a node via a lease, succeeding iff the version still matches OR the
    /// existing lease is expired (Issue #3577).
    ///
    /// A thin convenience over [`compare_and_set_node`](Self::compare_and_set_node):
    /// it stamps `lease_owner_key = owner` and `lease_until_key = lease_until`
    /// (as integer microseconds since epoch) into `properties` (a full replace),
    /// then buffers a CAS whose commit-time precondition is
    /// `current_version == expected_version` **OR** the entity's current
    /// `lease_until_key` property is `<=` the commit timestamp (the lease is
    /// expired / unclaimed). The property key names are caller-supplied — this is
    /// a convention, not a hardcoded schema. Lease expiry is evaluated against
    /// the commit HLC timestamp, not the transaction snapshot. Returns the new
    /// version id on success; aborts with
    /// [`TransactionError::CasMismatch`](crate::core::error::TransactionError::CasMismatch)
    /// when the version is stale AND the lease is still held.
    #[allow(clippy::too_many_arguments)]
    fn claim_with_lease(
        &mut self,
        node_id: NodeId,
        expected_version: crate::core::id::VersionId,
        lease_owner_key: &str,
        lease_until_key: &str,
        owner: PropertyValue,
        lease_until: Timestamp,
        properties: PropertyMap,
    ) -> Result<crate::core::id::VersionId> {
        self.claim_with_lease_with_options(
            node_id,
            expected_version,
            lease_owner_key,
            lease_until_key,
            owner,
            lease_until,
            properties,
            WriteRequestOptions::default(),
        )
    }

    /// [`claim_with_lease`](Self::claim_with_lease) with a
    /// [`WriteRequestOptions`] bundle. The most general claim method.
    #[allow(clippy::too_many_arguments)]
    fn claim_with_lease_with_options(
        &mut self,
        node_id: NodeId,
        expected_version: crate::core::id::VersionId,
        lease_owner_key: &str,
        lease_until_key: &str,
        owner: PropertyValue,
        lease_until: Timestamp,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<crate::core::id::VersionId>;

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
    ) -> Result<()> {
        let options = WriteRequestOptions {
            valid_from,
            provenance: None,
        };
        self.update_edge_with_options(edge_id, properties, options)
    }

    /// Update an edge's properties with an optional [`WriteRequestOptions`] bundle
    /// (backdated `valid_from` and/or a write-time [`Provenance`] bundle,
    /// Issue #3224).
    ///
    /// This is the most general edge-update method; all other
    /// `update_edge*` methods delegate to it. The provenance recorded here
    /// describes *this* version only -- it is not inherited from the
    /// version being updated.
    fn update_edge_with_options(
        &mut self,
        edge_id: EdgeId,
        properties: PropertyMap,
        options: WriteRequestOptions,
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
    ) -> Result<()> {
        self.delete_node_with_options(
            node_id,
            WriteRequestOptions {
                valid_from,
                provenance: None,
            },
        )
    }

    /// Delete a node with an optional [`WriteRequestOptions`] bundle: a
    /// backdated deletion `valid_from` and/or a write-time [`Provenance`]
    /// bundle recording the acting principal on the tombstone version
    /// (Issue #3427).
    ///
    /// This is the most general non-cascade node-delete method; all other
    /// `delete_node*` (non-cascade) methods delegate to it. Passing
    /// `WriteRequestOptions::default()` is identical to
    /// [`delete_node`](Self::delete_node) — no provenance, `valid_from`
    /// defaults to the transaction start time.
    ///
    /// # Warning
    ///
    /// Like [`delete_node`](Self::delete_node), this does NOT delete connected
    /// edges. Prefer [`delete_node_cascade`](Self::delete_node_cascade) /
    /// [`delete_node_cascade_with_options`](Self::delete_node_cascade_with_options)
    /// for referential safety.
    fn delete_node_with_options(
        &mut self,
        node_id: NodeId,
        options: WriteRequestOptions,
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
    fn delete_node_cascade(&mut self, node_id: NodeId) -> Result<()> {
        self.delete_node_cascade_with_options(node_id, WriteRequestOptions::default())
    }

    /// Delete a node and all connected edges (cascade delete) with an optional
    /// [`WriteRequestOptions`] bundle (Issue #3427).
    ///
    /// The same `options` (backdated `valid_from` and/or a write-time
    /// [`Provenance`] bundle recording the acting principal) is stamped onto
    /// the node's tombstone **and every co-deleted edge's tombstone**, so a
    /// cascade delete attributes every tombstone it creates, not just the node.
    /// Passing `WriteRequestOptions::default()` is identical to
    /// [`delete_node_cascade`](Self::delete_node_cascade).
    fn delete_node_cascade_with_options(
        &mut self,
        node_id: NodeId,
        options: WriteRequestOptions,
    ) -> Result<()>;

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
    ) -> Result<()> {
        self.delete_edge_with_options(
            edge_id,
            WriteRequestOptions {
                valid_from,
                provenance: None,
            },
        )
    }

    /// Delete an edge with an optional [`WriteRequestOptions`] bundle: a
    /// backdated deletion `valid_from` and/or a write-time [`Provenance`]
    /// bundle recording the acting principal on the tombstone version
    /// (Issue #3427).
    ///
    /// This is the most general edge-delete method; all other `delete_edge*`
    /// methods delegate to it. Passing `WriteRequestOptions::default()` is
    /// identical to [`delete_edge`](Self::delete_edge).
    fn delete_edge_with_options(
        &mut self,
        edge_id: EdgeId,
        options: WriteRequestOptions,
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

    // ===== Replace / tombstone (non-PATCH) writes (Issue #3549) =====

    /// Replace a node's **entire** property map and label (non-PATCH overwrite).
    ///
    /// Unlike [`update_node`](Self::update_node) (PATCH: merges the incoming map
    /// onto the existing one), this performs a full overwrite: the node's
    /// resulting property map is *exactly* `properties`, and its label becomes
    /// `label`. Any key present on the prior version but absent from
    /// `properties` is **removed** from current state — its history is
    /// preserved and still recallable `AS OF` an earlier bi-temporal coordinate
    /// (anchor *both* dimensions before the replace). Passing an empty map
    /// removes all properties while keeping the node in existence.
    ///
    /// Node label mutation is only possible through this method;
    /// [`update_node`](Self::update_node) preserves the label.
    ///
    /// This is the most general node-replace method; the other `replace_node*`
    /// methods delegate to it. The provenance recorded here describes *this*
    /// version only — it is not inherited from the version being replaced.
    fn replace_node_with_options(
        &mut self,
        node_id: NodeId,
        label: &str,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<()>;

    /// Replace a node's entire property map and label with an optional backdated
    /// `valid_from` time. See [`replace_node_with_options`](Self::replace_node_with_options).
    fn replace_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.replace_node_with_options(
            node_id,
            label,
            properties,
            WriteRequestOptions {
                valid_from,
                provenance: None,
            },
        )
    }

    /// Replace a node's entire property map and label (full overwrite).
    ///
    /// Convenience wrapper: `valid_from` defaults to the transaction start time.
    /// See [`replace_node_with_options`](Self::replace_node_with_options).
    fn replace_node(
        &mut self,
        node_id: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<()> {
        self.replace_node_with_valid_time(node_id, label, properties, None)
    }

    /// Replace an edge's **entire** property map (non-PATCH overwrite).
    ///
    /// Overwrites the property map exactly, like
    /// [`replace_node_with_options`](Self::replace_node_with_options), but the
    /// edge's `source`, `target`, and edge type (label) are **immutable** and
    /// preserved from the existing edge (edge-type mutation is out of scope).
    /// Any key absent from `properties` is removed from current state; history
    /// is preserved. Passing an empty map removes all properties.
    ///
    /// This is the most general edge-replace method; the other `replace_edge*`
    /// methods delegate to it.
    fn replace_edge_with_options(
        &mut self,
        edge_id: EdgeId,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<()>;

    /// Replace an edge's entire property map with an optional backdated
    /// `valid_from` time. See [`replace_edge_with_options`](Self::replace_edge_with_options).
    fn replace_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.replace_edge_with_options(
            edge_id,
            properties,
            WriteRequestOptions {
                valid_from,
                provenance: None,
            },
        )
    }

    /// Replace an edge's entire property map (full overwrite; endpoints/type
    /// immutable). Convenience wrapper: `valid_from` defaults to the
    /// transaction start time. See
    /// [`replace_edge_with_options`](Self::replace_edge_with_options).
    fn replace_edge(&mut self, edge_id: EdgeId, properties: PropertyMap) -> Result<()> {
        self.replace_edge_with_valid_time(edge_id, properties, None)
    }

    /// Remove a single property key from a node (read-modify-replace).
    ///
    /// Reads the node's current property map (read-your-own-writes within the
    /// transaction), drops `key`, and records a full replacement version with
    /// the reduced map and the node's existing label. Removing a key that is
    /// **absent** is a no-op that still succeeds and records **no** new version.
    /// History is preserved: the removed key is still recallable `AS OF` an
    /// earlier bi-temporal coordinate.
    fn remove_node_property(&mut self, node_id: NodeId, key: &str) -> Result<()>;

    /// Remove a single property key from an edge (read-modify-replace).
    ///
    /// Edge counterpart of [`remove_node_property`](Self::remove_node_property);
    /// the edge's endpoints and type are preserved. Removing an absent key is a
    /// no-op success that records no new version.
    fn remove_edge_property(&mut self, edge_id: EdgeId, key: &str) -> Result<()>;
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
