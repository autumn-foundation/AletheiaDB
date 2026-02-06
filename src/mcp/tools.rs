//! Tool definitions for the GallifreyDB MCP server.
//!
//! This module defines all the MCP tools that expose GallifreyDB functionality
//! to LLM clients. Each tool is defined with JSON Schema-compatible request types.

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Node Operations
// ============================================================================

/// Request to get a node by its ID.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetNodeRequest {
    /// The unique identifier of the node (u64).
    #[schemars(description = "The unique identifier of the node")]
    pub node_id: u64,
}

/// Request to create a new node.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateNodeRequest {
    /// The label/type of the node (e.g., "Person", "Document").
    #[schemars(description = "The label/type of the node (e.g., 'Person', 'Document')")]
    pub label: String,

    /// Properties to set on the node as key-value pairs.
    #[schemars(description = "Properties to set on the node as key-value pairs")]
    pub properties: Option<HashMap<String, serde_json::Value>>,
}

/// Request to update an existing node's properties.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdateNodeRequest {
    /// The unique identifier of the node to update.
    #[schemars(description = "The unique identifier of the node to update")]
    pub node_id: u64,

    /// New properties to set (replaces all existing properties).
    #[schemars(description = "New properties to set (replaces all existing properties)")]
    pub properties: HashMap<String, serde_json::Value>,
}

/// Request to delete a node.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeleteNodeRequest {
    /// The unique identifier of the node to delete.
    #[schemars(description = "The unique identifier of the node to delete")]
    pub node_id: u64,
}

/// Request to delete a node and all its connected edges (cascade delete).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeleteNodeCascadeRequest {
    /// The unique identifier of the node to delete.
    #[schemars(
        description = "The unique identifier of the node to delete along with all its connected edges"
    )]
    pub node_id: u64,
}

/// Request to list nodes with optional filtering.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListNodesRequest {
    /// Filter by node label (optional).
    #[schemars(description = "Filter by node label (optional)")]
    pub label: Option<String>,

    /// Filter by property key (requires label and property_value).
    #[schemars(
        description = "Filter by property key. Must be used with 'label' and 'property_value'."
    )]
    pub property_key: Option<String>,

    /// Filter by property value (requires label and property_key).
    /// Supports: strings, integers, floats, booleans, and null.
    #[schemars(
        description = "Filter by property value (JSON). Must be used with 'label' and 'property_key'."
    )]
    pub property_value: Option<serde_json::Value>,

    /// Maximum number of nodes to return (default: 100).
    #[schemars(description = "Maximum number of nodes to return (default: 100)")]
    pub limit: Option<usize>,

    /// Number of nodes to skip (for pagination).
    #[schemars(description = "Number of nodes to skip (for pagination)")]
    pub offset: Option<usize>,
}

/// Request to count nodes.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CountNodesRequest {
    /// Filter by node label (optional).
    #[schemars(description = "Filter by node label (optional, if not provided counts all nodes)")]
    pub label: Option<String>,
}

// ============================================================================
// Edge Operations
// ============================================================================

/// Request to get an edge by its ID.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetEdgeRequest {
    /// The unique identifier of the edge (u64).
    #[schemars(description = "The unique identifier of the edge")]
    pub edge_id: u64,
}

/// Request to create a new edge between nodes.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateEdgeRequest {
    /// The source node ID.
    #[schemars(description = "The source node ID")]
    pub source_id: u64,

    /// The target node ID.
    #[schemars(description = "The target node ID")]
    pub target_id: u64,

    /// The label/type of the edge (e.g., "KNOWS", "WORKS_AT").
    #[schemars(description = "The label/type of the edge (e.g., 'KNOWS', 'WORKS_AT')")]
    pub label: String,

    /// Properties to set on the edge as key-value pairs.
    #[schemars(description = "Properties to set on the edge as key-value pairs")]
    pub properties: Option<HashMap<String, serde_json::Value>>,
}

/// Request to update an existing edge's properties.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdateEdgeRequest {
    /// The unique identifier of the edge to update.
    #[schemars(description = "The unique identifier of the edge to update")]
    pub edge_id: u64,

    /// New properties to set (replaces all existing properties).
    #[schemars(description = "New properties to set (replaces all existing properties)")]
    pub properties: HashMap<String, serde_json::Value>,
}

/// Request to delete an edge.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeleteEdgeRequest {
    /// The unique identifier of the edge to delete.
    #[schemars(description = "The unique identifier of the edge to delete")]
    pub edge_id: u64,
}

/// Request to list edges with optional filtering.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListEdgesRequest {
    /// Filter by edge label (optional).
    #[schemars(description = "Filter by edge label (optional)")]
    pub label: Option<String>,

    /// Maximum number of edges to return (default: 100).
    #[schemars(description = "Maximum number of edges to return (default: 100)")]
    pub limit: Option<usize>,

    /// Number of edges to skip (for pagination).
    #[schemars(description = "Number of edges to skip (for pagination)")]
    pub offset: Option<usize>,
}

/// Request to count edges.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CountEdgesRequest {
    /// Filter by edge label (optional).
    #[schemars(description = "Filter by edge label (optional, if not provided counts all edges)")]
    pub label: Option<String>,
}

/// Request to get outgoing edges from a node.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetOutgoingEdgesRequest {
    /// The source node ID.
    #[schemars(description = "The source node ID")]
    pub node_id: u64,

    /// Filter by edge label (optional).
    #[schemars(description = "Filter by edge label (optional)")]
    pub label: Option<String>,
}

/// Request to get incoming edges to a node.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetIncomingEdgesRequest {
    /// The target node ID.
    #[schemars(description = "The target node ID")]
    pub node_id: u64,

    /// Filter by edge label (optional).
    #[schemars(description = "Filter by edge label (optional)")]
    pub label: Option<String>,
}

// ============================================================================
// Graph Traversal Operations
// ============================================================================

/// Request to perform graph traversal.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TraverseRequest {
    /// Starting node ID for traversal.
    #[schemars(description = "Starting node ID for traversal")]
    pub start_node_id: u64,

    /// Edge label to traverse (e.g., "KNOWS", "WORKS_AT").
    #[schemars(description = "Edge label to traverse (e.g., 'KNOWS', 'WORKS_AT')")]
    pub edge_label: String,

    /// Traversal direction: "outgoing", "incoming", or "both".
    #[schemars(
        description = "Traversal direction: 'outgoing', 'incoming', or 'both' (default: 'outgoing')"
    )]
    pub direction: Option<String>,

    /// Maximum traversal depth (default: 1).
    #[schemars(description = "Maximum traversal depth (default: 1)")]
    pub depth: Option<usize>,

    /// Maximum number of results to return.
    #[schemars(description = "Maximum number of results to return (default: 100)")]
    pub limit: Option<usize>,
}

// ============================================================================
// Vector Operations
// ============================================================================

/// Request to find similar nodes by vector embedding.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindSimilarRequest {
    /// The property name that contains the vector embedding.
    #[schemars(
        description = "The property name that contains the vector embedding (e.g., 'embedding')"
    )]
    pub property_name: String,

    /// The query embedding vector.
    #[schemars(description = "The query embedding vector (array of f32 values)")]
    pub embedding: Vec<f32>,

    /// Number of similar results to return.
    #[schemars(description = "Number of similar results to return (default: 10)")]
    pub k: Option<usize>,
}

/// Request to enable vector indexing on a property.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EnableVectorIndexRequest {
    /// The property name to index.
    #[schemars(description = "The property name to index (e.g., 'embedding')")]
    pub property_name: String,

    /// The dimension of the vectors.
    #[schemars(description = "The dimension of the vectors")]
    pub dimensions: usize,

    /// Distance metric: "cosine", "euclidean", or "dot".
    #[schemars(
        description = "Distance metric: 'cosine', 'euclidean', or 'dot' (default: 'cosine')"
    )]
    pub distance_metric: Option<String>,
}

/// Request to list all enabled vector indexes.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListVectorIndexesRequest {}

// ============================================================================
// Temporal Operations
// ============================================================================

/// Request to get a node at a specific point in time.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetNodeAtTimeRequest {
    /// The unique identifier of the node.
    #[schemars(description = "The unique identifier of the node")]
    pub node_id: u64,

    /// Valid time as ISO 8601 timestamp (when the fact was true in reality).
    #[schemars(
        description = "Valid time as ISO 8601 timestamp (when the fact was true in reality)"
    )]
    pub valid_time: String,

    /// Transaction time as ISO 8601 timestamp (when the fact was recorded).
    /// If not provided, uses current time.
    #[schemars(
        description = "Transaction time as ISO 8601 timestamp (when recorded). If not provided, uses current time."
    )]
    pub transaction_time: Option<String>,
}

/// Request to get an edge at a specific point in time.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetEdgeAtTimeRequest {
    /// The unique identifier of the edge.
    #[schemars(description = "The unique identifier of the edge")]
    pub edge_id: u64,

    /// Valid time as ISO 8601 timestamp (when the fact was true in reality).
    #[schemars(
        description = "Valid time as ISO 8601 timestamp (when the fact was true in reality)"
    )]
    pub valid_time: String,

    /// Transaction time as ISO 8601 timestamp (when the fact was recorded).
    /// If not provided, uses current time.
    #[schemars(
        description = "Transaction time as ISO 8601 timestamp (when recorded). If not provided, uses current time."
    )]
    pub transaction_time: Option<String>,
}

/// Request to get a node at a specific valid time (independent dimension query).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetNodeAtValidTimeRequest {
    /// The unique identifier of the node.
    #[schemars(description = "The unique identifier of the node")]
    pub node_id: u64,

    /// Valid time as ISO 8601 timestamp (when the fact was true in reality).
    #[schemars(
        description = "Valid time as ISO 8601 timestamp (when the fact was true in reality)"
    )]
    pub valid_time: String,
}

/// Request to get a node at a specific transaction time (independent dimension query).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetNodeAtTransactionTimeRequest {
    /// The unique identifier of the node.
    #[schemars(description = "The unique identifier of the node")]
    pub node_id: u64,

    /// Transaction time as ISO 8601 timestamp (when the fact was recorded).
    #[schemars(
        description = "Transaction time as ISO 8601 timestamp (when the fact was recorded)"
    )]
    pub transaction_time: String,
}

/// Request to get the complete version history of a node.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetNodeHistoryRequest {
    /// The unique identifier of the node.
    #[schemars(description = "The unique identifier of the node")]
    pub node_id: u64,
}

/// Request to compute the difference between two versions of a node.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DiffNodeVersionsRequest {
    /// The unique identifier of the node.
    #[schemars(description = "The unique identifier of the node")]
    pub node_id: u64,

    /// The ID of the older version (from).
    #[schemars(description = "The ID of the older version (from)")]
    pub from_version: u64,

    /// The ID of the newer version (to).
    #[schemars(description = "The ID of the newer version (to)")]
    pub to_version: u64,
}

/// Request to get an edge at a specific valid time (independent dimension query).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetEdgeAtValidTimeRequest {
    /// The unique identifier of the edge.
    #[schemars(description = "The unique identifier of the edge")]
    pub edge_id: u64,

    /// Valid time as ISO 8601 timestamp (when the fact was true in reality).
    #[schemars(
        description = "Valid time as ISO 8601 timestamp (when the fact was true in reality)"
    )]
    pub valid_time: String,
}

/// Request to get an edge at a specific transaction time (independent dimension query).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetEdgeAtTransactionTimeRequest {
    /// The unique identifier of the edge.
    #[schemars(description = "The unique identifier of the edge")]
    pub edge_id: u64,

    /// Transaction time as ISO 8601 timestamp (when the fact was recorded).
    #[schemars(
        description = "Transaction time as ISO 8601 timestamp (when the fact was recorded)"
    )]
    pub transaction_time: String,
}

/// Request to get the complete version history of an edge.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetEdgeHistoryRequest {
    /// The unique identifier of the edge.
    #[schemars(description = "The unique identifier of the edge")]
    pub edge_id: u64,
}

/// Request to compute the difference between two versions of an edge.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DiffEdgeVersionsRequest {
    /// The unique identifier of the edge.
    #[schemars(description = "The unique identifier of the edge")]
    pub edge_id: u64,

    /// The ID of the older version (from).
    #[schemars(description = "The ID of the older version (from)")]
    pub from_version: u64,

    /// The ID of the newer version (to).
    #[schemars(description = "The ID of the newer version (to)")]
    pub to_version: u64,
}

// ============================================================================
// Hybrid Query Operations
// ============================================================================

/// Request for hybrid query combining graph traversal, vector search, and temporal filtering.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct HybridQueryRequest {
    /// Starting node ID (optional, for graph-first queries).
    #[schemars(description = "Starting node ID (optional, for graph-first queries)")]
    pub start_node_id: Option<u64>,

    /// Edge label for traversal (optional).
    #[schemars(description = "Edge label for traversal (optional)")]
    pub traverse_edge: Option<String>,

    /// Traversal depth (optional, default: 1).
    #[schemars(description = "Traversal depth (optional, default: 1)")]
    pub traverse_depth: Option<usize>,

    /// Property name for vector similarity (optional).
    #[schemars(description = "Property name for vector similarity (optional)")]
    pub vector_property: Option<String>,

    /// Query embedding for vector similarity (optional).
    #[schemars(description = "Query embedding for vector similarity (optional)")]
    pub query_embedding: Option<Vec<f32>>,

    /// Number of similar results for vector search (optional).
    #[schemars(description = "Number of similar results for vector search (optional)")]
    pub top_k: Option<usize>,

    /// Valid time for temporal filtering (optional, ISO 8601).
    #[schemars(description = "Valid time for temporal filtering (optional, ISO 8601)")]
    pub valid_time: Option<String>,

    /// Transaction time for temporal filtering (optional, ISO 8601).
    #[schemars(description = "Transaction time for temporal filtering (optional, ISO 8601)")]
    pub transaction_time: Option<String>,

    /// Filter by node label (optional).
    #[schemars(description = "Filter by node label (optional)")]
    pub filter_label: Option<String>,

    /// Maximum number of results.
    #[schemars(description = "Maximum number of results (default: 100)")]
    pub limit: Option<usize>,
}

// ============================================================================
// Response Types (for serialization)
// ============================================================================

/// Serializable node representation for MCP responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResponse {
    pub id: u64,
    pub label: String,
    pub properties: HashMap<String, serde_json::Value>,
}

/// Serializable edge representation for MCP responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeResponse {
    pub id: u64,
    pub source_id: u64,
    pub target_id: u64,
    pub label: String,
    pub properties: HashMap<String, serde_json::Value>,
}

/// Similarity search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityResult {
    pub node: NodeResponse,
    pub score: f32,
}

/// Traversal result with path information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalResult {
    pub node: NodeResponse,
    pub path: Vec<u64>,
    pub depth: usize,
}

/// Hybrid query result combining all result types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridQueryResult {
    pub node: NodeResponse,
    pub similarity_score: Option<f32>,
    pub traversal_path: Option<Vec<u64>>,
    pub timestamp: Option<String>,
}

/// Information about a specific version in an entity's history.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfoResponse {
    pub version_number: u64,
    pub version_id: u64,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub transaction_from: String,
    pub transaction_to: Option<String>,
    pub properties: HashMap<String, serde_json::Value>,
    pub label: String,
}

/// Complete version history of an entity (node or edge).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityHistoryResponse {
    pub versions: Vec<VersionInfoResponse>,
}

/// Difference between two versions showing property changes.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionDiffResponse {
    pub from_version: u64,
    pub to_version: u64,
    pub added: HashMap<String, serde_json::Value>,
    pub removed: HashMap<String, serde_json::Value>,
    pub modified: Vec<PropertyChangeResponse>,
}

/// Details of a property modification.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyChangeResponse {
    pub key: String,
    pub old_value: serde_json::Value,
    pub new_value: serde_json::Value,
}
