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

    /// Retract a node as of valid time `valid_to` (Issue #3230): close its
    /// valid-time interval at `valid_to` without deleting its history.
    ///
    /// After retraction, `AS OF VALID_TIME` queries strictly before
    /// `valid_to` still return the node; queries at or after `valid_to` do
    /// not. `AS OF SYSTEM_TIME` queries positioned before the retraction's
    /// commit still show the fact as open-ended (append-only — retraction
    /// never rewrites the past record). The node is absent from
    /// current-state queries, like a delete, while its full history remains
    /// queryable via [`get_node_history`](Self::get_node_history).
    ///
    /// # Referential safety (mirrors the #3209 DETACH contract)
    ///
    /// If the node has connected edges, this method **refuses** with a
    /// [`TransactionError::ValidationFailed`](crate::core::error::TransactionError::ValidationFailed)
    /// naming the connected-edge count — it never silently strands edges
    /// pointing at a retracted node. Use
    /// [`retract_node_detach`](Self::retract_node_detach) to co-retract the
    /// connected edges at the same `valid_to`.
    ///
    /// # Idempotency
    ///
    /// Retracting an already-retracted (or deleted) node is a no-op that
    /// returns the *existing* `valid_to` with `already_retracted: true`.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TemporalError`](crate::core::error::TemporalError)
    /// if `valid_to` precedes the node's `valid_from` (equality is allowed,
    /// yielding an empty interval) or is more than one year in the future;
    /// a nonexistent node is
    /// [`StorageError::NodeNotFound`](crate::core::error::StorageError::NodeNotFound).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn retract_node(
        &self,
        node_id: NodeId,
        valid_to: Timestamp,
    ) -> Result<crate::api::transaction::RetractionResult> {
        // The connected-edge check and the retraction run inside a single
        // write transaction so they observe the same storage state — no
        // check-then-act gap for a concurrent writer to slip an edge into
        // (same rationale as the MCP delete_node handler, Issue #3209).
        self.write(|tx| {
            if self.current.get_node(node_id).is_ok() {
                let connected_edges = self.count_connected_edges(node_id)?;
                if connected_edges > 0 {
                    return Err(crate::core::error::TransactionError::ValidationFailed {
                        reason: format!(
                            "Node {} has {} connected edge(s); refusing to retract. \
                             Use retract_node_detach to co-retract the connected edges \
                             at the same valid time, or retract the edges first.",
                            node_id.as_u64(),
                            connected_edges
                        ),
                    }
                    .into());
                }
            }
            tx.retract_node(node_id, valid_to)
        })
    }

    /// Retract a node and co-retract every connected edge at the same
    /// `valid_to` (Issue #3230) — the retraction analog of
    /// `DETACH DELETE` / [`delete_node_cascade`](crate::api::transaction::WriteOps::delete_node_cascade).
    ///
    /// The returned
    /// [`RetractionResult::edges_retracted`](crate::api::transaction::RetractionResult::edges_retracted)
    /// reports how many edges were newly retracted alongside the node
    /// (edges already retracted beforehand are not double-counted).
    ///
    /// See [`retract_node`](Self::retract_node) for the bi-temporal
    /// semantics, idempotency, and validation errors. Note that every
    /// co-retracted edge is validated too: `valid_to` must not precede any
    /// connected edge's own `valid_from`.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn retract_node_detach(
        &self,
        node_id: NodeId,
        valid_to: Timestamp,
    ) -> Result<crate::api::transaction::RetractionResult> {
        self.write(|tx| {
            use crate::api::transaction::ReadOps;
            // Enumerate connected edges INSIDE the write transaction (same
            // no-check-then-act rationale as retract_node). Deduplicate so a
            // self-loop (present in both adjacency directions) is retracted
            // once.
            let mut edge_ids = tx.get_outgoing_edges(node_id);
            edge_ids.extend(tx.get_incoming_edges(node_id));
            edge_ids.sort_unstable();
            edge_ids.dedup();

            let mut edges_retracted = 0;
            for edge_id in edge_ids {
                let edge_result = tx.retract_edge(edge_id, valid_to)?;
                if !edge_result.already_retracted {
                    edges_retracted += 1;
                }
            }

            let mut result = tx.retract_node(node_id, valid_to)?;
            result.edges_retracted = edges_retracted;
            Ok(result)
        })
    }

    /// Retract an edge as of valid time `valid_to` (Issue #3230): close its
    /// valid-time interval at `valid_to` without deleting its history.
    ///
    /// See [`retract_node`](Self::retract_node) for the bi-temporal
    /// semantics, idempotency, and validation errors (edges have no
    /// connected-entity concern, so there is no refusing/detach split).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn retract_edge(
        &self,
        edge_id: EdgeId,
        valid_to: Timestamp,
    ) -> Result<crate::api::transaction::RetractionResult> {
        self.write(|tx| tx.retract_edge(edge_id, valid_to))
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

    /// Valid-time retraction tests (Issue #3230).
    ///
    /// Retraction closes an entity's valid-time interval at `T` without
    /// deleting its history: `AS OF VALID_TIME < T` still shows the fact,
    /// `>= T` does not, and `AS OF SYSTEM_TIME` before the retraction's
    /// commit still shows the fact as open-ended (append-only, never
    /// rewrite).
    mod retraction_tests {
        use super::*;
        use crate::core::error::{Error, TemporalError, TransactionError};
        use crate::core::hlc::HybridTimestamp;
        use crate::core::id::NodeId;
        use crate::core::temporal::time;

        fn hours_ago(now: i64, hours: i64) -> HybridTimestamp {
            HybridTimestamp::new(now - hours * 3_600_000_000, 0).unwrap()
        }

        fn hours_from_now(now: i64, hours: i64) -> HybridTimestamp {
            HybridTimestamp::new(now + hours * 3_600_000_000, 0).unwrap()
        }

        /// AC #1: retract node at T -> visible strictly before T, not
        /// visible at exactly T (half-open boundary), not visible after T.
        #[test]
        fn retract_node_before_at_after_valid_time_boundary() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_create = hours_ago(now, 3);
            let t_retract = hours_ago(now, 1);

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                    Some(t_create),
                )
                .unwrap();

            let result = db.retract_node(id, t_retract).unwrap();
            assert!(!result.already_retracted);
            assert_eq!(result.valid_from, t_create);
            assert_eq!(result.valid_to, t_retract);
            assert_eq!(result.edges_retracted, 0);

            // Visible strictly before T.
            assert!(db.get_node_at_valid_time(id, hours_ago(now, 2)).is_ok());
            // NOT visible at exactly T (half-open [valid_from, T)).
            assert!(db.get_node_at_valid_time(id, t_retract).is_err());
            // NOT visible after T.
            assert!(db.get_node_at_valid_time(id, time::now()).is_err());
            // Gone from current state.
            assert!(db.get_node(id).is_err());
        }

        /// AC #2: history preserved — every prior version present, and the
        /// closed interval is visible as a distinct historical state.
        #[test]
        fn retract_node_history_preserves_versions_and_shows_closed_interval() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_create = hours_ago(now, 3);
            let t_update = hours_ago(now, 2);
            let t_retract = hours_ago(now, 1);

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("city", "Paris").build(),
                    Some(t_create),
                )
                .unwrap();
            db.update_node_with_valid_time(
                id,
                PropertyMapBuilder::new().insert("city", "London").build(),
                Some(t_update),
            )
            .unwrap();

            db.retract_node(id, t_retract).unwrap();

            let history = db.get_node_history(id).unwrap();
            assert_eq!(
                history.version_count(),
                3,
                "create + update + retraction = 3 versions, zero loss"
            );

            // v1: [t_create, t_update) — closed by the update.
            let v1 = &history.versions[0];
            assert_eq!(v1.temporal.valid_time().start(), t_create);
            assert_eq!(v1.temporal.valid_time().end(), t_update);

            // v2: the pre-retraction head. Its VALID interval must remain
            // open-ended (append-only: retraction never rewrites the past
            // record), while its TRANSACTION time is closed by the
            // retraction commit.
            let v2 = &history.versions[1];
            assert_eq!(v2.temporal.valid_time().start(), t_update);
            assert!(
                v2.temporal.valid_time().is_current(),
                "pre-retraction head's valid interval must stay open-ended"
            );
            assert!(
                !v2.temporal.transaction_time().is_current(),
                "pre-retraction head's transaction time must be closed"
            );

            // v3: the retraction version — closed valid interval
            // [t_update, t_retract), open transaction time.
            let v3 = &history.versions[2];
            assert_eq!(v3.temporal.valid_time().start(), t_update);
            assert_eq!(v3.temporal.valid_time().end(), t_retract);
            assert!(v3.temporal.transaction_time().is_current());
        }

        /// AC #3: AS OF SYSTEM_TIME positioned before the retraction's
        /// transaction time still shows the fact as currently valid
        /// (open-ended); positioned after, the valid interval is closed.
        #[test]
        fn retract_node_as_of_system_time_before_retraction_shows_open_ended() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_create = hours_ago(now, 3);
            let t_retract = hours_ago(now, 1);

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(t_create),
                )
                .unwrap();
            // A system-time coordinate strictly after the create commit and
            // strictly before the retraction commit.
            let tx_before_retraction = time::now();

            let (_, tx_retraction) = db
                .write_with_timestamp(|tx| tx.retract_node(id, t_retract))
                .unwrap();

            // Before the retraction was recorded, the fact was believed
            // valid — even at valid times >= T.
            assert!(
                db.get_node_at_time(id, time::now(), tx_before_retraction)
                    .is_ok(),
                "AS OF SYSTEM_TIME before retraction must show the fact open-ended"
            );

            // After the retraction was recorded: valid < T still visible,
            // valid >= T not.
            assert!(
                db.get_node_at_time(id, hours_ago(now, 2), tx_retraction)
                    .is_ok()
            );
            assert!(db.get_node_at_time(id, time::now(), tx_retraction).is_err());
        }

        /// AC (idempotency): re-retracting returns the EXISTING valid_to,
        /// even when the second call passes a different T; no new version.
        #[test]
        fn double_retract_is_idempotent_and_returns_original_valid_to() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t1 = hours_ago(now, 2);
            let t2 = hours_ago(now, 1);

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(hours_ago(now, 3)),
                )
                .unwrap();

            let first = db.retract_node(id, t1).unwrap();
            assert!(!first.already_retracted);
            assert_eq!(first.valid_to, t1);
            let versions_after_first = db.get_node_history(id).unwrap().version_count();

            // Second retraction with a DIFFERENT T: no-op, original end.
            let second = db.retract_node(id, t2).unwrap();
            assert!(second.already_retracted);
            assert_eq!(second.valid_to, t1, "must return the existing valid_to");

            assert_eq!(
                db.get_node_history(id).unwrap().version_count(),
                versions_after_first,
                "idempotent re-retract must not append a version"
            );
        }

        /// AC (validation): T earlier than valid_from is rejected; T equal
        /// to valid_from succeeds (empty interval); far-future T rejected;
        /// nonexistent entity is not-found.
        #[test]
        fn retract_node_validation_boundaries() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_create = hours_ago(now, 2);

            let id = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(t_create),
                )
                .unwrap();

            // T < valid_from: clear typed error.
            let err = db.retract_node(id, hours_ago(now, 3)).unwrap_err();
            match err {
                Error::Temporal(TemporalError::ValidTimeBeforeEntityCreation { .. }) => {}
                other => panic!("Expected ValidTimeBeforeEntityCreation, got: {other:?}"),
            }

            // Far-future T: rejected.
            let err = db
                .retract_node(id, hours_from_now(now, 24 * 400))
                .unwrap_err();
            match err {
                Error::Temporal(TemporalError::ValidTimeTooFarInFuture { .. }) => {}
                other => panic!("Expected ValidTimeTooFarInFuture, got: {other:?}"),
            }

            // T == valid_from: allowed, yields an empty interval — the node
            // was never valid at any instant.
            let result = db.retract_node(id, t_create).unwrap();
            assert_eq!(result.valid_from, t_create);
            assert_eq!(result.valid_to, t_create);
            assert!(db.get_node_at_valid_time(id, t_create).is_err());

            // Nonexistent entity: not-found.
            let missing = NodeId::new(999_999).unwrap();
            assert!(db.retract_node(missing, time::now()).is_err());
        }

        /// AC (referential safety): plain retract_node refuses when the node
        /// has connected edges; retract_node_detach co-retracts them at the
        /// same T and reports edges_retracted.
        #[test]
        fn retract_node_refuses_with_connected_edges_and_detach_coretracts() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_create = hours_ago(now, 3);
            let t_retract = hours_ago(now, 1);

            let a = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(t_create),
                )
                .unwrap();
            let b = db
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().build(),
                    Some(t_create),
                )
                .unwrap();
            let edge_id = db
                .create_edge_with_valid_time(
                    a,
                    b,
                    "KNOWS",
                    PropertyMapBuilder::new().build(),
                    Some(t_create),
                )
                .unwrap();

            // Refuse-by-default: the error names the connected-edge count.
            let err = db.retract_node(a, t_retract).unwrap_err();
            match &err {
                Error::Transaction(TransactionError::ValidationFailed { reason }) => {
                    assert!(reason.contains("1 connected edge"), "got: {reason}");
                }
                other => panic!("Expected ValidationFailed refusal, got: {other:?}"),
            }
            // Nothing was retracted by the refusal.
            assert!(db.get_node(a).is_ok());
            assert!(db.get_edge(edge_id).is_ok());

            // Detach form co-retracts the edge at the same T.
            let result = db.retract_node_detach(a, t_retract).unwrap();
            assert!(!result.already_retracted);
            assert_eq!(result.edges_retracted, 1);

            // Edge queryable strictly before T, gone at/after T.
            assert!(
                db.get_edge_at_valid_time(edge_id, hours_ago(now, 2))
                    .is_ok()
            );
            assert!(db.get_edge_at_valid_time(edge_id, t_retract).is_err());
            assert!(db.get_edge_at_valid_time(edge_id, time::now()).is_err());
            // Node likewise.
            assert!(db.get_node_at_valid_time(a, hours_ago(now, 2)).is_ok());
            assert!(db.get_node_at_valid_time(a, time::now()).is_err());
        }

        /// AC: retract_edge direct — same before/at/after matrix, plus
        /// idempotency and history preservation.
        #[test]
        fn retract_edge_before_at_after_matrix() {
            let (_tmp, db) = create_test_db().unwrap();
            let now = time::now().wallclock();
            let t_create = hours_ago(now, 3);
            let t_retract = hours_ago(now, 1);

            let a = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let b = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let edge_id = db
                .create_edge_with_valid_time(
                    a,
                    b,
                    "KNOWS",
                    PropertyMapBuilder::new().build(),
                    Some(t_create),
                )
                .unwrap();

            let result = db.retract_edge(edge_id, t_retract).unwrap();
            assert!(!result.already_retracted);
            assert_eq!(result.valid_from, t_create);
            assert_eq!(result.valid_to, t_retract);

            assert!(
                db.get_edge_at_valid_time(edge_id, hours_ago(now, 2))
                    .is_ok()
            );
            assert!(db.get_edge_at_valid_time(edge_id, t_retract).is_err());
            assert!(db.get_edge_at_valid_time(edge_id, time::now()).is_err());
            assert!(db.get_edge(edge_id).is_err());

            // History: create + retraction version, closed interval visible.
            let history = db.get_edge_history(edge_id).unwrap();
            assert_eq!(history.version_count(), 2);
            let head = &history.versions[1];
            assert_eq!(head.temporal.valid_time().start(), t_create);
            assert_eq!(head.temporal.valid_time().end(), t_retract);

            // Idempotent re-retract returns the original end.
            let second = db.retract_edge(edge_id, hours_ago(now, 2)).unwrap();
            assert!(second.already_retracted);
            assert_eq!(second.valid_to, t_retract);
        }

        /// AC (crash recovery): after a WAL replay into a fresh database,
        /// the full before/at/after + history matrix still holds, honoring
        /// the retraction's valid_to faithfully.
        #[test]
        fn retraction_survives_wal_replay_crash_recovery() {
            use crate::config::WalConfigBuilder;
            use crate::storage::wal::DurabilityMode;
            use crate::{AletheiaDB, AletheiaDBConfig};

            let temp_dir = tempfile::tempdir().unwrap();
            let wal_dir = temp_dir.path().join("wal");
            let make_config = || {
                AletheiaDBConfig::builder()
                    .wal(
                        WalConfigBuilder::new()
                            .wal_dir(wal_dir.clone())
                            .durability_mode(DurabilityMode::Synchronous)
                            .build(),
                    )
                    .build()
            };

            let now = time::now().wallclock();
            let t_create = hours_ago(now, 3);
            let t_retract = hours_ago(now, 1);

            let (node_id, edge_id, other_id);
            {
                let db = AletheiaDB::with_unified_config(make_config()).unwrap();
                node_id = db
                    .create_node_with_valid_time(
                        "Person",
                        PropertyMapBuilder::new().insert("name", "Alice").build(),
                        Some(t_create),
                    )
                    .unwrap();
                other_id = db
                    .create_node_with_valid_time(
                        "Person",
                        PropertyMapBuilder::new().build(),
                        Some(t_create),
                    )
                    .unwrap();
                edge_id = db
                    .create_edge_with_valid_time(
                        node_id,
                        other_id,
                        "KNOWS",
                        PropertyMapBuilder::new().build(),
                        Some(t_create),
                    )
                    .unwrap();
                let result = db.retract_node_detach(node_id, t_retract).unwrap();
                assert_eq!(result.edges_retracted, 1);
                // Simulate a crash: drop without a checkpoint.
            }

            // "Restart": recovery replays the WAL into fresh storage.
            let db = AletheiaDB::with_unified_config(make_config()).unwrap();

            // Current state: retracted entities are absent.
            assert!(db.get_node(node_id).is_err());
            assert!(db.get_edge(edge_id).is_err());
            assert!(db.get_node(other_id).is_ok());

            // Valid-time matrix, honoring the retraction's backdated T
            // (NOT the replay/commit time).
            assert!(
                db.get_node_at_valid_time(node_id, hours_ago(now, 2))
                    .is_ok()
            );
            assert!(db.get_node_at_valid_time(node_id, t_retract).is_err());
            assert!(db.get_node_at_valid_time(node_id, time::now()).is_err());
            assert!(
                db.get_edge_at_valid_time(edge_id, hours_ago(now, 2))
                    .is_ok()
            );
            assert!(db.get_edge_at_valid_time(edge_id, t_retract).is_err());
            assert!(db.get_edge_at_valid_time(edge_id, time::now()).is_err());

            // History: create + retraction versions, closed interval intact.
            let history = db.get_node_history(node_id).unwrap();
            assert_eq!(history.version_count(), 2);
            let head = &history.versions[1];
            assert_eq!(head.temporal.valid_time().start(), t_create);
            assert_eq!(
                head.temporal.valid_time().end(),
                t_retract,
                "replay must honor the retraction's valid_to faithfully"
            );

            // Idempotency survives recovery too.
            let again = db.retract_node(node_id, time::now()).unwrap();
            assert!(again.already_retracted);
            assert_eq!(again.valid_to, t_retract);
        }
    }
}
