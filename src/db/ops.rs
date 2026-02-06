use crate::api::transaction::WriteOps;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::{PropertyMap, PropertyValue};
use crate::db::AletheiaDB;
use crate::storage::current::{IncomingEdgesIter, OutgoingEdgesIter};
use crate::utils::error::Result;

impl AletheiaDB {
    /// Create a node with the given label and properties.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::PropertyMapBuilder;
    ///
    /// let node_id = db.create_node(
    ///     "Person",
    ///     PropertyMapBuilder::new()
    ///         .insert("name", "Alice")
    ///         .build()
    /// )?;
    /// ```
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        self.write(|tx| tx.create_node(label, properties))
    }

    /// Create an edge between two nodes.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::PropertyMapBuilder;
    ///
    /// let edge_id = db.create_edge(
    ///     source_id,
    ///     target_id,
    ///     "KNOWS",
    ///     PropertyMapBuilder::new().insert("since", 2024).build()
    /// )?;
    /// ```
    pub fn create_edge(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        self.write(|tx| tx.create_edge(source, target, label, properties))
    }

    /// Get the current state of a node.
    ///
    /// This uses the fast path (current storage) for O(1) lookup.
    pub fn get_node(&self, node_id: NodeId) -> Result<Node> {
        self.current.get_node(node_id)
    }

    /// Get the current state of an edge.
    pub fn get_edge(&self, edge_id: EdgeId) -> Result<Edge> {
        self.current.get_edge(edge_id)
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
    /// ```ignore
    /// // Process all Person nodes
    /// for node_id in db.scan_nodes_by_label("Person") {
    ///     let node = db.get_node(node_id)?;
    ///     println!("Person: {:?}", node.properties.get("name"));
    /// }
    ///
    /// // Count nodes by label
    /// let person_count = db.scan_nodes_by_label("Person").count();
    /// let product_count = db.scan_nodes_by_label("Product").count();
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
    pub fn get_edge_target(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.current.get_edge_target(edge_id)
    }

    /// Get the source node of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the source NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_source(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.current.get_edge_source(edge_id)
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

    /// Get outgoing edges with a specific label (current state).
    pub fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    /// Get the number of nodes in the current state.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.current.node_count()
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
}
