use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::{Timestamp, time};
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
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_at_time").entered();

        self.historical
            .read()
            .get_node_at_time(node_id, valid_time, transaction_time)
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility with historical storage (handles closed intervals from deletions).
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edge_at_time").entered();

        self.historical
            .read()
            .get_edge_at_time(edge_id, valid_time, transaction_time)
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
    /// ```ignore
    /// // Query 100 nodes at a historical point with single lock acquisition
    /// let node_ids = vec![node1, node2, /* ... */, node100];
    /// let results = db.get_nodes_at_time(&node_ids, valid_time, tx_time)?;
    ///
    /// for (node_id, node_opt) in results {
    ///     if let Some(node) = node_opt {
    ///         println!("Node {} existed with properties: {:?}", node_id, node.properties());
    ///     } else {
    ///         println!("Node {} did not exist at this time", node_id);
    ///     }
    /// }
    /// ```
    pub fn get_nodes_at_time(
        &self,
        node_ids: &[NodeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(NodeId, Option<Node>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_nodes_at_time").entered();

        self.historical
            .read()
            .get_nodes_at_time(node_ids, valid_time, transaction_time)
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
    /// ```ignore
    /// // Query multiple edges at a historical point with single lock acquisition
    /// let edge_ids = vec![edge1, edge2, edge3];
    /// let results = db.get_edges_at_time(&edge_ids, valid_time, tx_time)?;
    ///
    /// for (edge_id, edge_opt) in results {
    ///     if let Some(edge) = edge_opt {
    ///         println!("Edge {} existed: {} -> {}", edge_id, edge.source, edge.target);
    ///     } else {
    ///         println!("Edge {} did not exist at this time", edge_id);
    ///     }
    /// }
    /// ```
    pub fn get_edges_at_time(
        &self,
        edge_ids: &[EdgeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(EdgeId, Option<Edge>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edges_at_time").entered();

        self.historical
            .read()
            .get_edges_at_time(edge_ids, valid_time, transaction_time)
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
    /// ```ignore
    /// // "What were Alice's properties on January 15th?"
    /// let node = db.get_node_at_valid_time(alice_id, jan_15)?;
    /// ```
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
    /// ```ignore
    /// // "What did we know about Alice on February 1st?"
    /// let node = db.get_node_at_transaction_time(alice_id, feb_1)?;
    /// ```
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
    /// ```ignore
    /// let history = db.get_node_history(alice_id)?;
    /// println!("Alice has {} versions", history.version_count());
    /// ```
    pub fn get_node_history(&self, node_id: NodeId) -> Result<EntityHistory> {
        self.historical.read().get_node_history(node_id)
    }

    /// Get a node at a specific logical version number.
    ///
    /// Version numbers are 1-indexed (1 = first version, 2 = second version, etc.).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let v1 = db.get_node_at_version(alice_id, 1)?;  // Original version
    /// let v2 = db.get_node_at_version(alice_id, 2)?;  // After first update
    /// ```
    pub fn get_node_at_version(&self, node_id: NodeId, version_number: u64) -> Result<Node> {
        self.historical
            .read()
            .get_node_at_version(node_id, version_number)
    }

    /// Compute the difference between two versions of a node.
    ///
    /// Shows which properties were added, removed, or modified.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let history = db.get_node_history(alice_id)?;
    /// let v1 = history.first_version().unwrap().version_id;
    /// let v2 = history.current_version().unwrap().version_id;
    ///
    /// let diff = db.diff_node_versions(alice_id, v1, v2)?;
    /// if diff.has_changes() {
    ///     println!("Properties changed: {}", diff.change_count());
    /// }
    /// ```
    pub fn diff_node_versions(
        &self,
        node_id: NodeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        self.historical
            .read()
            .diff_node_versions(node_id, from_version, to_version)
    }

    /// Get an edge at a specific valid time.
    ///
    /// Query by valid time only (transaction time defaults to now).
    pub fn get_edge_at_valid_time(&self, edge_id: EdgeId, valid_time: Timestamp) -> Result<Edge> {
        let transaction_time = time::now();
        self.get_edge_at_time(edge_id, valid_time, transaction_time)
    }

    /// Get an edge at a specific transaction time.
    ///
    /// Query by transaction time only (valid time defaults to now).
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
    pub fn get_edge_history(&self, edge_id: EdgeId) -> Result<EntityHistory> {
        self.historical.read().get_edge_history(edge_id)
    }

    /// Compute the difference between two versions of an edge.
    ///
    /// Shows which properties were added, removed, or modified.
    pub fn diff_edge_versions(
        &self,
        edge_id: EdgeId,
        from_version: VersionId,
        to_version: VersionId,
    ) -> Result<VersionDiff> {
        self.historical
            .read()
            .diff_edge_versions(edge_id, from_version, to_version)
    }
}
