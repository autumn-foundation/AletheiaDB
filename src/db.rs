//! Main GallifreyDB database API.
//!
//! This module provides the primary interface to the database, coordinating
//! between current storage (fast path) and historical storage (temporal path).

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::PropertyMap;
use crate::core::temporal::{time, BiTemporalInterval, Timestamp};
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::version::AnchorConfig;
use crate::utils::error::{Result, StorageError};

/// Main GallifreyDB database.
///
/// This is the primary entry point for interacting with the database.
/// It coordinates between current storage (for fast current-state queries)
/// and historical storage (for temporal queries).
pub struct GallifreyDB {
    /// Current state storage (hot path)
    current: CurrentStorage,
    /// Historical version storage (temporal path)
    historical: HistoricalStorage,
    /// Temporal indexes for efficient time-based queries
    temporal_indexes: TemporalIndexes,
    /// Current logical timestamp for transaction time
    current_timestamp: Timestamp,
}

impl GallifreyDB {
    /// Create a new empty database with default configuration.
    pub fn new() -> Self {
        Self::with_config(AnchorConfig::default())
    }

    /// Create a new database with custom anchor configuration.
    pub fn with_config(config: AnchorConfig) -> Self {
        GallifreyDB {
            current: CurrentStorage::new(),
            historical: HistoricalStorage::with_config(config),
            temporal_indexes: TemporalIndexes::new(),
            current_timestamp: time::now(),
        }
    }

    /// Create a node with the given label and properties.
    ///
    /// The node is created at the current timestamp in both valid and transaction time.
    pub fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        let timestamp = self.next_timestamp();
        let temporal = BiTemporalInterval::current(timestamp);

        // Create in current storage
        let node_id = self.current.create_node(label, properties.clone())?;

        // Get the version ID from the current node
        let node = self.current.get_node(node_id)?;
        let version_id = node.current_version;

        // Store in historical storage
        let label_interned = crate::core::interning::GLOBAL_INTERNER.intern(label);
        self.historical.add_node_version(
            node_id,
            version_id,
            temporal,
            label_interned,
            properties,
        )?;

        // Index in temporal indexes
        self.temporal_indexes
            .insert_node_version(node_id, version_id, temporal);

        Ok(node_id)
    }

    /// Create an edge between two nodes.
    pub fn create_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        let timestamp = self.next_timestamp();
        let temporal = BiTemporalInterval::current(timestamp);

        // Create in current storage
        let edge_id = self
            .current
            .create_edge(source, target, label, properties.clone())?;

        // Get the version ID from the current edge
        let edge = self.current.get_edge(edge_id)?;
        let version_id = edge.current_version;

        // Store in historical storage
        let label_interned = crate::core::interning::GLOBAL_INTERNER.intern(label);
        self.historical.add_edge_version(
            edge_id,
            version_id,
            temporal,
            label_interned,
            source,
            target,
            properties,
        )?;

        // Index in temporal indexes
        self.temporal_indexes
            .insert_edge_version(edge_id, version_id, temporal);

        Ok(edge_id)
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

    /// Get outgoing edges from a node (current state).
    pub fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_outgoing_edges(node_id)
    }

    /// Get incoming edges to a node (current state).
    pub fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_incoming_edges(node_id)
    }

    /// Get outgoing edges with a specific label (current state).
    pub fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    /// Get a node as it existed at a specific point in bi-temporal space.
    ///
    /// This uses the slow path (historical storage + version reconstruction).
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        // Find the version valid at this time
        let version_id = self
            .historical
            .find_node_version_at_time(node_id, valid_time, transaction_time)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Get the version
        let version = self
            .historical
            .get_node_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Reconstruct properties
        let properties = self.historical.reconstruct_node_properties(version_id)?;

        // Build node from version
        Ok(Node::new(
            version.node_id,
            version.label,
            properties,
            version.id,
        ))
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        let version_id = self
            .historical
            .find_edge_version_at_time(edge_id, valid_time, transaction_time)
            .ok_or(StorageError::EdgeNotFound(edge_id))?;

        let version = self
            .historical
            .get_edge_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = self.historical.reconstruct_edge_properties(version_id)?;

        Ok(Edge::new(
            version.edge_id,
            version.label,
            version.source,
            version.target,
            properties,
            version.id,
        ))
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

    /// Get statistics about the historical storage.
    pub fn historical_stats(&self) -> crate::storage::historical::HistoricalStats {
        self.historical.stats()
    }

    /// Get the next timestamp and increment the counter.
    fn next_timestamp(&mut self) -> Timestamp {
        let ts = self.current_timestamp;
        self.current_timestamp += 1;
        ts
    }
}

impl Default for GallifreyDB {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_node() {
        let mut db = GallifreyDB::new();

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = db.create_node("Person", props).unwrap();

        assert_eq!(db.node_count(), 1);

        let node = db.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_create_edge() {
        let mut db = GallifreyDB::new();

        let alice = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = db
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        assert_eq!(db.edge_count(), 1);

        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, alice);
        assert_eq!(edge.target, bob);
    }

    #[test]
    fn test_time_travel_query() {
        let mut db = GallifreyDB::new();

        // Create a node at time T1
        let props_v1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = db.create_node("Person", props_v1).unwrap();
        let t1 = db.current_timestamp - 1; // Timestamp when created

        // In a real implementation, we'd create a second version here with an update_node method
        // For now, just verify we can query at T1

        // Query at time T1
        let historical_node = db.get_node_at_time(node_id, t1, t1).unwrap();
        assert_eq!(
            historical_node.get_property("age").and_then(|v| v.as_int()),
            Some(30)
        );

        // Query current state
        let current_node = db.get_node(node_id).unwrap();
        assert_eq!(
            current_node.get_property("age").and_then(|v| v.as_int()),
            Some(30)
        );
    }

    #[test]
    fn test_graph_traversal() {
        let mut db = GallifreyDB::new();

        let n0 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        db.create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let outgoing = db.get_outgoing_edges(n0);
        assert_eq!(outgoing.len(), 2);

        let knows_edges = db.get_outgoing_edges_with_label(n0, "KNOWS");
        assert_eq!(knows_edges.len(), 2);
    }

    #[test]
    fn test_historical_stats() {
        let mut db = GallifreyDB::new();

        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let stats = db.historical_stats();
        assert_eq!(stats.total_node_versions, 2);
        assert_eq!(stats.node_anchor_count, 2); // First versions are always anchors
    }
}
