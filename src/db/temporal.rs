//! Temporal query operations for bi-temporal data.
//!
//! Methods for querying historical states of the graph using valid and transaction times.
use crate::core::changefeed::{
    ChangeCursor, ChangeFeedPage, ChangeFeedQuery, ChangeRecord, RawChange,
};
use crate::core::error::{Result, ResultExt};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::{TimeRange, Timestamp, time};
use crate::db::AletheiaDB;
use crate::query::{EntityHistory, VersionDiff};

#[cfg(test)]
thread_local! {
    /// Test-only instrumentation: the number of candidate [`RawChange`] rows the most recent
    /// [`AletheiaDB::list_changes`] call **on this thread** held for pagination — i.e. the
    /// "materialize / refilter" working set the filter+limit pushdown is meant to bound. Parity
    /// and bench tests read this to assert the hot/cold pushdown actually shrinks the working set
    /// to `O(page)` instead of `O(total matches)`.
    ///
    /// It is **thread-local**, not a process-global static, on purpose: libtest runs each test on
    /// its own thread and `list_changes` records the count synchronously on that same thread, so a
    /// test reads exactly the count from its own call. A shared mutable global would race under
    /// default `cargo test` parallelism (a concurrent test's call could clobber the value between
    /// this test's production call and its read), producing both false reds and false greens.
    pub(crate) static LAST_LIST_CHANGES_CANDIDATES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

impl AletheiaDB {
    /// Get outgoing edges from a node at a specific point in time.
    ///
    /// This method uses the temporal adjacency index to efficiently find all
    /// edges that were valid at the specified time, including edges that have
    /// been deleted in current storage.
    ///
    /// # Arguments
    ///
    /// * `source` - The source node ID
    /// * `valid_time` - The valid time to query
    /// * `tx_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of edge IDs that were valid at the specified time. Returns an
    /// empty vector if no temporal adjacency index is configured.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::AletheiaDB;
    /// use aletheiadb::core::temporal::time;
    ///
    /// let db = AletheiaDB::new().unwrap();
    /// // ... create and delete edges ...
    /// let edges = db.get_outgoing_edges_at_time(node_id, valid_time, tx_time);
    /// ```
    pub fn get_outgoing_edges_at_time(
        &self,
        source: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        self.historical
            .read()
            .get_outgoing_edges_at_time(source, valid_time, tx_time)
    }

    /// Get incoming edges to a node at a specific point in time.
    ///
    /// This method uses the temporal adjacency index to efficiently find all
    /// edges that were valid at the specified time, including edges that have
    /// been deleted in current storage.
    ///
    /// # Arguments
    ///
    /// * `target` - The target node ID
    /// * `valid_time` - The valid time to query
    /// * `tx_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of edge IDs that were valid at the specified time. Returns an
    /// empty vector if no temporal adjacency index is configured.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::AletheiaDB;
    /// use aletheiadb::core::temporal::time;
    ///
    /// let db = AletheiaDB::new().unwrap();
    /// // ... create and delete edges ...
    /// let edges = db.get_incoming_edges_at_time(node_id, valid_time, tx_time);
    /// ```
    pub fn get_incoming_edges_at_time(
        &self,
        target: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        self.historical
            .read()
            .get_incoming_edges_at_time(target, valid_time, tx_time)
    }

    /// Get a node as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility with historical storage (handles closed intervals from deletions).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::temporal_query_span("get_node_at_time").entered();

        let node = self
            .historical
            .read()
            .get_node_at_time(node_id, valid_time, transaction_time)
            .record_error_metric()?;
        // GDPR crypto-shred (Issue #3359, PR-1b): unseal designated properties on
        // the point-in-time read boundary. No-op unless the DB has designations.
        #[cfg(feature = "audit-export")]
        let node = self.unseal_node_view(node);
        Ok(node)
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility with historical storage (handles closed intervals from deletions).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::temporal_query_span("get_edge_at_time").entered();

        let edge = self
            .historical
            .read()
            .get_edge_at_time(edge_id, valid_time, transaction_time)
            .record_error_metric()?;
        // GDPR crypto-shred (Issue #3359, PR-1b): unseal designated properties on
        // the point-in-time read boundary. No-op unless the DB has designations.
        #[cfg(feature = "audit-export")]
        let edge = self.unseal_edge_view(edge);
        Ok(edge)
    }

    /// Get multiple nodes as they existed at a specific point in bi-temporal space.
    ///
    /// This is more efficient than calling `get_node_at_time` in a loop because it
    /// acquires the historical storage lock only once for all queries.
    ///
    /// **Note**: This implementation is similar to `get_edges_at_time()`. While the
    /// duplication could be eliminated with a generic helper function, we keep them
    /// separate for clarity and maintainability, as the type-specific operations
    /// (Node vs Edge construction) would require complex trait bounds.
    ///
    /// # Arguments
    ///
    /// * `node_ids` - Slice of node IDs to query
    /// * `valid_time` - The valid time to query
    /// * `transaction_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, `Option<Node>`) pairs, where None indicates the node
    /// did not exist at the specified time. Results are returned in the same
    /// order as the input node_ids.
    ///
    /// # Error Handling
    ///
    /// Unlike `get_node_at_time()` which returns an error when a node is not found,
    /// this batch API returns `None` for individual nodes that don't exist at the
    /// specified time. This allows partial results when querying multiple nodes.
    /// The method only returns `Err` for systemic failures (lock poisoning, storage
    /// corruption, property reconstruction failures).
    ///
    /// # Duplicate Handling
    ///
    /// If `node_ids` contains duplicate IDs, each will be processed independently
    /// and appear in the results. No deduplication is performed. The caller is
    /// responsible for deduplication if needed.
    ///
    /// # Performance Characteristics
    ///
    /// The current implementation holds a read lock on historical storage for the
    /// entire batch processing duration, including property reconstruction. This
    /// design prioritizes simplicity and correctness for the initial implementation.
    ///
    /// For very large batches (1000+ entities), consider:
    /// - Breaking the batch into smaller chunks
    /// - Using this method when temporal consistency across the batch is required
    ///
    /// Future optimization: A two-phase approach (gather version IDs, then reconstruct)
    /// could reduce lock hold time for better concurrency, at the cost of additional
    /// lock acquisitions.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node1 = NodeId::new(1)?;
    /// # let valid_time = Timestamp::from(0);
    /// # let tx_time = Timestamp::from(0);
    /// // Query 100 nodes at a historical point with single lock acquisition
    /// let node_ids = vec![node1];
    /// let results = db.get_nodes_at_time(&node_ids, valid_time, tx_time)?;
    ///
    /// for (node_id, node_opt) in results {
    ///     if let Some(node) = node_opt {
    ///         println!("Node {} existed with properties: {:?}", node_id, node.properties);
    ///     } else {
    ///         println!("Node {} did not exist at this time", node_id);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_nodes_at_time(
        &self,
        node_ids: &[NodeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(NodeId, Option<Node>)>> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::temporal_query_span("get_nodes_at_time").entered();

        self.historical
            .read()
            .get_nodes_at_time(node_ids, valid_time, transaction_time)
            .record_error_metric()
    }

    /// Get multiple edges as they existed at a specific point in bi-temporal space.
    ///
    /// This is more efficient than calling `get_edge_at_time` in a loop because it
    /// acquires the historical storage lock only once for all queries.
    ///
    /// # Arguments
    ///
    /// * `edge_ids` - Slice of edge IDs to query
    /// * `valid_time` - The valid time to query
    /// * `transaction_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of (EdgeId, `Option<Edge>`) pairs, where None indicates the edge
    /// did not exist at the specified time. Results are returned in the same
    /// order as the input edge_ids.
    ///
    /// # Error Handling
    ///
    /// Unlike `get_edge_at_time()` which returns an error when an edge is not found,
    /// this batch API returns `None` for individual edges that don't exist at the
    /// specified time. This allows partial results when querying multiple edges.
    /// The method only returns `Err` for systemic failures (lock poisoning, storage
    /// corruption, property reconstruction failures).
    ///
    /// # Duplicate Handling
    ///
    /// If `edge_ids` contains duplicate IDs, each will be processed independently
    /// and appear in the results. No deduplication is performed. The caller is
    /// responsible for deduplication if needed.
    ///
    /// # Performance Characteristics
    ///
    /// The current implementation holds a read lock on historical storage for the
    /// entire batch processing duration, including property reconstruction. This
    /// design prioritizes simplicity and correctness for the initial implementation.
    ///
    /// For very large batches (1000+ entities), consider:
    /// - Breaking the batch into smaller chunks
    /// - Using this method when temporal consistency across the batch is required
    ///
    /// Future optimization: A two-phase approach (gather version IDs, then reconstruct)
    /// could reduce lock hold time for better concurrency, at the cost of additional
    /// lock acquisitions.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::EdgeId;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let edge1 = EdgeId::new(1)?;
    /// # let valid_time = Timestamp::from(0);
    /// # let tx_time = Timestamp::from(0);
    /// // Query multiple edges at a historical point with single lock acquisition
    /// let edge_ids = vec![edge1];
    /// let results = db.get_edges_at_time(&edge_ids, valid_time, tx_time)?;
    ///
    /// for (edge_id, edge_opt) in results {
    ///     if let Some(edge) = edge_opt {
    ///         println!("Edge {} existed: {} -> {}", edge_id, edge.source, edge.target);
    ///     } else {
    ///         println!("Edge {} did not exist at this time", edge_id);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edges_at_time(
        &self,
        edge_ids: &[EdgeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(EdgeId, Option<Edge>)>> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::temporal_query_span("get_edges_at_time").entered();

        self.historical
            .read()
            .get_edges_at_time(edge_ids, valid_time, transaction_time)
            .record_error_metric()
    }

    // ========================================================================
    // History & Version API (Phase 9: True Bi-Temporal)
    // ========================================================================

    /// Get a node as it was valid at a specific valid time.
    ///
    /// Transaction time defaults to "now" - queries what was valid at the given time,
    /// based on the latest knowledge.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let jan_15 = Timestamp::from(0);
    /// // "What were Alice's properties on January 15th?"
    /// let node = db.get_node_at_valid_time(alice_id, jan_15)?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_at_valid_time(&self, node_id: NodeId, valid_time: Timestamp) -> Result<Node> {
        let tx_time = time::now();
        self.get_node_at_time(node_id, valid_time, tx_time)
    }

    /// Get a node as it was recorded at a specific transaction time.
    ///
    /// Valid time defaults to "now" - queries what we knew at the given time,
    /// regardless of when facts were valid.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let feb_1 = Timestamp::from(0);
    /// // "What did we know about Alice on February 1st?"
    /// let node = db.get_node_at_transaction_time(alice_id, feb_1)?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_at_transaction_time(
        &self,
        node_id: NodeId,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        let valid_time = time::now();
        self.get_node_at_time(node_id, valid_time, transaction_time)
    }

    /// Get the complete version history of a node.
    ///
    /// Returns all versions in chronological order (oldest first).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// let history = db.get_node_history(alice_id)?;
    /// println!("Alice has {} versions", history.version_count());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_history(&self, node_id: NodeId) -> Result<EntityHistory> {
        self.historical
            .read()
            .get_node_history(node_id)
            .record_error_metric()
    }

    /// Get a node at a specific logical version number.
    ///
    /// Version numbers are 1-indexed (1 = first version, 2 = second version, etc.).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// let v1 = db.get_node_at_version(alice_id, 1)?;  // Original version
    /// let v2 = db.get_node_at_version(alice_id, 2)?;  // After first update
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_at_version(&self, node_id: NodeId, version_number: u64) -> Result<Node> {
        self.historical
            .read()
            .get_node_at_version(node_id, version_number)
            .record_error_metric()
    }

    /// Compute the difference between two versions of a node.
    ///
    /// Shows which properties were added, removed, or modified.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// let history = db.get_node_history(alice_id)?;
    /// let v1 = history.first_version().unwrap().version_id;
    /// let v2 = history.current_version().unwrap().version_id;
    ///
    /// let diff = db.diff_node_versions(alice_id, v1, v2)?;
    /// if diff.has_changes() {
    ///     println!("Properties changed: {}", diff.change_count());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn diff_node_versions(
        &self,
        node_id: NodeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        self.historical
            .read()
            .diff_node_versions(node_id, from_version, to_version)
            .record_error_metric()
    }

    /// Get an edge at a specific valid time.
    ///
    /// Query by valid time only (transaction time defaults to now).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_at_valid_time(&self, edge_id: EdgeId, valid_time: Timestamp) -> Result<Edge> {
        let transaction_time = time::now();
        self.get_edge_at_time(edge_id, valid_time, transaction_time)
    }

    /// Get an edge at a specific transaction time.
    ///
    /// Query by transaction time only (valid time defaults to now).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_at_transaction_time(
        &self,
        edge_id: EdgeId,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        let valid_time = time::now();
        self.get_edge_at_time(edge_id, valid_time, transaction_time)
    }

    /// Get the complete version history of an edge.
    ///
    /// Returns all versions in chronological order (oldest first).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_history(&self, edge_id: EdgeId) -> Result<EntityHistory> {
        self.historical
            .read()
            .get_edge_history(edge_id)
            .record_error_metric()
    }

    /// Compute the difference between two versions of an edge.
    ///
    /// Shows which properties were added, removed, or modified.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn diff_edge_versions(
        &self,
        edge_id: EdgeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        self.historical
            .read()
            .diff_edge_versions(edge_id, from_version, to_version)
            .record_error_metric()
    }

    /// Enumerate the entities (nodes **and** edges) that changed within a transaction-time
    /// window — the graph-wide temporal changefeed (Issue #3216).
    ///
    /// This is the *discovery* counterpart to the per-entity history/diff APIs: it answers
    /// "**which** entities were created, modified, or deleted between T1 and T2?" without the
    /// caller already knowing any IDs. Each returned [`ChangeRecord`](crate::core::ChangeRecord)
    /// carries the entity id, kind, change type, and the version's transaction- and valid-time
    /// bounds, so a caller can then drill in with `get_node_history` / `diff_node_versions`.
    ///
    /// # Semantics
    ///
    /// - The transaction-time window `[tx_from, tx_to)` is **required** and half-open: a
    ///   version is included iff its commit timestamp lies in the window.
    /// - An optional valid-time window further constrains results (both `valid_from` and
    ///   `valid_to` must be supplied together). Deletions (tombstones) have an empty valid-time
    ///   range and are included iff their deletion instant lies within the valid window.
    /// - An optional label filter matches both node labels and edge types by exact string.
    /// - Results are deterministically ordered (transaction-time ascending, then kind, then id)
    ///   and bounded by `limit`; when more rows remain, `next_cursor` carries an opaque,
    ///   replayable continuation token.
    /// - An empty window (`tx_from == tx_to`) yields an empty page — it is **not** an error.
    /// - Only committed versions are ever returned; uncommitted/rolled-back versions are never
    ///   present in historical storage.
    ///
    /// # Errors
    ///
    /// Returns `TemporalError::InvalidTimeRange` when `tx_from > tx_to` (or for an inverted
    /// valid-time window), and `QueryError::InvalidParameter` for a malformed `cursor` or a
    /// half-specified valid-time window.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn list_changes(&self, query: &ChangeFeedQuery) -> Result<ChangeFeedPage> {
        #[cfg(feature = "observability")]
        let _span = crate::observability::temporal_query_span("list_changes").entered();

        // Validate the (required) transaction-time window. `TimeRange::new` rejects start > end
        // but allows start == end, so an empty window is valid and yields no rows.
        let tx_window = TimeRange::new(query.tx_from, query.tx_to)?;

        // Validate the optional valid-time window: both bounds or neither.
        let valid_window = match (query.valid_from, query.valid_to) {
            (Some(from), Some(to)) => Some(TimeRange::new(from, to)?),
            (None, None) => None,
            _ => {
                return Err(crate::core::error::QueryError::InvalidParameter {
                    parameter: "valid_time".to_string(),
                    reason: "valid_from and valid_to must be supplied together".to_string(),
                }
                .into());
            }
        };

        // Decode the continuation cursor (if any) before touching storage.
        let cursor = match &query.cursor {
            Some(token) => Some(ChangeCursor::decode(token)?),
            None => None,
        };

        let label = query.label.as_deref();
        let valid = valid_window.as_ref();

        // A page must be able to carry a continuation cursor, so it can never be empty while more
        // rows exist: treat limit 0 as 1. Computed up front so the filter + limit can be pushed
        // down into both tiers' scans.
        let limit = query.limit.max(1);

        // Filter + limit pushdown (Issue #3216, PR 2): each tier applies the transaction-/valid-
        // time window, label filter, and the strict `> cursor` resume *during* its scan, and
        // retains only the `bound = limit + 1`-smallest survivors by cursor. The `+1` lets us
        // detect `has_more` without materializing the rest. Bounding per tier is sound because
        // the page is the `limit`-smallest of the union: a survivor beyond a tier's
        // `limit + 1`-smallest has at least `limit` smaller survivors in the union and can never
        // reach the page. The retained per-tier sets therefore always contain the true page, so
        // the merged result below is byte-identical to an unbounded scan.
        let bound = limit.saturating_add(1);

        // Scan the hot tier under the read lock, but keep the lock hold short: `collect_changes`
        // returns lightweight `RawChange`s (no label allocation), already filtered/resumed and
        // bounded. The cold-tier scan (disk I/O) happens after the lock is released using the
        // cloned tiered handle.
        let (mut changes, tiered) = {
            let hist = self.historical.read();
            (
                hist.collect_changes(&tx_window, valid, label, cursor, bound),
                hist.tiered_storage_arc(),
            )
        };

        // Include versions that have migrated out of the hot maps into cold storage, so the feed
        // is complete when tiered storage is enabled. The cold scan applies the same filter +
        // resume + bound; dedup against hot by (kind, version_id) covers a version transiently
        // present in both tiers during migration (its cursor is identical in both, so it survives
        // in the smaller-cursor region of each bounded set and dedup keeps the hot copy).
        if let Some(tiered) = tiered {
            let mut seen: std::collections::HashSet<(u8, u64)> = changes
                .iter()
                .map(|r| (r.cursor.kind_ord, r.cursor.version_id))
                .collect();

            for rec in tiered.collect_cold_changes(&tx_window, valid, label, cursor, bound)? {
                if seen.insert((rec.cursor.kind_ord, rec.cursor.version_id)) {
                    changes.push(rec);
                }
            }
        }

        // Test-only: record the candidate working-set size before pagination bounds it. With the
        // pushdown this is `O(page)` (<= 2 * (limit + 1)) rather than `O(total matches)`. Recorded
        // in a thread-local so parallel tests never clobber each other's readings.
        #[cfg(test)]
        LAST_LIST_CHANGES_CANDIDATES.with(|c| c.set(changes.len()));

        // The resume cursor was already applied inside each tier's scan, so no post-merge
        // `retain` is needed here.

        // Bound the page. When more rows remain than fit, select the `limit` smallest by cursor
        // in O(n) before sorting just that page, rather than fully sorting the whole scan.
        let has_more = changes.len() > limit;
        if has_more {
            changes.select_nth_unstable_by_key(limit, |record| record.cursor);
            changes.truncate(limit);
        }

        // Deterministic order: transaction-time ascending, then kind, id, version.
        changes.sort_unstable_by_key(|record| record.cursor);

        let next_cursor = if has_more {
            changes.last().map(|record| record.cursor.encode())
        } else {
            None
        };

        // Resolve labels only for the rows that made it onto this page.
        let records: Vec<ChangeRecord> = changes.into_iter().map(RawChange::into_record).collect();

        Ok(ChangeFeedPage {
            changes: records,
            next_cursor,
        })
    }
}

#[cfg(test)]
mod changefeed_tests {
    use crate::AletheiaDB;
    use crate::api::WriteOps;
    use crate::core::PropertyMapBuilder;
    use crate::core::changefeed::{ChangeFeedQuery, ChangeType, EntityKind};
    use crate::core::error::Error;
    use crate::core::temporal::{TIMESTAMP_MAX, TimeRange, Timestamp, time};

    fn props(name: &str) -> crate::core::PropertyMap {
        PropertyMapBuilder::new().insert("name", name).build()
    }

    /// A window covering the entire timeline.
    fn all() -> (Timestamp, Timestamp) {
        (Timestamp::from(0), TIMESTAMP_MAX)
    }

    fn query(tx_from: Timestamp, tx_to: Timestamp, limit: usize) -> ChangeFeedQuery {
        ChangeFeedQuery::new(tx_from, tx_to, limit)
    }

    #[test]
    fn created_classification() {
        let db = AletheiaDB::new().unwrap();
        let (id, _t) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();

        let (from, to) = all();
        let page = db.list_changes(&query(from, to, 100)).unwrap();
        assert_eq!(page.changes.len(), 1);
        let rec = &page.changes[0];
        assert_eq!(rec.entity_id, id.as_u64());
        assert_eq!(rec.kind, EntityKind::Node);
        assert_eq!(rec.change_type, ChangeType::Created);
        assert_eq!(rec.label, "Person");
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn modified_classification() {
        let db = AletheiaDB::new().unwrap();
        let (id, _t) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();
        db.write(|tx| {
            tx.update_node(id, props("Alice v2"))?;
            Ok::<_, Error>(())
        })
        .unwrap();

        let (from, to) = all();
        let page = db.list_changes(&query(from, to, 100)).unwrap();
        // Two versions: created + modified, ordered by tx-time ascending.
        assert_eq!(page.changes.len(), 2);
        assert_eq!(page.changes[0].change_type, ChangeType::Created);
        assert_eq!(page.changes[1].change_type, ChangeType::Modified);
    }

    #[test]
    fn deleted_classification() {
        let db = AletheiaDB::new().unwrap();
        let (id, _t) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();
        db.write(|tx| {
            tx.delete_node(id)?;
            Ok::<_, Error>(())
        })
        .unwrap();

        let (from, to) = all();
        let page = db.list_changes(&query(from, to, 100)).unwrap();
        let deleted: Vec<_> = page
            .changes
            .iter()
            .filter(|r| r.change_type == ChangeType::Deleted)
            .collect();
        assert_eq!(deleted.len(), 1, "expected exactly one deletion row");
        // A tombstone has an empty valid-time range.
        assert!(deleted[0].valid_time_range.is_empty());
    }

    #[test]
    fn enumerates_nodes_and_edges() {
        let db = AletheiaDB::new().unwrap();
        let (a, _) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("A")))
            .unwrap();
        let (b, _) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("B")))
            .unwrap();
        let (edge, _) = db
            .write_with_timestamp(|tx| tx.create_edge(a, b, "KNOWS", props("e")))
            .unwrap();

        let (from, to) = all();
        let page = db.list_changes(&query(from, to, 100)).unwrap();
        assert_eq!(page.changes.len(), 3);
        assert!(
            page.changes
                .iter()
                .any(|r| r.kind == EntityKind::Edge && r.entity_id == edge.as_u64())
        );
        assert_eq!(
            page.changes
                .iter()
                .filter(|r| r.kind == EntityKind::Node)
                .count(),
            2
        );
    }

    #[test]
    fn tx_window_is_half_open() {
        let db = AletheiaDB::new().unwrap();
        let (_id, t_create) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();

        // [t_create, MAX) -> includes the version (start is inclusive).
        let included = db
            .list_changes(&query(t_create, TIMESTAMP_MAX, 100))
            .unwrap();
        assert_eq!(included.changes.len(), 1);

        // [0, t_create) -> excludes the version (end is exclusive).
        let excluded = db
            .list_changes(&query(Timestamp::from(0), t_create, 100))
            .unwrap();
        assert_eq!(excluded.changes.len(), 0);
    }

    #[test]
    fn empty_window_is_empty_not_error() {
        let db = AletheiaDB::new().unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();

        let now = time::now();
        let page = db.list_changes(&query(now, now, 100)).unwrap();
        assert!(page.changes.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn invalid_window_errors() {
        let db = AletheiaDB::new().unwrap();
        let later = Timestamp::from(2_000_000);
        let earlier = Timestamp::from(1_000_000);
        let err = db.list_changes(&query(later, earlier, 100));
        assert!(err.is_err(), "tx_from > tx_to must error");
    }

    #[test]
    fn valid_time_constraint_filters() {
        let db = AletheiaDB::new().unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();

        let (from, to) = all();
        // A node created "now" is valid from now to forever; a valid window entirely in the
        // far past should exclude it.
        let mut q = query(from, to, 100);
        q.valid_from = Some(Timestamp::from(1));
        q.valid_to = Some(Timestamp::from(2));
        let page = db.list_changes(&q).unwrap();
        assert_eq!(page.changes.len(), 0);

        // A valid window covering the whole timeline includes it.
        let mut q2 = query(from, to, 100);
        q2.valid_from = Some(Timestamp::from(0));
        q2.valid_to = Some(TIMESTAMP_MAX);
        let page2 = db.list_changes(&q2).unwrap();
        assert_eq!(page2.changes.len(), 1);
    }

    #[test]
    fn half_specified_valid_window_errors() {
        let db = AletheiaDB::new().unwrap();
        let (from, to) = all();
        let mut q = query(from, to, 100);
        q.valid_from = Some(Timestamp::from(1));
        // valid_to omitted.
        assert!(db.list_changes(&q).is_err());
    }

    #[test]
    fn label_filter() {
        let db = AletheiaDB::new().unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Company", props("Acme")))
            .unwrap();

        let (from, to) = all();
        let mut q = query(from, to, 100);
        q.label = Some("Person".to_string());
        let page = db.list_changes(&q).unwrap();
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].label, "Person");
    }

    #[test]
    fn ordering_is_deterministic() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..5 {
            db.write_with_timestamp(|tx| tx.create_node("Person", props(&format!("n{i}"))))
                .unwrap();
        }
        let (from, to) = all();
        let page = db.list_changes(&query(from, to, 100)).unwrap();
        assert_eq!(page.changes.len(), 5);
        // Transaction-time ascending, then kind, id, version.
        for w in page.changes.windows(2) {
            let a = &w[0];
            let b = &w[1];
            assert!(
                (
                    a.transaction_time(),
                    a.kind.ord(),
                    a.entity_id,
                    a.version_id
                ) <= (
                    b.transaction_time(),
                    b.kind.ord(),
                    b.entity_id,
                    b.version_id
                )
            );
        }
    }

    #[test]
    fn pagination_is_stable_and_replayable() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..5 {
            db.write_with_timestamp(|tx| tx.create_node("Person", props(&format!("n{i}"))))
                .unwrap();
        }
        let (from, to) = all();

        // Full unpaginated result for comparison.
        let full = db.list_changes(&query(from, to, 100)).unwrap();
        assert_eq!(full.changes.len(), 5);

        // Page through with limit = 2.
        let mut collected = Vec::new();
        let mut cursor = None;
        let mut pages = 0;
        loop {
            let mut q = query(from, to, 2);
            q.cursor = cursor.clone();
            let page = db.list_changes(&q).unwrap();
            collected.extend(page.changes.iter().map(|r| r.entity_id));
            pages += 1;
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
            assert!(pages <= 5, "pagination did not terminate");
        }
        assert_eq!(pages, 3); // 2 + 2 + 1
        let expected: Vec<u64> = full.changes.iter().map(|r| r.entity_id).collect();
        assert_eq!(collected, expected, "paged result must equal unpaginated");

        // Replayability: re-running page 2 with page 1's cursor yields identical rows.
        let mut q1 = query(from, to, 2);
        q1.cursor = None;
        let page1 = db.list_changes(&q1).unwrap();
        let mut q2a = query(from, to, 2);
        q2a.cursor = page1.next_cursor.clone();
        let page2a = db.list_changes(&q2a).unwrap();
        let mut q2b = query(from, to, 2);
        q2b.cursor = page1.next_cursor.clone();
        let page2b = db.list_changes(&q2b).unwrap();
        assert_eq!(page2a.changes, page2b.changes);
    }

    #[test]
    fn rolled_back_tx_is_not_surfaced() {
        let db = AletheiaDB::new().unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("committed")))
            .unwrap();

        let (from, to) = all();
        let before = db
            .list_changes(&query(from, to, 100))
            .unwrap()
            .changes
            .len();

        // A write that creates a node then fails -> rolled back, no version committed.
        let result: Result<(), Error> = db.write(|tx| {
            let _ = tx.create_node("Person", props("rolled-back"))?;
            Err(Error::Other("intentional rollback".to_string()))
        });
        assert!(result.is_err());

        let after = db
            .list_changes(&query(from, to, 100))
            .unwrap()
            .changes
            .len();
        assert_eq!(before, after, "rolled-back versions must not appear");
    }

    #[test]
    fn bad_cursor_errors_not_panics() {
        let db = AletheiaDB::new().unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();
        let (from, to) = all();
        let mut q = query(from, to, 100);
        q.cursor = Some("not-a-valid-cursor".to_string());
        assert!(db.list_changes(&q).is_err());
    }

    #[test]
    fn unknown_node_window_far_future_is_empty() {
        let db = AletheiaDB::new().unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
            .unwrap();
        // Window far in the future, after everything committed.
        let far = Timestamp::from(time::now().wallclock() + 1_000_000_000_000);
        let page = db.list_changes(&query(far, TIMESTAMP_MAX, 100)).unwrap();
        assert!(page.changes.is_empty());
        // Sanity: TimeRange constructor is reachable for documentation of bounds.
        let _ = TimeRange::new(Timestamp::from(0), far).unwrap();
    }

    #[test]
    fn updated_edge_is_modified() {
        let db = AletheiaDB::new().unwrap();
        let (a, _) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("A")))
            .unwrap();
        let (b, _) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("B")))
            .unwrap();
        let (edge, _) = db
            .write_with_timestamp(|tx| tx.create_edge(a, b, "KNOWS", props("e")))
            .unwrap();
        db.write(|tx| {
            tx.update_edge(edge, props("e2"))?;
            Ok::<_, Error>(())
        })
        .unwrap();

        let (from, to) = all();
        let mut q = query(from, to, 100);
        q.label = Some("KNOWS".to_string());
        let page = db.list_changes(&q).unwrap();
        assert_eq!(page.changes.len(), 2);
        assert_eq!(page.changes[0].change_type, ChangeType::Created);
        assert_eq!(page.changes[1].change_type, ChangeType::Modified);
    }

    #[test]
    fn pagination_carries_version_id_for_drill_in() {
        // Every emitted row exposes a version_id (usable with the history/diff APIs) and the
        // cursor includes it, so the sort key is unique per version regardless of how commit
        // timestamps are assigned. Paging never drops or duplicates a row.
        let db = AletheiaDB::new().unwrap();
        for i in 0..5 {
            db.write_with_timestamp(|tx| tx.create_node("Person", props(&format!("n{i}"))))
                .unwrap();
        }
        let (from, to) = all();
        let full = db.list_changes(&query(from, to, 100)).unwrap();
        assert_eq!(full.changes.len(), 5);

        let mut collected: Vec<(u64, u64)> = Vec::new();
        let mut cursor = None;
        loop {
            let mut q = query(from, to, 2);
            q.cursor = cursor.clone();
            let page = db.list_changes(&q).unwrap();
            collected.extend(page.changes.iter().map(|r| (r.entity_id, r.version_id)));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
            assert!(collected.len() <= 10, "pagination did not terminate");
        }
        let mut expected: Vec<(u64, u64)> = full
            .changes
            .iter()
            .map(|r| (r.entity_id, r.version_id))
            .collect();
        expected.sort_unstable();
        collected.sort_unstable();
        assert_eq!(
            collected, expected,
            "no version dropped or duplicated across pages"
        );
    }

    #[test]
    fn limit_zero_is_treated_as_one() {
        let db = AletheiaDB::new().unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("a")))
            .unwrap();
        db.write_with_timestamp(|tx| tx.create_node("Person", props("b")))
            .unwrap();

        let (from, to) = all();
        let page = db.list_changes(&query(from, to, 0)).unwrap();
        assert!(
            !page.changes.is_empty(),
            "limit 0 must not silently drop all rows"
        );
        assert!(
            page.next_cursor.is_some(),
            "rows remain, so a continuation cursor must be present"
        );
    }

    #[test]
    fn valid_time_window_boundary_pairing() {
        use crate::api::WriteOps;
        let db = AletheiaDB::new().unwrap();
        // Create with valid_from=100, then delete with valid_from=200 (tombstone instant 200).
        let id = db
            .write(|tx| tx.create_node_with_valid_time("Person", props("x"), Some(100.into())))
            .unwrap();
        db.write(|tx| {
            tx.delete_node_with_valid_time(id, Some(200.into()))?;
            Ok::<_, Error>(())
        })
        .unwrap();

        let (from, to) = all();

        // Window [100,200): the live create overlaps; the deletion instant 200 is excluded.
        let mut q = query(from, to, 100);
        q.valid_from = Some(100.into());
        q.valid_to = Some(200.into());
        let page = db.list_changes(&q).unwrap();
        assert!(
            page.changes
                .iter()
                .any(|r| r.change_type == ChangeType::Created)
        );
        assert!(
            !page
                .changes
                .iter()
                .any(|r| r.change_type == ChangeType::Deleted),
            "deletion instant equals the exclusive upper bound, so it is excluded"
        );

        // Window [200,300): both the still-live-at-200 create and the deletion instant are in.
        let mut q2 = query(from, to, 100);
        q2.valid_from = Some(200.into());
        q2.valid_to = Some(300.into());
        let page2 = db.list_changes(&q2).unwrap();
        assert!(
            page2
                .changes
                .iter()
                .any(|r| r.change_type == ChangeType::Deleted)
        );
    }

    #[test]
    fn cold_storage_versions_appear_in_feed() {
        use crate::core::id::{NodeId, VersionId};
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::temporal::BiTemporalInterval;
        use crate::core::version::NodeVersion;
        use crate::storage::redb_cold_storage::RedbColdStorage;
        use crate::storage::tiered_storage::TieredStorage;
        use std::sync::Arc;

        let db = AletheiaDB::new().unwrap();
        let (hot_id, _t) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("hot")))
            .unwrap();

        // Place a version that exists ONLY in the cold tier, as if migrated out of the hot maps.
        let dir = tempfile::tempdir().unwrap();
        let cold =
            Arc::new(RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap());
        let cold_version = NodeVersion::new_anchor(
            VersionId::new(999_999).unwrap(),
            NodeId::new(888_888).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("ColdNode").unwrap(),
            PropertyMap::new(),
        );
        cold.store_node_version(&cold_version).unwrap();
        let tiered = Arc::new(TieredStorage::with_default_config(cold));
        db.__test_historical_storage()
            .write()
            .set_tiered_storage(tiered);

        let (from, to) = all();
        let page = db.list_changes(&query(from, to, 100)).unwrap();
        assert!(
            page.changes.iter().any(|r| r.entity_id == hot_id.as_u64()),
            "hot version still present"
        );
        assert!(
            page.changes.iter().any(|r| r.entity_id == 888_888),
            "cold-only version must be included in the feed"
        );
    }
}

/// Parity + edge-case tests for the `list_changes` filter/limit pushdown (PR 2, Issue #3216).
///
/// Every scenario runs the same query through the production [`AletheiaDB::list_changes`] (the
/// filter+limit-pushed-down path) and through [`list_changes_reference`] — a faithful copy of the
/// pre-pushdown algorithm (unbounded hot collect + full cold scan + post-merge cursor retain +
/// `select_nth` limit) — and asserts the two produce a byte-identical page (`changes` Vec +
/// `next_cursor`). The reference is independent of the pushdown's bounding logic, so it is a
/// genuine old-vs-new oracle rather than a restatement of the optimized code.
#[cfg(test)]
mod changefeed_pushdown_tests {
    use super::LAST_LIST_CHANGES_CANDIDATES;
    use crate::AletheiaDB;
    use crate::api::WriteOps;
    use crate::core::PropertyMap;
    use crate::core::PropertyMapBuilder;
    use crate::core::changefeed::{
        ChangeCursor, ChangeFeedPage, ChangeFeedQuery, ChangeRecord, ChangeType, EntityKind,
        RawChange, build_raw_change,
    };
    use crate::core::error::{Error, Result};
    use crate::core::id::{EdgeId, NodeId, VersionId};
    use crate::core::interning::{GLOBAL_INTERNER, InternedString};
    use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, Timestamp, time};
    use crate::core::version::{EdgeVersion, NodeVersion};
    use crate::storage::redb_cold_storage::RedbColdStorage;
    use crate::storage::tiered_storage::TieredStorage;
    use std::sync::Arc;

    fn props(name: &str) -> PropertyMap {
        PropertyMapBuilder::new().insert("name", name).build()
    }

    fn all() -> (Timestamp, Timestamp) {
        (Timestamp::from(0), TIMESTAMP_MAX)
    }

    fn query(from: Timestamp, to: Timestamp, limit: usize) -> ChangeFeedQuery {
        ChangeFeedQuery::new(from, to, limit)
    }

    fn ts(micros: i64) -> Timestamp {
        Timestamp::from(micros)
    }

    /// A synthetic cold node "Created" anchor with a caller-chosen version id and tx-time.
    fn cold_node(vid: u64, nid: u64, tx: Timestamp, label: &str) -> NodeVersion {
        NodeVersion::new_anchor(
            VersionId::new(vid).unwrap(),
            NodeId::new(nid).unwrap(),
            BiTemporalInterval::now(tx, tx),
            GLOBAL_INTERNER.intern(label).unwrap(),
            PropertyMap::new(),
        )
    }

    /// A synthetic cold node tombstone (empty valid range `[v, v)`) committed at `tx`.
    fn cold_node_tomb(vid: u64, nid: u64, tx: Timestamp, valid_instant: Timestamp) -> NodeVersion {
        let interval = BiTemporalInterval::new(
            TimeRange::new(valid_instant, valid_instant).unwrap(),
            TimeRange::from(tx),
        );
        NodeVersion::new_anchor(
            VersionId::new(vid).unwrap(),
            NodeId::new(nid).unwrap(),
            interval,
            GLOBAL_INTERNER.intern("Ghost").unwrap(),
            PropertyMap::new(),
        )
    }

    /// A synthetic cold edge "Created" anchor.
    fn cold_edge(vid: u64, eid: u64, tx: Timestamp, label: &str) -> EdgeVersion {
        EdgeVersion::new_anchor(
            VersionId::new(vid).unwrap(),
            EdgeId::new(eid).unwrap(),
            BiTemporalInterval::now(tx, tx),
            GLOBAL_INTERNER.intern(label).unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMap::new(),
        )
    }

    /// Attach a cold tier holding the given synthetic versions to `db`. Returns the tempdir,
    /// which must be kept alive for the duration of the test.
    fn attach_cold(
        db: &AletheiaDB,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
    ) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let cold =
            Arc::new(RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap());
        for v in nodes {
            cold.store_node_version(v).unwrap();
        }
        for v in edges {
            cold.store_edge_version(v).unwrap();
        }
        let tiered = Arc::new(TieredStorage::with_default_config(cold));
        db.__test_historical_storage()
            .write()
            .set_tiered_storage(tiered);
        dir
    }

    /// Clone a node version out of the hot maps by version id (used to build a dual-tier version).
    fn hot_node_version(db: &AletheiaDB, version_id: u64) -> NodeVersion {
        db.__test_historical_storage()
            .read()
            .get_node_version(VersionId::new(version_id).unwrap())
            .expect("hot version present")
            .clone()
    }

    /// Faithful copy of the pre-pushdown `list_changes` algorithm — the parity oracle.
    fn list_changes_reference(db: &AletheiaDB, query: &ChangeFeedQuery) -> Result<ChangeFeedPage> {
        let tx_window = TimeRange::new(query.tx_from, query.tx_to)?;
        let valid_window = match (query.valid_from, query.valid_to) {
            (Some(from), Some(to)) => Some(TimeRange::new(from, to)?),
            (None, None) => None,
            _ => {
                return Err(crate::core::error::QueryError::InvalidParameter {
                    parameter: "valid_time".to_string(),
                    reason: "valid_from and valid_to must be supplied together".to_string(),
                }
                .into());
            }
        };
        let cursor = match &query.cursor {
            Some(token) => Some(ChangeCursor::decode(token)?),
            None => None,
        };
        let label = query.label.as_deref();
        let valid = valid_window.as_ref();

        let (mut changes, tiered) = {
            let hist = db.__test_historical_storage().read();
            (
                hist.collect_changes(&tx_window, valid, label, None, usize::MAX),
                hist.tiered_storage_arc(),
            )
        };

        if let Some(tiered) = tiered {
            let mut seen: std::collections::HashSet<(u8, u64)> = changes
                .iter()
                .map(|r| (r.cursor.kind_ord, r.cursor.version_id))
                .collect();
            for v in tiered.scan_node_versions_cold()? {
                if let Some(rec) = build_raw_change(
                    v.id.as_u64(),
                    v.node_id.as_u64(),
                    EntityKind::Node,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                    &tx_window,
                    valid,
                    label,
                )
                .filter(|rec| seen.insert((rec.cursor.kind_ord, rec.cursor.version_id)))
                {
                    changes.push(rec);
                }
            }
            for v in tiered.scan_edge_versions_cold()? {
                if let Some(rec) = build_raw_change(
                    v.id.as_u64(),
                    v.edge_id.as_u64(),
                    EntityKind::Edge,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                    &tx_window,
                    valid,
                    label,
                )
                .filter(|rec| seen.insert((rec.cursor.kind_ord, rec.cursor.version_id)))
                {
                    changes.push(rec);
                }
            }
        }

        let limit = query.limit.max(1);
        if let Some(c) = cursor {
            changes.retain(|record| record.cursor > c);
        }
        let has_more = changes.len() > limit;
        if has_more {
            changes.select_nth_unstable_by_key(limit, |record| record.cursor);
            changes.truncate(limit);
        }
        changes.sort_unstable_by_key(|record| record.cursor);
        let next_cursor = if has_more {
            changes.last().map(|record| record.cursor.encode())
        } else {
            None
        };
        let records: Vec<_> = changes.into_iter().map(RawChange::into_record).collect();
        Ok(ChangeFeedPage {
            changes: records,
            next_cursor,
        })
    }

    /// Assert the production path and the reference oracle produce byte-identical pages, and
    /// return the production page for further assertions.
    fn assert_parity(db: &AletheiaDB, query: &ChangeFeedQuery) -> ChangeFeedPage {
        let got = db.list_changes(query).expect("list_changes ok");
        let want = list_changes_reference(db, query).expect("reference ok");
        assert_eq!(
            got.changes, want.changes,
            "page rows must be byte-identical"
        );
        assert_eq!(
            got.next_cursor, want.next_cursor,
            "next_cursor must be byte-identical"
        );
        got
    }

    /// Drain every page of a query (following `next_cursor`) into one ordered Vec.
    fn drain_all(db: &AletheiaDB, base: &ChangeFeedQuery) -> Vec<(u64, u8, u64)> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q = base.clone();
            q.cursor = cursor.clone();
            let page = db.list_changes(&q).unwrap();
            for r in &page.changes {
                out.push((r.entity_id, r.kind.ord(), r.version_id));
            }
            match page.next_cursor {
                Some(tok) => cursor = Some(tok),
                None => break,
            }
        }
        out
    }

    /// A **second**, fully independent reference implementation of `list_changes` that shares
    /// none of the production changefeed machinery — it does not call `collect_changes`,
    /// `consider_version`, `build_raw_change`, or `BoundedChanges`. Instead it walks every hot
    /// version (`visit_node_versions`/`visit_edge_versions`) and every cold version
    /// (`scan_node_versions_cold`/`scan_edge_versions_cold`), and re-derives the filter,
    /// classification, sort key, dedup, resume, and pagination from scratch here in the test.
    ///
    /// This discharges the #3685 deferred follow-up: the existing `list_changes_reference`
    /// oracle reuses `collect_changes`/`build_raw_change`, so it is only a *partial* differential
    /// (it cannot catch a bug living inside those shared helpers). This one is a genuine
    /// hand-rolled cross-check. The semantics replicated below are those of `build_raw_change`
    /// (`src/core/changefeed.rs`): half-open tx-window `contains(tx_start)`; valid-window
    /// intersection with the tombstone-point (`contains(start)`) vs live-overlap (`overlaps`)
    /// distinction; label equality; Created/Modified/Deleted classification; the
    /// `(tx_wallclock, tx_logical, kind_ord, entity_id, version_id)` sort key; strict
    /// `> resume_after`; cross-tier dedup by `(kind_ord, version_id)` keeping the hot copy.
    fn list_changes_reference_independent(
        db: &AletheiaDB,
        query: &ChangeFeedQuery,
    ) -> Result<ChangeFeedPage> {
        // Validate the windows exactly as production does.
        let tx_window = TimeRange::new(query.tx_from, query.tx_to)?;
        let valid_window = match (query.valid_from, query.valid_to) {
            (Some(from), Some(to)) => Some(TimeRange::new(from, to)?),
            (None, None) => None,
            _ => {
                return Err(crate::core::error::QueryError::InvalidParameter {
                    parameter: "valid_time".to_string(),
                    reason: "valid_from and valid_to must be supplied together".to_string(),
                }
                .into());
            }
        };
        let cursor = match &query.cursor {
            Some(token) => Some(ChangeCursor::decode(token)?),
            None => None,
        };
        let label = query.label.as_deref();
        let valid = valid_window.as_ref();

        // Inline, from-scratch filter + classification. Returns the row iff it survives every
        // filter and the strict `> cursor` resume. No production helper is consulted.
        let consider = |version_id: u64,
                        entity_id: u64,
                        kind: EntityKind,
                        temporal: &crate::core::temporal::BiTemporalInterval,
                        label_id: InternedString,
                        prev_is_none: bool|
         -> Option<ChangeRecord> {
            let tx_range = temporal.transaction_time();
            // Half-open transaction-time window [t1, t2).
            if !tx_window.contains(tx_range.start()) {
                return None;
            }

            let valid_range = temporal.valid_time();
            let is_deletion = valid_range.is_empty();

            if let Some(vw) = valid {
                // Tombstone is a degenerate point at its instant; a live version is a range.
                let intersects = if is_deletion {
                    vw.contains(valid_range.start())
                } else {
                    valid_range.overlaps(vw)
                };
                if !intersects {
                    return None;
                }
            }

            if let Some(filter) = label
                && GLOBAL_INTERNER.resolve_with(label_id, |s| s == filter) != Some(true)
            {
                return None;
            }

            let change_type = if is_deletion {
                ChangeType::Deleted
            } else if prev_is_none {
                ChangeType::Created
            } else {
                ChangeType::Modified
            };

            let label_str = GLOBAL_INTERNER
                .resolve_with(label_id, |s| s.to_string())
                .unwrap_or_default();
            let record = ChangeRecord {
                entity_id,
                version_id,
                kind,
                change_type,
                label: label_str,
                transaction_time_range: tx_range,
                valid_time_range: valid_range,
            };

            // Strict `> resume_after` on the derived sort key.
            if let Some(c) = cursor
                && record.cursor() <= c
            {
                return None;
            }
            Some(record)
        };

        let mut rows: Vec<ChangeRecord> = Vec::new();
        let mut seen: std::collections::HashSet<(u8, u64)> = std::collections::HashSet::new();

        // Hot tier: dedup wins go to the hot copy, so ingest it first.
        let tiered = {
            let hist = db.__test_historical_storage().read();
            hist.visit_node_versions(|v| {
                if let Some(rec) = consider(
                    v.id.as_u64(),
                    v.node_id.as_u64(),
                    EntityKind::Node,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                ) && seen.insert((rec.kind.ord(), rec.version_id))
                {
                    rows.push(rec);
                }
            });
            hist.visit_edge_versions(|v| {
                if let Some(rec) = consider(
                    v.id.as_u64(),
                    v.edge_id.as_u64(),
                    EntityKind::Edge,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                ) && seen.insert((rec.kind.ord(), rec.version_id))
                {
                    rows.push(rec);
                }
            });
            hist.tiered_storage_arc()
        };

        // Cold tier (full scan; skip versions already delivered by the hot tier).
        if let Some(tiered) = tiered {
            for v in tiered.scan_node_versions_cold()? {
                if let Some(rec) = consider(
                    v.id.as_u64(),
                    v.node_id.as_u64(),
                    EntityKind::Node,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                ) && seen.insert((rec.kind.ord(), rec.version_id))
                {
                    rows.push(rec);
                }
            }
            for v in tiered.scan_edge_versions_cold()? {
                if let Some(rec) = consider(
                    v.id.as_u64(),
                    v.edge_id.as_u64(),
                    EntityKind::Edge,
                    &v.temporal,
                    v.label,
                    v.prev_version.is_none(),
                ) && seen.insert((rec.kind.ord(), rec.version_id))
                {
                    rows.push(rec);
                }
            }
        }

        // Deterministic order, then bound to the page and compute the continuation cursor.
        let limit = query.limit.max(1);
        rows.sort_by_key(|r| r.cursor());
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if has_more {
            rows.last().map(|r| r.cursor().encode())
        } else {
            None
        };

        Ok(ChangeFeedPage {
            changes: rows,
            next_cursor,
        })
    }

    /// Assert the production path and the **independent** hand-rolled oracle produce a
    /// byte-identical page (`changes` Vec + `next_cursor`). Returns the production page.
    fn assert_parity_independent(db: &AletheiaDB, query: &ChangeFeedQuery) -> ChangeFeedPage {
        let got = db.list_changes(query).expect("list_changes ok");
        let want = list_changes_reference_independent(db, query).expect("independent reference ok");
        assert_eq!(
            got.changes, want.changes,
            "page rows must match the independent oracle"
        );
        assert_eq!(
            got.next_cursor, want.next_cursor,
            "next_cursor must match the independent oracle"
        );
        got
    }

    // Defense-in-depth: the independent (non-shared-helper) oracle agrees with production over a
    // mixed hot+cold fixture. This is expected to PASS — production is already correct; it guards
    // against a latent bug hiding inside the shared `build_raw_change`/`collect_changes` helpers
    // that the first `list_changes_reference` oracle cannot see (#3685).
    #[test]
    fn independent_oracle_matches_production_mixed_tiers() {
        let db = AletheiaDB::new().unwrap();
        // Hot tier: a handful of real committed node versions (create + modify).
        let (hid, _t) = db
            .write_with_timestamp(|tx| tx.create_node("Person", props("hot-alice")))
            .unwrap();
        db.write(|tx| {
            tx.update_node(hid, props("hot-alice-v2"))?;
            Ok::<_, Error>(())
        })
        .unwrap();
        db.write(|tx| {
            tx.create_node("Company", props("hot-acme"))?;
            Ok::<_, Error>(())
        })
        .unwrap();

        // Cold tier: synthetic node + edge versions (distinct high ids so no dual-tier overlap),
        // including a tombstone to exercise the Deleted classification path.
        let nodes = vec![
            cold_node(1000, 1000, ts(50), "Person"),
            cold_node(1001, 1001, ts(60), "Company"),
            cold_node_tomb(1002, 1003, ts(70), ts(65)),
        ];
        let edges = vec![
            cold_edge(2000, 2000, ts(55), "KNOWS"),
            cold_edge(2001, 2001, ts(80), "WORKS_AT"),
        ];
        let _dir = attach_cold(&db, &nodes, &edges);

        let (from, to) = all();
        // Whole-timeline scans at several limits (exercise pagination + next_cursor).
        for limit in [1usize, 2, 3, 5, 100] {
            assert_parity_independent(&db, &query(from, to, limit));
        }
        // Label-filtered scan.
        let mut q = query(from, to, 100);
        q.label = Some("Person".to_string());
        assert_parity_independent(&db, &q);
        // Narrow cold-only tx window.
        assert_parity_independent(&db, &query(ts(40), ts(90), 100));
    }

    // RED: fails until the ColdChangeDirectory pushdown lands (#3677).
    //
    // AC1 measurement: a bounded, narrowly-windowed `list_changes` over a large cold tier must
    // decode only the handful of versions in the window, not every cold version. Today the cold
    // scan decodes all 200 (see the "no `version_id` early-stop" correctness note in
    // `redb_cold_storage`), so this asserts the target the green phase must reach.
    #[test]
    fn cold_directory_decodes_only_window() {
        let db = AletheiaDB::new().unwrap();

        // 200 cold node versions spread across a wide tx-time range (tx = i * 1000 µs).
        let nodes: Vec<_> = (1..=200u64)
            .map(|i| cold_node(i, i, ts((i as i64) * 1000), "Person"))
            .collect();
        let _dir = attach_cold(&db, &nodes, &[]);

        // A narrow, recent tx-window matching only versions i in {196,197,198,199,200}.
        let q = query(ts(195_500), ts(201_000), 5);

        crate::storage::redb_cold_storage::reset_cold_decode_counter();
        let page = db.list_changes(&q).unwrap();
        let decoded = crate::storage::redb_cold_storage::cold_decode_count();

        // Sanity: the window really does match only a small set.
        assert_eq!(
            page.changes.len(),
            5,
            "window should surface exactly 5 rows"
        );

        assert!(
            decoded <= 20,
            "expected windowed query to decode only window candidates, decoded {decoded} of 200"
        );
    }

    // 1 -----------------------------------------------------------------------------------------
    #[test]
    fn parity_hot_only() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..50 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("n{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let (from, to) = all();
        for limit in [1usize, 7, 10, 49, 50, 100] {
            assert_parity(&db, &query(from, to, limit));
        }
    }

    // 2 -----------------------------------------------------------------------------------------
    #[test]
    fn parity_cold_only() {
        let db = AletheiaDB::new().unwrap();
        let nodes: Vec<_> = (0..20)
            .map(|i| cold_node(1000 + i, 5000 + i, ts(100 + i as i64), "ColdNode"))
            .collect();
        let edges: Vec<_> = (0..10)
            .map(|i| cold_edge(2000 + i, 6000 + i, ts(200 + i as i64), "ColdEdge"))
            .collect();
        let _dir = attach_cold(&db, &nodes, &edges);

        let (from, to) = all();
        for limit in [1usize, 5, 15, 29, 30, 100] {
            let page = assert_parity(&db, &query(from, to, limit));
            assert!(page.changes.len() <= limit.max(1));
        }
    }

    // 3 -----------------------------------------------------------------------------------------
    #[test]
    fn parity_mixed_tiers_dedup() {
        let db = AletheiaDB::new().unwrap();
        // Real hot writes.
        for i in 0..15 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("hot{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        // Pick one hot version and mirror it into cold to create a *dual-tier* version.
        let full = db.list_changes(&query(all().0, all().1, 1000)).unwrap();
        let dual = &full.changes[3];
        let dual_vid = dual.version_id;
        let dual_entity = dual.entity_id;
        let dual_version = hot_node_version(&db, dual_vid);

        let mut cold_nodes = vec![dual_version];
        cold_nodes
            .extend((0..10).map(|i| cold_node(9000 + i, 7000 + i, ts(50 + i as i64), "Cold")));
        let _dir = attach_cold(&db, &cold_nodes, &[]);

        let (from, to) = all();
        for limit in [1usize, 3, 10, 25, 100] {
            assert_parity(&db, &query(from, to, limit));
        }
        // Full drain: the dual-tier version appears exactly once across all pages.
        let drained = drain_all(&db, &query(from, to, 4));
        let dual_hits = drained
            .iter()
            .filter(|(eid, kind, vid)| {
                *eid == dual_entity && *kind == EntityKind::Node.ord() && *vid == dual_vid
            })
            .count();
        assert_eq!(dual_hits, 1, "dual-tier version must appear exactly once");
    }

    // 4 -----------------------------------------------------------------------------------------
    #[test]
    fn parity_empty_window() {
        let db = AletheiaDB::new().unwrap();
        db.write(|tx| {
            tx.create_node("Person", props("a"))?;
            Ok::<_, Error>(())
        })
        .unwrap();
        let _dir = attach_cold(&db, &[cold_node(1, 42, ts(5), "Cold")], &[]);
        let now = time::now();
        let page = assert_parity(&db, &query(now, now, 100));
        assert!(page.changes.is_empty(), "empty window yields no rows");
        assert!(page.next_cursor.is_none());
    }

    // 5 -----------------------------------------------------------------------------------------
    #[test]
    fn parity_limit_smaller_than_matches() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..50 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("n{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let (from, to) = all();
        let page = assert_parity(&db, &query(from, to, 10));
        assert_eq!(page.changes.len(), 10);
        assert!(page.next_cursor.is_some(), "has_more page carries a cursor");
    }

    // 6 -----------------------------------------------------------------------------------------
    #[test]
    fn limit_spans_hot_cold_boundary() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..5 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("hot{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        // Cold versions interleaved in tx-time with the hot ones (hot commit near time::now()).
        let cold_nodes: Vec<_> = (0..5)
            .map(|i| cold_node(4000 + i, 8000 + i, ts(10 + i as i64), "Cold"))
            .collect();
        let _dir = attach_cold(&db, &cold_nodes, &[]);

        let (from, to) = all();
        // Sweep the page boundary across the whole merged sequence.
        for limit in 1..=11 {
            assert_parity(&db, &query(from, to, limit));
        }
        // Full drain has no dup / gap: exactly the unbounded row set.
        let unbounded = db.list_changes(&query(from, to, 10_000)).unwrap();
        let drained = drain_all(&db, &query(from, to, 3));
        let want: Vec<_> = unbounded
            .changes
            .iter()
            .map(|r| (r.entity_id, r.kind.ord(), r.version_id))
            .collect();
        assert_eq!(drained, want, "paged drain equals unbounded, no dup/gap");
    }

    // 7 -----------------------------------------------------------------------------------------
    #[test]
    fn cursor_resume_after_cold_page() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..8 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("h{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let cold_nodes: Vec<_> = (0..8)
            .map(|i| cold_node(4100 + i, 8100 + i, ts(20 + i as i64), "Cold"))
            .collect();
        let _dir = attach_cold(&db, &cold_nodes, &[]);

        let (from, to) = all();
        let unbounded = db.list_changes(&query(from, to, 10_000)).unwrap();
        let want: Vec<_> = unbounded
            .changes
            .iter()
            .map(|r| (r.entity_id, r.kind.ord(), r.version_id))
            .collect();
        for page_size in [1usize, 2, 5, 7] {
            let drained = drain_all(&db, &query(from, to, page_size));
            assert_eq!(
                drained, want,
                "resume union equals unbounded @page={page_size}"
            );
        }
    }

    // 8 -----------------------------------------------------------------------------------------
    #[test]
    fn resume_across_version_id_inversion() {
        // Craft two cold versions where the SMALLER version_id carries the LARGER tx-time. This is
        // a *static* reproduction of the on-disk state a version_id/tx-time inversion produces — a
        // guard against a naive `version_id`-range early-stop implementation, not a live
        // concurrent race (the production scan decodes every entry, so there is no early-stop for
        // this to defeat; the test asserts scan/resume ordering follows tx-time via ChangeCursor,
        // never version_id). A true concurrency test that drives two racing commits is a
        // documented follow-up.
        let db = AletheiaDB::new().unwrap();
        let earlier_txn_bigger_vid = cold_node(500, 100, ts(1_000), "Cold"); // vid 500, tx 1000
        let later_txn_smaller_vid = cold_node(100, 200, ts(2_000), "Cold"); // vid 100, tx 2000
        let _dir = attach_cold(&db, &[earlier_txn_bigger_vid, later_txn_smaller_vid], &[]);

        let (from, to) = all();
        // Parity at every page size.
        for limit in [1usize, 2, 3] {
            assert_parity(&db, &query(from, to, limit));
        }
        // Page-by-1 order must be tx-time ascending: entity 100 (tx 1000) then entity 200 (tx 2000).
        let drained = drain_all(&db, &query(from, to, 1));
        assert_eq!(
            drained,
            vec![
                (100u64, EntityKind::Node.ord(), 500u64),
                (200u64, EntityKind::Node.ord(), 100u64),
            ],
            "ordering follows tx-time, not version_id"
        );
    }

    // 9 -----------------------------------------------------------------------------------------
    #[test]
    fn tombstone_valid_window_boundary() {
        let db = AletheiaDB::new().unwrap();
        // Deletion recorded at tx=1000, valid instant v=500.
        let tomb = cold_node_tomb(700, 300, ts(1_000), ts(500));
        let _dir = attach_cold(&db, &[tomb], &[]);

        let (from, to) = all();
        // Window [500, 501) contains v=500 -> deletion included.
        let mut q_in = query(from, to, 100);
        q_in.valid_from = Some(ts(500));
        q_in.valid_to = Some(ts(501));
        let page_in = assert_parity(&db, &q_in);
        assert_eq!(page_in.changes.len(), 1);
        assert_eq!(page_in.changes[0].change_type, ChangeType::Deleted);

        // Window [499, 500) excludes v=500 (half-open) -> deletion excluded.
        let mut q_out = query(from, to, 100);
        q_out.valid_from = Some(ts(499));
        q_out.valid_to = Some(ts(500));
        let page_out = assert_parity(&db, &q_out);
        assert!(
            page_out.changes.is_empty(),
            "tombstone excluded when window omits v"
        );
    }

    // 10 ----------------------------------------------------------------------------------------
    #[test]
    fn half_open_tx_boundary() {
        let db = AletheiaDB::new().unwrap();
        let at_from = cold_node(810, 410, ts(1_000), "Cold");
        let at_to = cold_node(820, 420, ts(2_000), "Cold");
        let _dir = attach_cold(&db, &[at_from, at_to], &[]);

        // Window [1000, 2000): includes the tx=1000 row, excludes the tx=2000 row.
        let page = assert_parity(&db, &query(ts(1_000), ts(2_000), 100));
        let ids: Vec<u64> = page.changes.iter().map(|r| r.entity_id).collect();
        assert_eq!(ids, vec![410], "tx_from included, tx_to excluded");
    }

    // 11 ----------------------------------------------------------------------------------------
    #[test]
    fn empty_cold_tier() {
        let db = AletheiaDB::new().unwrap();
        db.write(|tx| {
            tx.create_node("Person", props("a"))?;
            Ok::<_, Error>(())
        })
        .unwrap();
        let _dir = attach_cold(&db, &[], &[]); // tiered enabled, nothing migrated
        let (from, to) = all();
        let page = assert_parity(&db, &query(from, to, 100));
        assert_eq!(page.changes.len(), 1);
    }

    // 12 ----------------------------------------------------------------------------------------
    #[test]
    fn cold_disabled_config() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..5 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("n{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let (from, to) = all();
        // No tiered storage attached -> cold branch skipped entirely.
        for limit in [1usize, 3, 5, 100] {
            assert_parity(&db, &query(from, to, limit));
        }
    }

    // 13 ----------------------------------------------------------------------------------------
    #[test]
    fn limit_zero_floors_to_one() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..3 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("n{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let (from, to) = all();
        let page = assert_parity(&db, &query(from, to, 0));
        assert_eq!(page.changes.len(), 1, "limit 0 floors to 1 row");
        assert!(
            page.next_cursor.is_some(),
            "cursor present since more remain"
        );
    }

    // 14 ----------------------------------------------------------------------------------------
    #[test]
    fn label_filter_pushdown_parity() {
        let db = AletheiaDB::new().unwrap();
        db.write(|tx| {
            tx.create_node("Person", props("p"))?;
            Ok::<_, Error>(())
        })
        .unwrap();
        db.write(|tx| {
            tx.create_node("Widget", props("w"))?;
            Ok::<_, Error>(())
        })
        .unwrap();
        let cold_nodes = vec![
            cold_node(3100, 9100, ts(30), "Person"),
            cold_node(3101, 9101, ts(31), "Widget"),
        ];
        let cold_edges = vec![cold_edge(3200, 9200, ts(40), "KNOWS")];
        let _dir = attach_cold(&db, &cold_nodes, &cold_edges);

        let (from, to) = all();
        for lbl in ["Person", "Widget", "KNOWS", "Nonexistent"] {
            let mut q = query(from, to, 100);
            q.label = Some(lbl.to_string());
            let page = assert_parity(&db, &q);
            assert!(
                page.changes.iter().all(|r| r.label == lbl),
                "only matching-label rows for {lbl}"
            );
        }
    }

    // 15 ----------------------------------------------------------------------------------------
    #[test]
    fn valid_window_pushdown_parity() {
        let db = AletheiaDB::new().unwrap();
        // Live node valid from t=100 (open), and a tombstone at valid instant 500.
        let live = cold_node(3300, 9300, ts(100), "Cold");
        let tomb = cold_node_tomb(3301, 9301, ts(1_000), ts(500));
        let _dir = attach_cold(&db, &[live, tomb], &[]);

        let (from, to) = all();
        for (vf, vt) in [(50i64, 200i64), (400, 600), (600, 700), (0, 2_000)] {
            let mut q = query(from, to, 100);
            q.valid_from = Some(ts(vf));
            q.valid_to = Some(ts(vt));
            assert_parity(&db, &q);
        }
    }

    // 16 ----------------------------------------------------------------------------------------
    #[test]
    fn candidate_set_is_bounded() {
        // 500 hot matches, page limit 10. After the pushdown the candidate working set the query
        // holds for pagination must be O(page) — not O(total matches). Before the pushdown this
        // was 500 (the whole match set), which is the RED failure this asserts against.
        let db = AletheiaDB::new().unwrap();
        for i in 0..500 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("n{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let (from, to) = all();
        let limit = 10usize;
        let page = assert_parity(&db, &query(from, to, limit));
        assert_eq!(page.changes.len(), limit);
        let candidates = LAST_LIST_CHANGES_CANDIDATES.with(|c| c.get());
        assert!(
            candidates <= limit + 1,
            "hot candidate working set must be bounded to limit+1, got {candidates}"
        );
    }

    // 16b: mixed-tier candidate bound (hot+cold each bounded to limit+1).
    #[test]
    fn candidate_set_is_bounded_mixed() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..200 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("n{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let cold_nodes: Vec<_> = (0..200)
            .map(|i| cold_node(20_000 + i, 30_000 + i, ts(1 + i as i64), "Cold"))
            .collect();
        let _dir = attach_cold(&db, &cold_nodes, &[]);
        let (from, to) = all();
        let limit = 10usize;
        assert_parity(&db, &query(from, to, limit));
        let candidates = LAST_LIST_CHANGES_CANDIDATES.with(|c| c.get());
        assert!(
            candidates <= 2 * (limit + 1),
            "merged candidate set must be bounded to 2*(limit+1), got {candidates}"
        );
    }

    // 17 ----------------------------------------------------------------------------------------
    /// Differentially validate the RESUME path against the oracle with a real mid-stream cursor.
    ///
    /// Every other `assert_parity` call site passes a cursor-less query, so the oracle's resume
    /// branch (`if let Some(c) = cursor { changes.retain(|r| r.cursor > c) }`) would otherwise be
    /// dead code. Here we walk the whole hot+cold stream page-by-page and `assert_parity` each
    /// *resumed* page (cursor set to a genuine interior `next_cursor`), so the production in-scan
    /// `> cursor` filter is checked against the independent post-merge-retain oracle.
    #[test]
    fn resume_parity_midstream() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..12 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("h{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let cold_nodes: Vec<_> = (0..12)
            .map(|i| cold_node(4300 + i, 8300 + i, ts(60 + i as i64), "Cold"))
            .collect();
        let _dir = attach_cold(&db, &cold_nodes, &[]);

        let (from, to) = all();
        for page_size in [1usize, 3, 5] {
            let mut cursor: Option<String> = None;
            let mut resumed_pages = 0usize;
            loop {
                let mut q = query(from, to, page_size);
                q.cursor = cursor.clone();
                // Only interior (resumed) pages carry a cursor — parity-check those so the oracle's
                // resume branch actually executes.
                if cursor.is_some() {
                    assert_parity(&db, &q);
                    resumed_pages += 1;
                }
                let page = db.list_changes(&q).unwrap();
                match page.next_cursor {
                    Some(tok) => cursor = Some(tok),
                    None => break,
                }
            }
            assert!(
                resumed_pages > 0,
                "expected at least one mid-stream resumed page @page={page_size}"
            );
        }
    }

    // 18 ----------------------------------------------------------------------------------------
    /// The tightest correctness case: a page that interleaves rows from BOTH tiers while BOTH
    /// tiers individually hold far more matches than the bound (heavy eviction in each). This is
    /// where the per-tier `bound = limit + 1` proof is load-bearing: if the bounded heap dropped a
    /// true top-N row from one tier because the *other* tier also contributed to the page, the
    /// paged result would diverge from the unbounded oracle. Asserts order-sensitive byte equality.
    #[test]
    fn parity_both_tiers_over_bound_interleaved() {
        let db = AletheiaDB::new().unwrap();

        // Insert 100 HOT node versions directly at controlled EVEN tx-times (ts 2,4,…,200). Real
        // `db.write` commits cluster at `time::now()` and cannot be interleaved at the microsecond
        // level with small synthetic cold timestamps, so we drive the low-level historical insert
        // to place hot rows exactly between the cold ones.
        let hot_label = GLOBAL_INTERNER.intern("Hot").unwrap();
        {
            let hist = db.__test_historical_storage();
            let mut w = hist.write();
            for i in 0..100u64 {
                let t = ts(2 + (i as i64) * 2); // 2,4,…,200
                w.add_node_version(
                    NodeId::new(60_000 + i).unwrap(),
                    VersionId::new(70_000 + i).unwrap(),
                    t,
                    t,
                    hot_label,
                    PropertyMap::new(),
                    false,
                )
                .unwrap();
            }
        }

        // 100 COLD node versions at controlled ODD tx-times (ts 1,3,…,199).
        let cold_nodes: Vec<_> = (0..100u64)
            .map(|i| cold_node(40_000 + i, 50_000 + i, ts(1 + (i as i64) * 2), "Cold"))
            .collect();
        let _dir = attach_cold(&db, &cold_nodes, &[]);

        let (from, to) = all();
        // Merged cursor order alternates cold(ts1), hot(ts2), cold(ts3), hot(ts4), … so any page of
        // the smallest L (L < 100) interleaves both tiers, and with bound = L+1 ≪ 100 BOTH tiers
        // evict heavily during their scans.
        for limit in [2usize, 5, 10, 25, 50] {
            let page = assert_parity(&db, &query(from, to, limit));
            assert_eq!(page.changes.len(), limit);
            let has_hot = page.changes.iter().any(|r| r.label == "Hot");
            let has_cold = page.changes.iter().any(|r| r.label == "Cold");
            assert!(
                has_hot && has_cold,
                "page must interleave BOTH tiers @limit={limit}"
            );
        }

        // Paginated drain must equal the unbounded scan exactly (order-sensitive, no dup/gap).
        let unbounded = db.list_changes(&query(from, to, 10_000)).unwrap();
        let want: Vec<_> = unbounded
            .changes
            .iter()
            .map(|r| (r.entity_id, r.kind.ord(), r.version_id))
            .collect();
        assert_eq!(want.len(), 200, "all 200 versions visible unbounded");
        let drained = drain_all(&db, &query(from, to, 7));
        assert_eq!(
            drained, want,
            "paged drain equals unbounded across both over-bound tiers"
        );
    }

    // ==========================================================================================
    // Issue #3677: cold-tier directory pushdown — green-phase coverage.
    //
    // These run with the ColdChangeDirectory active (it is always on for an attached cold tier),
    // so they double as the parity regression guard for the pushdown. `attach_cold_with_max` lets
    // a test force a tiny directory budget (eviction) and reach the directory for white-box
    // eligibility probes.
    // ==========================================================================================

    /// Like [`attach_cold`], but with a caller-chosen directory budget; returns the tiered handle
    /// so a test can probe the directory (`eligible_candidates`, `len`, `is_complete`).
    fn attach_cold_with_max(
        db: &AletheiaDB,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        max_entries: usize,
    ) -> (tempfile::TempDir, Arc<TieredStorage>) {
        let dir = tempfile::tempdir().unwrap();
        let cold =
            Arc::new(RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap());
        for v in nodes {
            cold.store_node_version(v).unwrap();
        }
        for v in edges {
            cold.store_edge_version(v).unwrap();
        }
        let config = crate::storage::tiered_storage::TieredStorageConfig {
            cold_change_directory_max_entries: max_entries,
            ..Default::default()
        };
        let tiered = Arc::new(TieredStorage::new(config, cold));
        db.__test_historical_storage()
            .write()
            .set_tiered_storage(tiered.clone());
        (dir, tiered)
    }

    // D1 --------------------------------------------------------------------------------------
    #[test]
    fn cold_directory_parity_hot_only() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..40 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("h{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        // Empty cold tier still activates the directory-backed cold path.
        let _dir = attach_cold(&db, &[], &[]);
        let (from, to) = all();
        for limit in [1usize, 5, 10, 40, 100] {
            assert_parity(&db, &query(from, to, limit));
            assert_parity_independent(&db, &query(from, to, limit));
        }
    }

    // D2 --------------------------------------------------------------------------------------
    #[test]
    fn cold_directory_parity_cold_only() {
        let db = AletheiaDB::new().unwrap();
        let nodes: Vec<_> = (0..20)
            .map(|i| cold_node(1_000 + i, 5_000 + i, ts(100 + i as i64), "ColdNode"))
            .collect();
        let edges: Vec<_> = (0..10)
            .map(|i| cold_edge(2_000 + i, 6_000 + i, ts(200 + i as i64), "ColdEdge"))
            .collect();
        let _dir = attach_cold(&db, &nodes, &edges);
        let (from, to) = all();
        for limit in [1usize, 5, 15, 29, 30, 100] {
            assert_parity(&db, &query(from, to, limit));
            assert_parity_independent(&db, &query(from, to, limit));
        }
    }

    // D3 --------------------------------------------------------------------------------------
    #[test]
    fn cold_directory_parity_mixed_tiers() {
        let db = AletheiaDB::new().unwrap();
        for i in 0..15 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("hot{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let nodes = vec![
            cold_node(1_000, 1_000, ts(50), "Person"),
            cold_node(1_001, 1_001, ts(60), "Company"),
            cold_node_tomb(1_002, 1_003, ts(70), ts(65)),
        ];
        let edges = vec![
            cold_edge(2_000, 2_000, ts(55), "KNOWS"),
            cold_edge(2_001, 2_001, ts(80), "WORKS_AT"),
        ];
        let _dir = attach_cold(&db, &nodes, &edges);
        let (from, to) = all();
        for limit in [1usize, 2, 3, 5, 100] {
            assert_parity(&db, &query(from, to, limit));
            assert_parity_independent(&db, &query(from, to, limit));
        }
        // Cold-only narrow window through the directory.
        assert_parity_independent(&db, &query(ts(40), ts(90), 100));
    }

    // D4 --------------------------------------------------------------------------------------
    /// Directory ordering follows tx-time (`ChangeCursor`), never `version_id`: a smaller
    /// version_id carrying a larger tx-time still pages after. Byte-parity + a small decode count
    /// (only the two window candidates are point-read).
    #[test]
    fn cold_directory_version_id_inversion_parity() {
        let db = AletheiaDB::new().unwrap();
        let earlier_txn_bigger_vid = cold_node(500, 100, ts(1_000), "Cold"); // vid 500, tx 1000
        let later_txn_smaller_vid = cold_node(100, 200, ts(2_000), "Cold"); // vid 100, tx 2000
        let _dir = attach_cold(&db, &[earlier_txn_bigger_vid, later_txn_smaller_vid], &[]);

        let (from, to) = all();
        for limit in [1usize, 2, 3] {
            assert_parity(&db, &query(from, to, limit));
        }
        let drained = drain_all(&db, &query(from, to, 1));
        assert_eq!(
            drained,
            vec![
                (100u64, EntityKind::Node.ord(), 500u64),
                (200u64, EntityKind::Node.ord(), 100u64),
            ],
            "ordering follows tx-time, not version_id"
        );

        // Only the two in-window versions are decoded (point reads), not a full scan.
        crate::storage::redb_cold_storage::reset_cold_decode_counter();
        let _ = db.list_changes(&query(from, to, 10)).unwrap();
        let decoded = crate::storage::redb_cold_storage::cold_decode_count();
        assert!(
            decoded <= 4,
            "expected only window candidates decoded, got {decoded}"
        );
    }

    // D5 --------------------------------------------------------------------------------------
    /// A bounded query early-stops the ascending point-read walk after `bound` survivors, decoding
    /// only ~`bound` versions even though the window matches all 200.
    #[test]
    fn cold_directory_early_stop_bound() {
        let db = AletheiaDB::new().unwrap();
        let nodes: Vec<_> = (1..=200u64)
            .map(|i| cold_node(i, i, ts((i as i64) * 1_000), "Person"))
            .collect();
        let _dir = attach_cold(&db, &nodes, &[]);

        // Whole-timeline window (all 200 match), small limit -> bound = limit + 1.
        let (from, to) = all();
        let limit = 5usize;
        crate::storage::redb_cold_storage::reset_cold_decode_counter();
        let page = db.list_changes(&query(from, to, limit)).unwrap();
        let decoded = crate::storage::redb_cold_storage::cold_decode_count();
        assert_eq!(page.changes.len(), limit);
        assert!(
            decoded <= (limit as u64) + 2,
            "bounded walk must early-stop near bound, decoded {decoded} of 200"
        );
        // And it is still byte-identical to the oracle.
        assert_parity(&db, &query(from, to, limit));
    }

    // D6 --------------------------------------------------------------------------------------
    /// Under a tiny budget a *recent* window (above the eviction watermark) stays on the fast
    /// path: byte-parity and only the window candidates decoded.
    #[test]
    fn cold_directory_partial_coverage_recent_window() {
        let db = AletheiaDB::new().unwrap();
        let nodes: Vec<_> = (1..=200u64)
            .map(|i| cold_node(i, i, ts((i as i64) * 1_000), "Person"))
            .collect();
        // Budget 10 -> newest 10 retained (i = 191..=200), i = 1..=190 evicted.
        let (_dir, tiered) = attach_cold_with_max(&db, &nodes, &[], 10);
        assert!(!tiered.cold_change_directory().is_complete());
        assert_eq!(tiered.cold_change_directory().len(), 10);

        // Recent window matching i in {196..=200}, entirely above the watermark.
        let q = query(ts(195_500), ts(201_000), 5);
        assert!(
            tiered
                .cold_change_directory()
                .eligible_candidates(&TimeRange::new(ts(195_500), ts(201_000)).unwrap(), None)
                .is_some(),
            "recent window must remain eligible under eviction"
        );

        crate::storage::redb_cold_storage::reset_cold_decode_counter();
        let page = db.list_changes(&q).unwrap();
        let decoded = crate::storage::redb_cold_storage::cold_decode_count();
        assert_eq!(page.changes.len(), 5);
        assert!(
            decoded <= 20,
            "recent-window query stays windowed under a tiny budget, decoded {decoded}"
        );
        assert_parity(&db, &q);
    }

    // D7 --------------------------------------------------------------------------------------
    /// Under the same tiny budget an *old* window (below the watermark) degrades to the full scan:
    /// still byte-correct, and confirmed to have taken the scan path (all versions decoded, and
    /// `eligible_candidates` returns `None`).
    #[test]
    fn cold_directory_degrade_over_budget_parity() {
        let db = AletheiaDB::new().unwrap();
        let nodes: Vec<_> = (1..=200u64)
            .map(|i| cold_node(i, i, ts((i as i64) * 1_000), "Person"))
            .collect();
        let (_dir, tiered) = attach_cold_with_max(&db, &nodes, &[], 10);

        // Old window matching i in {1..=5}, below the eviction watermark -> must degrade.
        let old_window = TimeRange::new(ts(500), ts(5_500)).unwrap();
        assert!(
            tiered
                .cold_change_directory()
                .eligible_candidates(&old_window, None)
                .is_none(),
            "old window below watermark must degrade to scan"
        );

        let q = query(ts(500), ts(5_500), 100);
        crate::storage::redb_cold_storage::reset_cold_decode_counter();
        let page = db.list_changes(&q).unwrap();
        let decoded = crate::storage::redb_cold_storage::cold_decode_count();
        assert_eq!(
            page.changes.len(),
            5,
            "window matches exactly 5 old versions"
        );
        assert!(
            decoded >= 200,
            "degrade path performs the full cold scan, decoded {decoded}"
        );
        assert_parity(&db, &q);
    }

    // D8 --------------------------------------------------------------------------------------
    /// The directory is seeded from the cold tier on construction (startup / attach), so a
    /// freshly-built `TieredStorage` over a populated cold store answers with parity.
    #[test]
    fn cold_directory_startup_rebuild_parity() {
        let db = AletheiaDB::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cold =
            Arc::new(RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap());
        let nodes: Vec<_> = (0..25)
            .map(|i| cold_node(3_000 + i, 7_000 + i, ts(10 + i as i64), "Cold"))
            .collect();
        let edges: Vec<_> = (0..15)
            .map(|i| cold_edge(4_000 + i, 8_000 + i, ts(300 + i as i64), "ColdEdge"))
            .collect();
        for v in &nodes {
            cold.store_node_version(v).unwrap();
        }
        for v in &edges {
            cold.store_edge_version(v).unwrap();
        }

        // Fresh construction over the already-populated cold Arc: `new` rebuilds the directory.
        let tiered = Arc::new(TieredStorage::with_default_config(cold));
        assert_eq!(
            tiered.cold_change_directory().len(),
            nodes.len() + edges.len(),
            "directory seeded from cold on construction"
        );
        assert!(tiered.cold_change_directory().is_complete());
        db.__test_historical_storage()
            .write()
            .set_tiered_storage(tiered);

        let (from, to) = all();
        for limit in [1usize, 5, 20, 40, 100] {
            assert_parity(&db, &query(from, to, limit));
            assert_parity_independent(&db, &query(from, to, limit));
        }
        drop(dir);
    }

    // D9 --------------------------------------------------------------------------------------
    /// Directory correctness under concurrent hot→cold migration (AC4): writers create+update
    /// nodes, a migrator loops `migrate_to_cold`, and a reader pages `list_changes` — all
    /// concurrently. Each concurrent drain is duplicate-free, and after quiescence the paged drain
    /// equals the unbounded ground truth (no gap, no dup).
    #[test]
    fn cold_directory_concurrent_migration_stress() {
        use crate::storage::migration::{MigrationPolicy, MigrationService};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let db = Arc::new(AletheiaDB::new().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let cold =
            Arc::new(RedbColdStorage::with_default_config(dir.path().join("cold.redb")).unwrap());
        let tiered = Arc::new(TieredStorage::with_default_config(cold.clone()));
        db.__test_historical_storage()
            .write()
            .set_tiered_storage(tiered);
        // age_threshold 0 => every non-head version is a migration candidate immediately.
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::ZERO)
            .min_hot_versions(1)
            .batch_size(1_000)
            .enabled(true)
            .build();
        let service = Arc::new(MigrationService::new(cold, policy));

        let stop = Arc::new(AtomicBool::new(false));
        let writers_done = Arc::new(AtomicBool::new(false));

        const WRITERS: u64 = 2;
        const PER_WRITER: u64 = 60;

        // Writers: create + update each node (2 versions; the anchor becomes migratable).
        let mut writer_handles = Vec::new();
        for w in 0..WRITERS {
            let db = db.clone();
            writer_handles.push(std::thread::spawn(move || {
                for i in 0..PER_WRITER {
                    let name = format!("w{w}n{i}");
                    let (id, _t) = db
                        .write_with_timestamp(|tx| tx.create_node("Person", props(&name)))
                        .unwrap();
                    db.write(|tx| {
                        tx.update_node(id, props(&format!("{name}-v2")))?;
                        Ok::<_, Error>(())
                    })
                    .unwrap();
                }
            }));
        }

        // Migrator: loop migrate_to_cold until told to stop.
        let migrator = {
            let db = db.clone();
            let service = service.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = db
                        .__test_historical_storage()
                        .write()
                        .migrate_to_cold(&service);
                    std::thread::sleep(Duration::from_micros(200));
                }
            })
        };

        // Reader: drain repeatedly, concurrent with writes + migration; each drain is dup-free.
        let (from, to) = all();
        let reader = {
            let db = db.clone();
            let writers_done = writers_done.clone();
            std::thread::spawn(move || {
                let mut rounds = 0usize;
                loop {
                    let drained = drain_all(&db, &query(from, to, 7));
                    let mut seen = std::collections::HashSet::new();
                    for k in &drained {
                        assert!(
                            seen.insert(*k),
                            "duplicate (entity,kind,version) within one concurrent drain: {k:?}"
                        );
                    }
                    rounds += 1;
                    if writers_done.load(Ordering::Relaxed) && rounds > 3 {
                        break;
                    }
                }
            })
        };

        for h in writer_handles {
            h.join().unwrap();
        }
        writers_done.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        migrator.join().unwrap();

        // Quiesce: migrate everything possible, then compare a paged drain to the unbounded truth.
        let _ = db
            .__test_historical_storage()
            .write()
            .migrate_to_cold(&service);
        let unbounded = db.list_changes(&query(from, to, 1_000_000)).unwrap();
        let want: Vec<_> = unbounded
            .changes
            .iter()
            .map(|r| (r.entity_id, r.kind.ord(), r.version_id))
            .collect();
        assert_eq!(
            want.len() as u64,
            WRITERS * PER_WRITER * 2,
            "every created + modified version is visible exactly once"
        );
        let drained = drain_all(&db, &query(from, to, 11));
        assert_eq!(
            drained, want,
            "paged drain equals unbounded across concurrent migration (no gap, no dup)"
        );
        drop(dir);
    }

    /// Bench-adjacent (honest): print the candidate working-set size and wall time of the
    /// optimized path vs the unbounded reference oracle for a bounded/limited query over a large
    /// hot history. Ignored by default; run with `--ignored --nocapture` to capture PR numbers.
    #[test]
    #[ignore = "bench-adjacent; run explicitly to capture numbers"]
    fn bench_list_changes_pushdown() {
        use std::time::Instant;
        let db = AletheiaDB::new().unwrap();
        let n = 20_000;
        for i in 0..n {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("n{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let (from, to) = all();
        let q = query(from, to, 10);

        // Warm both paths once.
        let _ = db.list_changes(&q).unwrap();
        let _ = list_changes_reference(&db, &q).unwrap();

        let iters = 50;
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = list_changes_reference(&db, &q).unwrap();
        }
        let ref_elapsed = t0.elapsed() / iters;

        let t1 = Instant::now();
        for _ in 0..iters {
            let _ = db.list_changes(&q).unwrap();
        }
        let new_elapsed = t1.elapsed() / iters;
        let candidates = LAST_LIST_CHANGES_CANDIDATES.with(|c| c.get());

        println!("=== list_changes pushdown bench ({n} hot versions, limit 10) ===");
        println!("reference (unbounded) : {ref_elapsed:?}/call");
        println!("pushed-down (new)     : {new_elapsed:?}/call");
        println!("new-path candidate working set: {candidates} rows (was {n})");
    }
}
