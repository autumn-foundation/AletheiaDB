//! Basic graph operations for creating and reading nodes and edges.
//!
//! Contains convenience methods for the most common graph operations.
use crate::api::transaction::WriteOps;
use crate::core::error::{Result, ResultExt};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::{PropertyMap, PropertyValue};
use crate::db::AletheiaDB;
use crate::storage::current::{IncomingEdgesIter, OutgoingEdgesIter};

impl AletheiaDB {
    /// Create a node with the given label and properties.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
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
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// let node_id = db.create_node("Person", properties! {
    ///     "name" => "Alice",
    ///     "age" => 30,
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// let edge_id = db.create_edge(source_id, target_id, "WORKS_FOR", properties! {
    ///     "role" => "Engineer"
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # See Also
    ///
    /// * [`write`](Self::write) - For batched write operations.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
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
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
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
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// let node = db.get_node(node_id)?;
    /// assert!(node.has_label_str("Person"));
    /// # Ok(())
    /// # }
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// let is_person = db.with_node(node_id, |node| node.has_label_str("Person"))?;
    /// assert!(is_person);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_node<F, R>(&self, id: NodeId, f: F) -> Result<R>
    where
        F: FnOnce(&Node) -> R,
    {
        self.current.with_node(id, f).record_error_metric()
    }

    /// Get the current state of an edge.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # let edge_id = db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let edge = db.get_edge(edge_id)?;
    /// assert!(edge.has_label_str("WORKS_FOR"));
    /// # Ok(())
    /// # }
    /// ```
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
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # db.create_node("Person", properties! { "name" => "Bob" })?;
    /// let people: Vec<_> = db.scan_nodes_by_label("Person").collect();
    /// assert_eq!(people.len(), 2);
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
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # let edge_id = db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let retrieved_target = db.get_edge_target(edge_id)?;
    /// assert_eq!(retrieved_target, target_id);
    /// # Ok(())
    /// # }
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # let edge_id = db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let retrieved_source = db.get_edge_source(edge_id)?;
    /// assert_eq!(retrieved_source, source_id);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_source(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.current.get_edge_source(edge_id).record_error_metric()
    }

    /// Get outgoing edges from a node (current state).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let outgoing = db.get_outgoing_edges(source_id);
    /// assert_eq!(outgoing.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_outgoing_edges(node_id)
    }

    /// Get outgoing edges from a node as an iterator (current state).
    ///
    /// This provides zero-allocation traversal.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let count = db.get_outgoing_edges_iter(source_id).count();
    /// assert_eq!(count, 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_outgoing_edges_iter(&self, node_id: NodeId) -> OutgoingEdgesIter<'_> {
        self.current.get_outgoing_edges_iter(node_id)
    }

    /// Get incoming edges to a node (current state).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let incoming = db.get_incoming_edges(target_id);
    /// assert_eq!(incoming.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_incoming_edges(node_id)
    }

    /// Get incoming edges to a node as an iterator (current state).
    ///
    /// This provides zero-allocation traversal.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let count = db.get_incoming_edges_iter(target_id).count();
    /// assert_eq!(count, 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_incoming_edges_iter(&self, node_id: NodeId) -> IncomingEdgesIter<'_> {
        self.current.get_incoming_edges_iter(node_id)
    }

    /// Get incoming edges with a specific label (current state).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let incoming = db.get_incoming_edges_with_label(target_id, "WORKS_FOR");
    /// assert_eq!(incoming.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_incoming_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_incoming_edges_with_label(node_id, label)
    }

    /// Get outgoing edges with a specific label (current state).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let outgoing = db.get_outgoing_edges_with_label(source_id, "WORKS_FOR");
    /// assert_eq!(outgoing.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    /// Get the number of nodes in the current state.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// let count = db.node_count();
    /// assert_eq!(count, 2);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn node_count(&self) -> usize {
        self.current.node_count()
    }

    /// Get all node IDs currently in the database.
    ///
    /// Returns a snapshot of all live node IDs. For large graphs prefer
    /// [`scan_nodes_by_label`](Self::scan_nodes_by_label) to avoid loading
    /// the full set into memory.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # db.create_node("Person", properties! { "name" => "Bob" })?;
    /// let all_ids = db.get_all_node_ids();
    /// assert_eq!(all_ids.len(), 2);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn get_all_node_ids(&self) -> Vec<NodeId> {
        self.current.get_all_node_ids()
    }

    /// Get the number of edges in the current state.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let count = db.edge_count();
    /// assert_eq!(count, 1);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.current.edge_count()
    }

    /// Get the out-degree of a node (current state).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let degree = db.out_degree(source_id);
    /// assert_eq!(degree, 1);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn out_degree(&self, node_id: NodeId) -> usize {
        self.current.out_degree(node_id)
    }

    /// Get the in-degree of a node (current state).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let source_id = db.create_node("Person", properties! { "name" => "Alice" })?;
    /// # let target_id = db.create_node("Company", properties! { "name" => "Acme Corp" })?;
    /// # db.create_edge(source_id, target_id, "WORKS_FOR", properties! {})?;
    /// let degree = db.in_degree(target_id);
    /// assert_eq!(degree, 1);
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn in_degree(&self, node_id: NodeId) -> usize {
        self.current.in_degree(node_id)
    }

    /// Find nodes by label and property value (current state).
    ///
    /// Returns the IDs of all nodes with the given label whose specified property
    /// equals the given value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::{AletheiaDB, properties, PropertyValue};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # db.create_node("Person", properties! { "name" => "Alice", "age" => 30 })?;
    /// # db.create_node("Person", properties! { "name" => "Bob", "age" => 30 })?;
    /// let thirty_year_olds = db.find_nodes_by_property("Person", "age", &PropertyValue::Int(30));
    /// assert_eq!(thirty_year_olds.len(), 2);
    /// # Ok(())
    /// # }
    /// ```
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
}
