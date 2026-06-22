//! Temporal query operations for bi-temporal data.
//!
//! Methods for querying historical states of the graph using valid and transaction times.
use crate::core::changefeed::{
    ChangeCursor, ChangeFeedPage, ChangeFeedQuery, ChangeRecord, EntityKind, RawChange,
    build_raw_change,
};
use crate::core::error::{Result, ResultExt};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::{TimeRange, Timestamp, time};
use crate::db::AletheiaDB;
use crate::query::{EntityHistory, VersionDiff};

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

        self.historical
            .read()
            .get_node_at_time(node_id, valid_time, transaction_time)
            .record_error_metric()
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

        self.historical
            .read()
            .get_edge_at_time(edge_id, valid_time, transaction_time)
            .record_error_metric()
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

        // Scan the hot tier under the read lock, but keep the lock hold short: `collect_changes`
        // returns lightweight `RawChange`s (no label allocation), and the cold-tier scan (disk
        // I/O) happens after the lock is released using the cloned tiered handle.
        let (mut changes, tiered) = {
            let hist = self.historical.read();
            (
                hist.collect_changes(&tx_window, valid, label),
                hist.tiered_storage_arc(),
            )
        };

        // Include versions that have migrated out of the hot maps into cold storage, so the feed
        // is complete when tiered storage is enabled. Dedup against hot by (kind, version_id) in
        // case a version is transiently present in both tiers during migration.
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
                ) && seen.insert((rec.cursor.kind_ord, rec.cursor.version_id))
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
                ) && seen.insert((rec.cursor.kind_ord, rec.cursor.version_id))
                {
                    changes.push(rec);
                }
            }
        }

        // A page must be able to carry a continuation cursor, so it can never be empty while more
        // rows exist: treat limit 0 as 1.
        let limit = query.limit.max(1);

        // Resume strictly after the cursor key (the precomputed `cursor` is a Copy field, so no
        // per-element recomputation).
        if let Some(c) = cursor {
            changes.retain(|record| record.cursor > c);
        }

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
