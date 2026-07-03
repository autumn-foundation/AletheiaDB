//! Basic graph operations for creating and reading nodes and edges.
//!
//! Contains convenience methods for the most common graph operations.
use crate::api::transaction::WriteOps;
use crate::core::error::{Result, ResultExt};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::storage::current::{IncomingEdgesIter, OutgoingEdgesIter};
use crate::storage::wal::WalOperation;

impl AletheiaDB {
    /// Create a node with the given label and properties.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    /// `valid_time` defaults to the transaction start time; use
    /// [`create_node_with_valid_time`](Self::create_node_with_valid_time) to
    /// backdate or future-date the fact.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let node_id = db.create_node(
    ///     "Person",
    ///     PropertyMapBuilder::new()
    ///         .insert("name", "Alice")
    ///         .build()
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # See Also
    ///
    /// * [`write`](Self::write) - For batched write operations.
    /// * [`create_node_with_valid_time`](Self::create_node_with_valid_time) - To set a specific valid time.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        self.create_node_with_valid_time(label, properties, None)
    }

    /// Create a node with the given label, properties, and an optional valid time.
    ///
    /// Use this to record a fact whose real-world effective date differs from
    /// "now" -- for example, an LLM extracting "Alice became CEO on 2021-03-01"
    /// from a document ingested today. When `valid_from` is `None`, this behaves
    /// exactly like [`create_node`](Self::create_node) (valid time defaults to
    /// the transaction start time). Transaction time is always system-assigned
    /// to the commit time and cannot be set by the caller.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TemporalError`](crate::core::error::TemporalError) if
    /// `valid_from` is more than one year in the future; it is never silently
    /// coerced.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// // Record that Alice became CEO on 2021-03-01, even though we're
    /// // recording it today.
    /// let march_2021 = time::from_secs(1614556800);
    /// let node_id = db.create_node_with_valid_time(
    ///     "Person",
    ///     PropertyMapBuilder::new()
    ///         .insert("name", "Alice")
    ///         .insert("title", "CEO")
    ///         .build(),
    ///     Some(march_2021),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # See Also
    ///
    /// * [`write`](Self::write) - For batched write operations.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_node_with_valid_time(
        &self,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<NodeId> {
        self.write(|tx| tx.create_node_with_valid_time(label, properties, valid_from))
    }

    /// Create an edge between two nodes.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    /// `valid_time` defaults to the transaction start time; use
    /// [`create_edge_with_valid_time`](Self::create_edge_with_valid_time) to
    /// backdate or future-date the fact.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder, core::NodeId};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = NodeId::new(1)?;
    /// # let target_id = NodeId::new(2)?;
    /// let edge_id = db.create_edge(
    ///     source_id,
    ///     target_id,
    ///     "KNOWS",
    ///     PropertyMapBuilder::new().insert("since", 2024).build()
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # See Also
    ///
    /// * [`write`](Self::write) - For batched write operations.
    /// * [`create_edge_with_valid_time`](Self::create_edge_with_valid_time) - To set a specific valid time.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_edge(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        self.create_edge_with_valid_time(source, target, label, properties, None)
    }

    /// Create an edge between two nodes with an optional valid time.
    ///
    /// See [`create_node_with_valid_time`](Self::create_node_with_valid_time)
    /// for the bi-temporal semantics: `valid_from` sets when the relationship
    /// became true in the real world, while transaction time always remains
    /// system-assigned to the commit time.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TemporalError`](crate::core::error::TemporalError) if
    /// `valid_from` is more than one year in the future.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder, core::NodeId};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = NodeId::new(1)?;
    /// # let target_id = NodeId::new(2)?;
    /// let since_2021 = time::from_secs(1609459200);
    /// let edge_id = db.create_edge_with_valid_time(
    ///     source_id,
    ///     target_id,
    ///     "KNOWS",
    ///     PropertyMapBuilder::new().build(),
    ///     Some(since_2021),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # See Also
    ///
    /// * [`write`](Self::write) - For batched write operations.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_edge_with_valid_time(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<EdgeId> {
        self.write(|tx| {
            tx.create_edge_with_valid_time(source, target, label, properties, valid_from)
        })
    }

    /// Update a node's properties with an optional valid time (PATCH semantics).
    ///
    /// When `valid_from` is `None`, valid time defaults to the transaction
    /// start time. Pass `Some(ts)` to retroactively correct or future-date a
    /// fact. Transaction time is always system-assigned.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TemporalError`](crate::core::error::TemporalError) if
    /// `valid_from` is more than one year in the future, or precedes the
    /// node's own creation time.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder, core::NodeId};
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node_id = NodeId::new(1)?;
    /// let yesterday = time::from_secs(time::now().wallclock() / 1_000_000 - 86_400);
    /// db.update_node_with_valid_time(
    ///     node_id,
    ///     PropertyMapBuilder::new().insert("city", "London").build(),
    ///     Some(yesterday),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn update_node_with_valid_time(
        &self,
        node_id: NodeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.write(|tx| tx.update_node_with_valid_time(node_id, properties, valid_from))
    }

    /// Update an edge's properties with an optional valid time (PATCH semantics).
    ///
    /// See [`update_node_with_valid_time`](Self::update_node_with_valid_time)
    /// for the bi-temporal semantics.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TemporalError`](crate::core::error::TemporalError) if
    /// `valid_from` is more than one year in the future, or precedes the
    /// edge's own creation time.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn update_edge_with_valid_time(
        &self,
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.write(|tx| tx.update_edge_with_valid_time(edge_id, properties, valid_from))
    }

    /// Delete a node with an optional valid time (without deleting connected edges).
    ///
    /// # Warning
    ///
    /// This does NOT delete edges connected to the node, which may leave
    /// orphaned edges in the graph. For most use cases, prefer a cascade
    /// delete that also removes connected edges. Only use this method if you
    /// explicitly need to preserve edges for a specialized use case.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TemporalError`](crate::core::error::TemporalError) if
    /// `valid_from` is more than one year in the future, or precedes the
    /// node's own creation time.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_node_with_valid_time(
        &self,
        node_id: NodeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.write(|tx| tx.delete_node_with_valid_time(node_id, valid_from))
    }

    /// Delete an edge with an optional valid time.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TemporalError`](crate::core::error::TemporalError) if
    /// `valid_from` is more than one year in the future, or precedes the
    /// edge's own creation time.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_edge_with_valid_time(
        &self,
        edge_id: EdgeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.write(|tx| tx.delete_edge_with_valid_time(edge_id, valid_from))
    }

    /// Create a node with an optional [`WriteRequestOptions`](crate::api::transaction::WriteRequestOptions)
    /// bundle: a backdated `valid_from` and/or a write-time [`Provenance`] bundle (Issue #3224).
    ///
    /// Passing `WriteRequestOptions::default()` is identical to [`create_node`](Self::create_node).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder, Provenance};
    /// # use aletheiadb::api::transaction::WriteRequestOptions;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let provenance = Provenance::builder().source("hr-system").confidence(0.95).build()?;
    /// let node_id = db.create_node_with_options(
    ///     "Person",
    ///     PropertyMapBuilder::new().insert("name", "Alice").build(),
    ///     WriteRequestOptions::new().with_provenance(provenance),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_node_with_options(
        &self,
        label: &str,
        properties: PropertyMap,
        options: crate::api::transaction::WriteRequestOptions,
    ) -> Result<NodeId> {
        self.write(|tx| tx.create_node_with_options(label, properties, options))
    }

    /// Create an edge with an optional [`WriteRequestOptions`](crate::api::transaction::WriteRequestOptions)
    /// bundle. See [`create_node_with_options`](Self::create_node_with_options).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_edge_with_options(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        options: crate::api::transaction::WriteRequestOptions,
    ) -> Result<EdgeId> {
        self.write(|tx| tx.create_edge_with_options(source, target, label, properties, options))
    }

    /// Update a node's properties (PATCH semantics) with an optional
    /// [`WriteRequestOptions`](crate::api::transaction::WriteRequestOptions) bundle.
    ///
    /// The provenance recorded here describes *this* version only -- it is
    /// not inherited from the version being updated.
    /// See [`create_node_with_options`](Self::create_node_with_options).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn update_node_with_options(
        &self,
        node_id: NodeId,
        properties: PropertyMap,
        options: crate::api::transaction::WriteRequestOptions,
    ) -> Result<()> {
        self.write(|tx| tx.update_node_with_options(node_id, properties, options))
    }

    /// Update an edge's properties (PATCH semantics) with an optional
    /// [`WriteRequestOptions`](crate::api::transaction::WriteRequestOptions) bundle.
    ///
    /// The provenance recorded here describes *this* version only -- it is
    /// not inherited from the version being updated.
    /// See [`create_node_with_options`](Self::create_node_with_options).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn update_edge_with_options(
        &self,
        edge_id: EdgeId,
        properties: PropertyMap,
        options: crate::api::transaction::WriteRequestOptions,
    ) -> Result<()> {
        self.write(|tx| tx.update_edge_with_options(edge_id, properties, options))
    }

    /// Get the provenance bundle attached to a node's *current* version, if any.
    ///
    /// Returns `Ok(None)` (not an error) if the node has no provenance --
    /// never a fabricated default (Issue #3224).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_provenance(
        &self,
        node_id: NodeId,
    ) -> Result<Option<crate::core::provenance::Provenance>> {
        self.historical
            .read()
            .get_current_node_provenance(node_id)
            .record_error_metric()
    }

    /// Get the provenance bundle attached to an edge's *current* version, if any.
    ///
    /// See [`get_node_provenance`](Self::get_node_provenance) for semantics.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_provenance(
        &self,
        edge_id: EdgeId,
    ) -> Result<Option<crate::core::provenance::Provenance>> {
        self.historical
            .read()
            .get_current_edge_provenance(edge_id)
            .record_error_metric()
    }

    /// Get the provenance bundle attached to a *specific* node version, if any.
    ///
    /// Unlike [`get_node_provenance`](Self::get_node_provenance), this looks
    /// up an exact version rather than re-resolving "whichever version is
    /// current right now". Callers that already hold a [`Node`] snapshot
    /// (e.g. from [`get_node`](Self::get_node)) should pass that snapshot's
    /// `current_version` here to get a consistent, race-free provenance read
    /// for the exact version they already have, rather than risking a
    /// concurrent write shifting "current" between two independent reads.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_version_provenance(
        &self,
        version_id: VersionId,
    ) -> Result<Option<crate::core::provenance::Provenance>> {
        self.historical
            .read()
            .get_node_version_provenance(version_id)
            .record_error_metric()
    }

    /// Get the provenance bundle attached to a *specific* edge version, if any.
    ///
    /// See [`get_node_version_provenance`](Self::get_node_version_provenance)
    /// for semantics.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_version_provenance(
        &self,
        version_id: VersionId,
    ) -> Result<Option<crate::core::provenance::Provenance>> {
        self.historical
            .read()
            .get_edge_version_provenance(version_id)
            .record_error_metric()
    }

    /// Get the current state of a node.
    ///
    /// This uses the fast path (current storage) for O(1) lookup.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node(&self, node_id: NodeId) -> Result<Node> {
        self.current.get_node(node_id).record_error_metric()
    }

    /// Access a node without cloning, executing a closure on the node data.
    ///
    /// This method provides zero-copy read access to node data for hot paths
    /// where only specific fields are needed.
    ///
    /// # Performance
    ///
    /// - **No allocation**: Does not clone the Node
    /// - **No Arc increment**: Does not increment PropertyMap reference count (unless cloned in closure)
    /// - **Lock duration**: Holds DashMap read lock only during closure execution
    ///
    /// # Safety & Deadlocks
    ///
    /// **WARNING**: The closure is executed while holding a read lock on the node shard.
    /// Do NOT attempt to modify the graph or perform operations that might acquire a
    /// write lock on the same shard (e.g., `update_node`, `delete_node`) within the closure.
    /// Doing so will cause a deadlock (lock re-entrancy hazard).
    #[inline]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_node<F, R>(&self, id: NodeId, f: F) -> Result<R>
    where
        F: FnOnce(&Node) -> R,
    {
        self.current.with_node(id, f).record_error_metric()
    }

    /// Get the current state of an edge.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge(&self, edge_id: EdgeId) -> Result<Edge> {
        self.current.get_edge(edge_id).record_error_metric()
    }

    /// Scan all nodes with a specific label, returning an iterator over node IDs.
    ///
    /// This method provides efficient iteration over all nodes matching a given label/type.
    /// Useful for operations that need to process all entities of a certain type.
    ///
    /// # Arguments
    ///
    /// * `label` - The label/type to filter by (e.g., "Person", "Product", "Event")
    ///
    /// # Returns
    ///
    /// An iterator yielding `NodeId` for all nodes with the specified label.
    ///
    /// # Performance
    ///
    /// - **Time**: O(n) scan of all nodes with efficient label filtering
    /// - **Space**: O(1) - lazy iterator, no allocation
    /// - **Comparison**: Uses interned string pointer equality (very fast)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// // Process all Person nodes
    /// for node_id in db.scan_nodes_by_label("Person") {
    ///     let node = db.get_node(node_id)?;
    ///     println!("Person: {:?}", node.properties.get("name"));
    /// }
    ///
    /// // Count nodes by label
    /// let person_count = db.scan_nodes_by_label("Person").count();
    /// let product_count = db.scan_nodes_by_label("Product").count();
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # See Also
    ///
    /// - [`find_nodes_by_property`](Self::find_nodes_by_property) - Find nodes by label + property value
    /// - [`node_count`](Self::node_count) - Total node count across all labels
    pub fn scan_nodes_by_label(&self, label: &str) -> impl Iterator<Item = NodeId> + '_ {
        self.current.scan_nodes_by_label(label)
    }

    // ========================================================================
    // Zero-copy access methods (Issue #190)
    // ========================================================================

    /// Get the target node of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the target NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_target(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.current.get_edge_target(edge_id).record_error_metric()
    }

    /// Get the source node of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the source NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_source(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.current.get_edge_source(edge_id).record_error_metric()
    }

    /// Get outgoing edges from a node (current state).
    pub fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_outgoing_edges(node_id)
    }

    /// Get outgoing edges from a node as an iterator (current state).
    ///
    /// This provides zero-allocation traversal.
    pub fn get_outgoing_edges_iter(&self, node_id: NodeId) -> OutgoingEdgesIter<'_> {
        self.current.get_outgoing_edges_iter(node_id)
    }

    /// Get incoming edges to a node (current state).
    pub fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_incoming_edges(node_id)
    }

    /// Get incoming edges to a node as an iterator (current state).
    ///
    /// This provides zero-allocation traversal.
    pub fn get_incoming_edges_iter(&self, node_id: NodeId) -> IncomingEdgesIter<'_> {
        self.current.get_incoming_edges_iter(node_id)
    }

    /// Get incoming edges with a specific label (current state).
    pub fn get_incoming_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_incoming_edges_with_label(node_id, label)
    }

    /// Get outgoing edges with a specific label (current state).
    pub fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    /// Count the edges connected to a node (both outgoing and incoming).
    ///
    /// This is an additive, non-breaking helper (Issue #3209) that lets callers
    /// learn how many edges reference a node *before* deleting it, so they can
    /// decide whether a detach/cascade delete is required to avoid orphaning
    /// edges.
    ///
    /// Returns [`StorageError::NodeNotFound`](crate::storage::StorageError::NodeNotFound)
    /// if the node does not exist in the current state, so callers never receive
    /// a misleading zero count for a missing node.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn count_connected_edges(&self, node_id: NodeId) -> Result<usize> {
        // Verify the node exists first; an absent node should error rather than
        // silently report zero connected edges.
        let _ = self.current.get_node_label(node_id)?;
        // Use degree counters rather than materializing edge-id vectors: this
        // avoids allocations for high-degree nodes.
        let outgoing = self.current.out_degree(node_id);
        let incoming = self.current.in_degree(node_id);
        Ok(outgoing + incoming)
    }

    /// Get the number of nodes in the current state.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.current.node_count()
    }

    /// Get all node IDs currently in the database.
    ///
    /// Returns a snapshot of all live node IDs. For large graphs prefer
    /// [`scan_nodes_by_label`](Self::scan_nodes_by_label) to avoid loading
    /// the full set into memory.
    #[inline]
    pub fn get_all_node_ids(&self) -> Vec<NodeId> {
        self.current.get_all_node_ids()
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

    /// Find nodes by label and property value (current state).
    ///
    /// Returns the IDs of all nodes with the given label whose specified property
    /// equals the given value.
    #[inline]
    pub fn find_nodes_by_property(
        &self,
        label: &str,
        property_key: &str,
        property_value: &PropertyValue,
    ) -> Vec<NodeId> {
        self.current
            .find_nodes_by_property(label, property_key, property_value)
    }

    /// Return all currently-valid nodes with the given label.
    ///
    /// Useful for pre-flight checks before enabling a uniqueness constraint.
    pub fn get_nodes_by_label(&self, label: &str) -> Vec<Node> {
        self.current.get_nodes_by_label(label)
    }

    // ========================================================================
    // Uniqueness constraint API
    // ========================================================================

    /// Begin building a uniqueness constraint for a label+property pair.
    ///
    /// Call `.enable()` on the returned builder to activate the constraint.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// db.unique_constraint("Person", "email").enable()?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "call .enable() to activate the constraint"]
    pub fn unique_constraint(
        &self,
        label: impl Into<String>,
        property: impl Into<String>,
    ) -> crate::db::constraint_builder::UniqueConstraintBuilder<'_> {
        crate::db::constraint_builder::UniqueConstraintBuilder::new(self, label, property)
    }

    /// Enable a uniqueness constraint on `(label, property)`.
    ///
    /// Fails with [`ConstraintError::DuplicateOnEnable`] if existing nodes already
    /// violate the constraint. No constraint is enabled in that case.
    pub(crate) fn enable_unique_constraint(&self, label: &str, property: &str) -> Result<()> {
        let label_id = GLOBAL_INTERNER.intern(label).record_error_metric()?;
        let prop_id = GLOBAL_INTERNER.intern(property).record_error_metric()?;

        // Pre-flight: scan current nodes for the label and reject if duplicates exist.
        let nodes = self.current.get_nodes_by_label(label);
        crate::core::constraint::ConstraintRegistry::check_no_duplicates(
            &nodes, label_id, prop_id, label, property,
        )?;

        // Populate the reservation index from existing nodes before declaring,
        // so that in-flight transactions observe the full index immediately.
        self.constraint_registry
            .rebuild_from_nodes(&nodes, label_id, prop_id);

        // Persist the declaration to the WAL BEFORE activating it in memory.
        // If the flush fails, we return an error without touching the in-memory
        // registry, so the constraint is never partially active.
        self.wal
            .append(WalOperation::DeclareUniqueConstraint {
                label: label_id,
                property: prop_id,
            })
            .record_error_metric()?;
        self.wal.flush().record_error_metric()?;

        // Record the declaration in the in-memory registry only after it is durable.
        self.constraint_registry.declare(label_id, prop_id);

        Ok(())
    }

    /// Disable a uniqueness constraint on `(label, property)`.
    ///
    /// Existing data is unaffected; the index slot is freed.
    pub fn disable_unique_constraint(&self, label: &str, property: &str) -> Result<()> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Ok(()), // never declared — nothing to do
        };
        let prop_id = match GLOBAL_INTERNER.get_id(property) {
            Some(id) => id,
            None => return Ok(()),
        };

        // Persist the drop to the WAL BEFORE removing it from memory.
        // If the flush fails, we return an error without touching the in-memory
        // registry, so the constraint is never silently lost.
        self.wal
            .append(WalOperation::DropUniqueConstraint {
                label: label_id,
                property: prop_id,
            })
            .record_error_metric()?;
        self.wal.flush().record_error_metric()?;

        self.constraint_registry.undeclare(label_id, prop_id);

        Ok(())
    }

    /// List all active uniqueness constraints as `(label, property)` string pairs.
    pub fn list_unique_constraints(&self) -> Vec<(String, String)> {
        self.constraint_registry.list()
    }
}

#[cfg(test)]
mod tests {
    use crate::PropertyMapBuilder;
    use crate::test_utils::create_test_db;

    #[test]
    fn get_all_node_ids_empty_db() {
        let (_tmp, db) = create_test_db().unwrap();
        assert!(db.get_all_node_ids().is_empty());
    }

    #[test]
    fn get_all_node_ids_returns_created_nodes() {
        let (_tmp, db) = create_test_db().unwrap();
        let a = db
            .create_node("X", PropertyMapBuilder::new().build())
            .unwrap();
        let b = db
            .create_node("X", PropertyMapBuilder::new().build())
            .unwrap();
        let ids = db.get_all_node_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
    }

    mod valid_time_tests {
        use super::*;
        use crate::core::error::{Error, TemporalError};
        use crate::core::hlc::HybridTimestamp;
        use crate::core::property::PropertyValue;
        use crate::core::temporal::time;

        /// `now` is the caller's own `time::now().wallclock()`, captured once at the
        /// start of the test and reused for every offset it computes. Calling
        /// `time::now()` freshly inside the helper would let each call observe a
        /// slightly different instant under load, making relative orderings between
        /// offsets (e.g. `t0 < t1 < t2`) non-deterministic.
        fn hours_ago(now: i64, hours: i64) -> HybridTimestamp {
            HybridTimestamp::new(now - hours * 3_600_000_000, 0).unwrap()
        }

        fn hours_from_now(now: i64, hours: i64) -> HybridTimestamp {
            HybridTimestamp::new(now + hours * 3_600_000_000, 0).unwrap()
        }

        #[test]
        fn create_node_with_valid_time_backdated_round_trip() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_past = hours_ago(now, 1);

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                    Some(t_past),
                )
                .unwrap();

            // Visible at t_past.
            let node = db.get_node_at_valid_time(id, t_past).unwrap();
            assert_eq!(
                node.properties.get("name"),
                Some(&PropertyValue::from("Alice"))
            );

            // Invisible strictly before t_past.
            let before = hours_ago(now, 2);
            assert!(db.get_node_at_valid_time(id, before).is_err());
        }

        #[test]
        fn create_node_with_valid_time_future_dated_invisible_now_visible_at_future() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_future = hours_from_now(now, 1);

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(t_future),
                )
                .unwrap();

            assert!(db.get_node_at_valid_time(id, time::now()).is_err());
            assert!(db.get_node_at_valid_time(id, t_future).is_ok());
        }

        #[test]
        fn create_edge_with_valid_time_backdated_round_trip() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let source = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let target = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();

            let t_past = hours_ago(now, 1);
            let edge_id = db
                .create_edge_with_valid_time(
                    source,
                    target,
                    "KNOWS",
                    PropertyMapBuilder::new().build(),
                    Some(t_past),
                )
                .unwrap();

            assert!(db.get_edge_at_valid_time(edge_id, t_past).is_ok());
            assert!(
                db.get_edge_at_valid_time(edge_id, hours_ago(now, 2))
                    .is_err()
            );
        }

        // NOTE: Updating/deleting closes the *transaction time* of the previous
        // version at commit (standard MVCC on the transaction-time axis), so a
        // valid-time probe strictly between the old and new `valid_from` is not
        // reachable via `get_node_at_valid_time(id, probe)` (which always queries
        // as of the *current* transaction time). This is pre-existing, unmodified
        // `WriteOps` behavior -- verified the same way the transaction-level tests
        // in `api::transaction::write::tests` do: by reading the recorded
        // `valid_from` back from historical storage directly.
        #[test]
        fn update_node_with_valid_time_backdated_round_trip() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("city", "Paris").build(),
                    Some(hours_ago(now, 2)),
                )
                .unwrap();

            let t_update = hours_ago(now, 1);
            db.update_node_with_valid_time(
                id,
                PropertyMapBuilder::new().insert("city", "London").build(),
                Some(t_update),
            )
            .unwrap();

            // The caller-specified valid_from was correctly threaded through.
            let historical = db.historical.read();
            let version_id = historical.get_current_node_version(id).unwrap();
            let version = historical.get_node_version(version_id).unwrap();
            assert_eq!(version.temporal.valid_time().start(), t_update);
            drop(historical);

            // Updated properties are visible from their own valid_from onward.
            let new_state = db.get_node_at_valid_time(id, t_update).unwrap();
            assert_eq!(
                new_state.properties.get("city"),
                Some(&PropertyValue::from("London"))
            );
            let now_state = db.get_node_at_valid_time(id, time::now()).unwrap();
            assert_eq!(
                now_state.properties.get("city"),
                Some(&PropertyValue::from("London"))
            );
        }

        #[test]
        fn update_edge_with_valid_time_backdated_round_trip() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let source = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let target = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let edge_id = db
                .create_edge_with_valid_time(
                    source,
                    target,
                    "KNOWS",
                    PropertyMapBuilder::new().insert("strength", 1i64).build(),
                    Some(hours_ago(now, 2)),
                )
                .unwrap();

            let t_update = hours_ago(now, 1);
            db.update_edge_with_valid_time(
                edge_id,
                PropertyMapBuilder::new().insert("strength", 9i64).build(),
                Some(t_update),
            )
            .unwrap();

            let historical = db.historical.read();
            let version_id = historical.get_current_edge_version(edge_id).unwrap();
            let version = historical.get_edge_version(version_id).unwrap();
            assert_eq!(version.temporal.valid_time().start(), t_update);
            drop(historical);

            let new_state = db.get_edge_at_valid_time(edge_id, t_update).unwrap();
            assert_eq!(
                new_state.properties.get("strength"),
                Some(&PropertyValue::from(9i64))
            );
            let now_state = db.get_edge_at_valid_time(edge_id, time::now()).unwrap();
            assert_eq!(
                now_state.properties.get("strength"),
                Some(&PropertyValue::from(9i64))
            );
        }

        #[test]
        fn delete_node_with_valid_time_round_trip() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(hours_ago(now, 2)),
                )
                .unwrap();

            let t_delete = hours_ago(now, 1);
            db.delete_node_with_valid_time(id, Some(t_delete)).unwrap();

            // Tombstone recorded with the caller-specified valid_from.
            let historical = db.historical.read();
            let version_id = historical.get_current_node_version(id).unwrap();
            let version = historical.get_node_version(version_id).unwrap();
            assert_eq!(version.temporal.valid_time().start(), t_delete);
            drop(historical);

            // No longer visible as of now.
            assert!(db.get_node_at_valid_time(id, time::now()).is_err());
        }

        #[test]
        fn delete_edge_with_valid_time_round_trip() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let source = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let target = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let edge_id = db
                .create_edge_with_valid_time(
                    source,
                    target,
                    "KNOWS",
                    PropertyMapBuilder::new().build(),
                    Some(hours_ago(now, 2)),
                )
                .unwrap();

            let t_delete = hours_ago(now, 1);
            db.delete_edge_with_valid_time(edge_id, Some(t_delete))
                .unwrap();

            let historical = db.historical.read();
            let version_id = historical.get_current_edge_version(edge_id).unwrap();
            let version = historical.get_edge_version(version_id).unwrap();
            assert_eq!(version.temporal.valid_time().start(), t_delete);
            drop(historical);

            assert!(db.get_edge_at_valid_time(edge_id, time::now()).is_err());
        }

        #[test]
        fn create_node_with_valid_time_rejects_far_future_typed_error() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let err = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(hours_from_now(now, 24 * 400)), // > 1 year
                )
                .unwrap_err();

            match err {
                Error::Temporal(TemporalError::ValidTimeTooFarInFuture { .. }) => {}
                other => panic!("Expected ValidTimeTooFarInFuture, got: {other:?}"),
            }
        }

        #[test]
        fn update_node_with_valid_time_rejects_before_creation_typed_error() {
            let (_tmp, db) = create_test_db().unwrap();
            let id = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();

            let way_in_past = HybridTimestamp::new(1000, 0).unwrap();
            let err = db
                .update_node_with_valid_time(
                    id,
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                    Some(way_in_past),
                )
                .unwrap_err();

            match err {
                Error::Temporal(TemporalError::ValidTimeBeforeEntityCreation { .. }) => {}
                other => panic!("Expected ValidTimeBeforeEntityCreation, got: {other:?}"),
            }
        }

        /// Regression test: backfilling a correction between an entity's true creation
        /// time and a later (already backdated) update must succeed through the public
        /// `AletheiaDB` API, not just at the `WriteTransaction` layer. Previously the
        /// "not before creation" floor was computed from the *latest* version instead of
        /// the entity's true original creation time, spuriously rejecting this.
        #[test]
        fn update_node_with_valid_time_backfill_between_existing_versions_succeeds() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t0 = hours_ago(now, 3); // true creation time
            let t2 = hours_ago(now, 2); // later backdated update (becomes latest version)
            let t1 = HybridTimestamp::new(now - 2 * 3_600_000_000 - 1_800_000_000, 0).unwrap(); // 2h30m ago: t0 < t1 < t2

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("city", "Paris").build(),
                    Some(t0),
                )
                .unwrap();
            db.update_node_with_valid_time(
                id,
                PropertyMapBuilder::new().insert("city", "London").build(),
                Some(t2),
            )
            .unwrap();

            // Backfilling between t0 and t2 must succeed, not be spuriously rejected
            // against t2 (the latest version) instead of t0 (the true creation time).
            let result = db.update_node_with_valid_time(
                id,
                PropertyMapBuilder::new().insert("city", "Berlin").build(),
                Some(t1),
            );
            assert!(
                result.is_ok(),
                "Backfill between existing versions should succeed, got: {result:?}"
            );
        }

        #[test]
        fn transaction_time_is_system_assigned_for_backdated_create() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_past = hours_ago(now, 1);
            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(t_past),
                )
                .unwrap();

            // At t_past, the write had not happened yet transactionally.
            assert!(db.get_node_at_transaction_time(id, t_past).is_err());
            // At now, the write is visible (transaction time == commit time, near now).
            assert!(db.get_node_at_transaction_time(id, time::now()).is_ok());
        }

        #[test]
        fn create_node_plain_delegates_with_none_and_is_valid_now() {
            let (_tmp, db) = create_test_db().unwrap();
            let id = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            assert!(db.get_node_at_valid_time(id, time::now()).is_ok());
        }

        #[test]
        fn create_edge_plain_delegates_with_none_and_is_valid_now() {
            let (_tmp, db) = create_test_db().unwrap();
            let source = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let target = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let edge_id = db
                .create_edge(source, target, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap();
            assert!(db.get_edge_at_valid_time(edge_id, time::now()).is_ok());
        }
    }
}
