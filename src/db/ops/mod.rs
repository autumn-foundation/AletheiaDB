//! Basic graph operations for creating and reading nodes and edges.
//!
//! Contains convenience methods for the most common graph operations.
use crate::api::transaction::WriteOps;
use crate::core::error::{Result, ResultExt};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use crate::db::AletheiaDB;
use crate::storage::current::{IncomingEdgesIter, OutgoingEdgesIter};
use crate::storage::wal::WalOperation;

/// How many candidate node ids the AS OF node-find scan reconstructs per
/// historical read-guard acquisition (Issue #3236). Chunking keeps cold-tier
/// I/O during property reconstruction from pinning the guard -- and thereby
/// blocking writers -- for the whole scan.
const AS_OF_FIND_CHUNK_SIZE: usize = 4096;

/// Result of a point-in-time (AS OF) node find (Issue #3236): the matching
/// nodes plus a disclosure flag for candidate-set truncation.
///
/// Returned by [`AletheiaDB::find_nodes_at_time`] and
/// [`AletheiaDB::find_nodes_by_property_at`].
#[derive(Debug, Clone, Default)]
pub struct NodesAtTime {
    /// Matching nodes, reconstructed at the queried bi-temporal coordinate
    /// and sorted by node id (so pagination over the set is deterministic).
    pub nodes: Vec<Node>,
    /// `true` when the candidate set (every node that has ever had a version
    /// recorded) exceeded the configured cap
    /// ([`crate::config::HistoricalConfigBuilder::max_schema_as_of_entities`],
    /// default 50,000) and was truncated to the lowest `cap` node ids before
    /// scanning. When set, `nodes` -- and any count derived from it -- may
    /// be incomplete: they reflect only the sampled candidate set. Mirrors
    /// [`GraphSchema::sampled`](crate::db::schema::GraphSchema::sampled).
    pub sampled: bool,
}

impl NodesAtTime {
    /// An empty, un-truncated result (e.g. for a never-interned label).
    fn empty() -> Self {
        Self::default()
    }
}

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
    /// Note that this holds even when `valid_to` is in the **future**:
    /// current state reflects post-retraction *knowledge*, not post-`T`
    /// reality. A node retracted at `now + 1h` is absent from current-state
    /// queries immediately (parity with
    /// [`delete_node_with_valid_time`](Self::delete_node_with_valid_time)),
    /// even though `get_node_at_valid_time(id, now)` still returns it.
    ///
    /// # Referential safety (mirrors the #3209 DETACH contract)
    ///
    /// If the node has connected edges, this method **refuses** with a
    /// [`TransactionError::ValidationFailed`](crate::core::error::TransactionError::ValidationFailed)
    /// naming the connected-edge count (**distinct** edges — a self-loop
    /// counts once, matching what the detach form would retract) — it never
    /// silently strands edges pointing at a retracted node. Use
    /// [`retract_node_detach`](Self::retract_node_detach) to co-retract the
    /// connected edges at the same `valid_to`.
    ///
    /// # Idempotency
    ///
    /// Retracting an already-retracted (or deleted) node is a no-op that
    /// returns the *existing* `valid_to` with `already_retracted: true`.
    /// For a node that was **deleted** (rather than retracted), the
    /// returned interval is the tombstone's degenerate `[d, d)` — both
    /// bounds equal the deletion's valid time, not the node's original
    /// `valid_from`.
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
            use crate::api::transaction::ReadOps;
            if tx.get_node(node_id).is_ok() {
                // Count DISTINCT connected edges with the same sort/dedup
                // enumeration the detach form uses, so the refusal count
                // always equals what retract_node_detach would retract (a
                // self-loop appears in both adjacency directions but is one
                // edge — count_connected_edges would report 2).
                let mut edge_ids = tx.get_outgoing_edges(node_id)?;
                edge_ids.extend(tx.get_incoming_edges(node_id)?);
                edge_ids.sort_unstable();
                edge_ids.dedup();
                let connected_edges = edge_ids.len();
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
    /// connected edge's own `valid_from`. If ANY connected edge fails that
    /// check, the whole detach **fails atomically** — neither the node nor
    /// any edge is retracted; retract the offending edge separately at a
    /// later `valid_to` first.
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
            // A node that is not visible (e.g. already retracted or never
            // created) has no enumerable edges; `retract_node` below still
            // owns the idempotency / not-found semantics for that case, so a
            // NodeNotFound here must not fail the detach early (Issue #359).
            let mut edge_ids = tx.get_outgoing_edges(node_id).unwrap_or_default();
            edge_ids.extend(tx.get_incoming_edges(node_id).unwrap_or_default());
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
    /// As with nodes, a **future** `valid_to` removes the edge from
    /// current-state queries immediately — current state reflects
    /// post-retraction knowledge, not post-`T` reality — while
    /// `get_edge_at_valid_time` before `valid_to` keeps returning it.
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

    /// Get the provenance bundle and bi-temporal interval of a *specific*
    /// node version in a single historical-storage read (Issue #3232).
    ///
    /// Like [`get_node_version_provenance`](Self::get_node_version_provenance),
    /// this resolves the exact version already captured in a [`Node`]
    /// snapshot's `current_version` -- which the point-in-time read paths set
    /// to the matched historical version -- so the returned interval reflects
    /// the version the caller is actually holding, for current-state and
    /// at-time reads alike.
    ///
    /// Returns `Ok(None)` when the version cannot be found in any tier.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_version_read_metadata(
        &self,
        version_id: VersionId,
    ) -> Result<
        Option<(
            Option<crate::core::provenance::Provenance>,
            BiTemporalInterval,
        )>,
    > {
        self.historical
            .read()
            .get_node_version_read_metadata(version_id)
            .record_error_metric()
    }

    /// Edge counterpart of
    /// [`get_node_version_read_metadata`](Self::get_node_version_read_metadata).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_version_read_metadata(
        &self,
        version_id: VersionId,
    ) -> Result<
        Option<(
            Option<crate::core::provenance::Provenance>,
            BiTemporalInterval,
        )>,
    > {
        self.historical
            .read()
            .get_edge_version_read_metadata(version_id)
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

    /// Find nodes by label as of a bi-temporal point (AS OF scan, Issue #3236).
    ///
    /// Returns every node with the given label **as it existed** at
    /// `(valid_time, transaction_time)`, with properties reconstructed from
    /// the historical version visible at that coordinate. Nodes that did not
    /// exist at the queried point are excluded; nodes since deleted from
    /// current state are still found when both dimensions anchor before the
    /// deletion.
    ///
    /// Results are sorted by node ID so pagination over the result set is
    /// deterministic. A label that has never been written yields an empty
    /// result, never an error.
    ///
    /// # Completeness and the candidate cap
    ///
    /// Candidates are every node that has ever had a version recorded (the
    /// same enumeration [`schema_as_of`](Self::schema_as_of) uses), capped
    /// at the same configurable limit
    /// ([`crate::config::HistoricalConfigBuilder::max_schema_as_of_entities`],
    /// default 50,000) to keep a single call bounded on databases with
    /// substantial bi-temporal history. When the cap is hit, the lowest
    /// `cap` node ids are kept (a deterministic set, so pagination stays
    /// stable) and [`NodesAtTime::sampled`] is `true` to disclose that the
    /// result -- including its length -- reflects only the sampled
    /// candidate set.
    ///
    /// # Performance
    ///
    /// Version-at-time resolution runs for every candidate, but property
    /// reconstruction runs only for candidates whose at-time label matches.
    /// A dedicated temporal label index is a deliberate follow-up if this
    /// path proves too slow at scale.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_nodes_at_time(
        &self,
        label: &str,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<NodesAtTime> {
        self.find_nodes_at_time_filtered(label, None, valid_time, transaction_time)
    }

    /// Find nodes by label and exact property match as of a bi-temporal
    /// point (Issue #3236).
    ///
    /// The bi-temporal counterpart of
    /// [`find_nodes_by_property`](Self::find_nodes_by_property): the property
    /// comparison runs against each node's state **at**
    /// `(valid_time, transaction_time)`, so a node whose property only
    /// matched before (or after) the queried point is excluded.
    ///
    /// With both dimensions at the current time this returns the same node
    /// set as `find_nodes_by_property` **for nodes whose valid interval has
    /// begun**. The one divergence is future-dated facts: a node created
    /// with a `valid_from` in the future is already present in current
    /// storage (and thus in `find_nodes_by_property`) but is not yet visible
    /// at `(now, now)` in the bi-temporal view, so this method excludes it
    /// until its valid time arrives.
    ///
    /// See [`find_nodes_at_time`](Self::find_nodes_at_time) for ordering,
    /// completeness (including the candidate cap and
    /// [`NodesAtTime::sampled`]), and performance characteristics.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_nodes_by_property_at(
        &self,
        label: &str,
        property_key: &str,
        property_value: &PropertyValue,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<NodesAtTime> {
        self.find_nodes_at_time_filtered(
            label,
            Some((property_key, property_value)),
            valid_time,
            transaction_time,
        )
    }

    /// Shared implementation for the AS OF node-find methods: enumerate every
    /// node that has ever had a version recorded (capped, see
    /// [`find_nodes_at_time`](Self::find_nodes_at_time)), resolve each
    /// candidate's version at the queried bi-temporal coordinate, and
    /// reconstruct properties only for those whose *at-time* label matches,
    /// then apply the optional exact-property filter.
    fn find_nodes_at_time_filtered(
        &self,
        label: &str,
        property_filter: Option<(&str, &PropertyValue)>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<NodesAtTime> {
        // A label (or property key) that has never been interned has never
        // been written anywhere -- current or historical -- so the result is
        // an empty set, never an error (mirroring `find_nodes_by_property`).
        let Some(label_id) = GLOBAL_INTERNER.get_id(label) else {
            return Ok(NodesAtTime::empty());
        };
        let key_filter = match property_filter {
            Some((key, value)) => match GLOBAL_INTERNER.get_id(key) {
                Some(key_id) => Some((key_id, value)),
                None => return Ok(NodesAtTime::empty()),
            },
            None => None,
        };

        // Candidate set: every node that has ever had a version recorded.
        // Unlike the current-state label index, this stays complete for nodes
        // deleted from current state (deletion closes the version's
        // transaction interval but the version head remains), so anchoring
        // both dimensions before a deletion still recalls the node.
        //
        // The set is capped at the same configurable limit `schema_as_of`
        // uses, keeping the lowest `cap` ids -- a deterministic subset, so
        // pagination over a sampled result is still stable. Sorted so the
        // result order (and thus pagination) is deterministic.
        let (node_ids, sampled) = {
            let historical = self.historical.read();
            let mut ids = historical.versioned_node_ids();
            let cap = historical.max_schema_as_of_entities();
            drop(historical);
            let sampled = crate::db::schema::cap_ids(&mut ids, cap);
            ids.sort_unstable();
            (ids, sampled)
        };

        // Reconstruct in chunks, re-acquiring the historical read guard per
        // chunk, so cold-tier I/O during property reconstruction cannot pin
        // the guard (and block writers) for the entire scan. Recorded
        // history is immutable and every candidate is resolved at the same
        // fixed bi-temporal coordinate, so for a past `transaction_time`
        // anchor the chunked scan is exactly as consistent as a
        // single-acquisition scan. The only skew window is a
        // `transaction_time` at/near now: a write committed between chunks
        // can be visible to later chunks but not earlier ones -- the same
        // race a caller already has against writes committed immediately
        // before or after a single-acquisition call.
        let mut matches = Vec::new();
        for chunk in node_ids.chunks(AS_OF_FIND_CHUNK_SIZE) {
            let chunk_nodes = self
                .historical
                .read()
                .get_nodes_at_time_with_label(chunk, label_id, valid_time, transaction_time)
                .record_error_metric()?;
            for node in chunk_nodes {
                if let Some((key_id, expected)) = &key_filter
                    && !node
                        .properties
                        .get_by_interned_key(key_id)
                        .is_some_and(|v| v == *expected)
                {
                    continue;
                }
                matches.push(node);
            }
        }
        Ok(NodesAtTime {
            nodes: matches,
            sampled,
        })
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
mod tests;
