//! Defines core traits used throughout the query execution engine.

use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::temporal::Timestamp;

/// A trait representing a read-only view of the graph.
///
/// This trait abstracts the underlying database implementation, allowing
/// query algorithms to operate on any source that provides graph access.
/// It effectively decouples the query engine from the storage engine.
pub trait GraphView {
    /// Get a node by ID.
    fn get_node(&self, id: NodeId) -> Result<Node>;

    /// Get an edge by ID.
    fn get_edge(&self, id: EdgeId) -> Result<Edge>;

    /// Get the target node ID of an edge.
    fn get_edge_target(&self, edge_id: EdgeId) -> Result<NodeId>;

    /// Get the source node ID of an edge.
    fn get_edge_source(&self, edge_id: EdgeId) -> Result<NodeId>;

    /// Get outgoing edges from a node.
    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId>;

    /// Get incoming edges to a node.
    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId>;

    /// Get outgoing edges with a specific label.
    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId>;

    // Temporal methods

    /// Get a node as it existed at a specific time.
    fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node>;

    /// Get an edge as it existed at a specific time.
    fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge>;

    /// Get outgoing edges valid at a specific time.
    fn get_outgoing_edges_at_time(
        &self,
        source: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId>;

    /// Get incoming edges valid at a specific time.
    fn get_incoming_edges_at_time(
        &self,
        target: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId>;

    // Vector methods

    /// Find k most similar nodes at a specific point in time.
    fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>>;
}

/// A trait for executing generic queries against the database.
pub trait QueryEngine: GraphView {
    /// Execute a built query against the engine.
    fn execute_query(&self, query: crate::query::Query) -> Result<crate::query::QueryResults>;
}

impl<T: GraphView + ?Sized> GraphView for std::sync::Arc<T> {
    fn get_node(&self, id: NodeId) -> Result<Node> {
        (**self).get_node(id)
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        (**self).get_edge(id)
    }

    fn get_edge_target(&self, edge_id: EdgeId) -> Result<NodeId> {
        (**self).get_edge_target(edge_id)
    }

    fn get_edge_source(&self, edge_id: EdgeId) -> Result<NodeId> {
        (**self).get_edge_source(edge_id)
    }

    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        (**self).get_outgoing_edges(node_id)
    }

    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        (**self).get_incoming_edges(node_id)
    }

    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        (**self).get_outgoing_edges_with_label(node_id, label)
    }

    fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        (**self).get_node_at_time(node_id, valid_time, transaction_time)
    }

    fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        (**self).get_edge_at_time(edge_id, valid_time, transaction_time)
    }

    fn get_outgoing_edges_at_time(
        &self,
        source: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        (**self).get_outgoing_edges_at_time(source, valid_time, tx_time)
    }

    fn get_incoming_edges_at_time(
        &self,
        target: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Vec<EdgeId> {
        (**self).get_incoming_edges_at_time(target, valid_time, tx_time)
    }

    fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        (**self).find_similar_as_of(embedding, k, timestamp)
    }
}

impl<T: QueryEngine + ?Sized> QueryEngine for std::sync::Arc<T> {
    fn execute_query(&self, query: crate::query::Query) -> Result<crate::query::QueryResults> {
        (**self).execute_query(query)
    }
}
