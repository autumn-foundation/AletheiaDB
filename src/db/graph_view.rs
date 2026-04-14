//! Implementation of the `GraphView` trait for `AletheiaDB`.
//!
//! This module adapts the main database struct to the `GraphView` trait,
//! allowing it to be used in query execution and other components that
//! require a generic view of the graph.
//!
//! The `GraphView` trait provides a unified interface for accessing graph elements
//! (nodes and edges) both in the current state and at specific points in time.

use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::query::traits::GraphView;

/// Implementation of `GraphView` for `AletheiaDB`.
///
/// This implementation delegates all calls to the underlying `AletheiaDB` methods,
/// exposing the database's full capabilities through the `GraphView` interface.
///
/// # Examples
///
/// ```rust
/// use aletheiadb::AletheiaDB;
/// use aletheiadb::query::traits::GraphView;
/// use aletheiadb::core::id::NodeId;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let db = AletheiaDB::new()?;
///
/// // Use the database as a GraphView
/// let view: &dyn GraphView = &db;
///
/// // Access graph elements through the trait
/// // let node = view.get_node(NodeId::new(1)?)?;
/// # Ok(())
/// # }
/// ```
impl GraphView for AletheiaDB {
    fn get_node(&self, id: NodeId) -> Result<Node> {
        self.get_node(id)
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        self.get_edge(id)
    }

    fn get_edge_target(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.get_edge_target(edge_id)
    }

    fn get_edge_source(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.get_edge_source(edge_id)
    }

    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.get_outgoing_edges(node_id)
    }

    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.get_incoming_edges(node_id)
    }

    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.get_outgoing_edges_with_label(node_id, label)
    }

    fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        self.get_node_at_time(node_id, valid_time, transaction_time)
    }

    fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        self.get_edge_at_time(edge_id, valid_time, transaction_time)
    }

    fn get_outgoing_edges_at_time(
        &self,
        source: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        self.get_outgoing_edges_at_time(source, valid_time, tx_time)
    }

    fn get_incoming_edges_at_time(
        &self,
        target: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        self.get_incoming_edges_at_time(target, valid_time, tx_time)
    }

    fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.find_similar_as_of(embedding, k, timestamp)
    }
}
