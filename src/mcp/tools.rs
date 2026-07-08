//! Tool definitions for the AletheiaDB MCP server.
//!
//! This module defines all the MCP tools that expose AletheiaDB functionality
//! to LLM clients. Each tool is defined with JSON Schema-compatible request types.

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Provenance (Issue #3224)
// ============================================================================

/// Optional write-time attributive provenance bundle.
///
/// Attach this to a write to record *who/what* wrote a fact, *why*, and *how
/// confident* the writer was -- complementing the bi-temporal axes (valid
/// time / transaction time) that already record *when*. Every field is
/// independently optional; an entirely empty bundle (all fields omitted) is
/// treated as no provenance at all. `confidence`, if present, is validated
/// to be in `[0.0, 1.0]` and rejected with a clear error otherwise.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ProvenanceRequest {
    /// Source system/identifier that produced this write (e.g. "hr-system",
    /// "csv-import:2026-06", "claude-mcp").
    #[schemars(
        description = "Source system/identifier that produced this write (e.g. 'hr-system', 'csv-import:2026-06', 'claude-mcp')"
    )]
    pub source: Option<String>,

    /// Confidence in this fact, in `[0.0, 1.0]`. Out-of-range values are rejected.
    #[schemars(
        description = "Confidence in this fact, in [0.0, 1.0]. Out-of-range values are rejected with a clear error"
    )]
    pub confidence: Option<f64>,

    /// Free-text explanation of the write.
    #[schemars(description = "Free-text explanation of the write")]
    pub note: Option<String>,

    /// Correlation ID grouping all writes made in one logical operation.
    #[schemars(description = "Correlation ID grouping all writes made in one logical operation")]
    pub correlation_id: Option<String>,
}

// ============================================================================
// Node Operations
// ============================================================================

/// Request to get a node by its ID.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetNodeRequest {
    /// The unique identifier of the node (u64).
    #[schemars(description = "The unique identifier of the node")]
    pub node_id: u64,

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,
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

    /// Optional valid time: when this fact became true in the real world.
    #[schemars(
        description = "Optional valid time: when this fact became true in the real world, as an \
                       ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or integer \
                       microseconds since epoch. Omit to default to the transaction time (today's \
                       behavior). Backdating records history; up to 1 year in the future is \
                       allowed. Transaction time is always system-assigned and cannot be set."
    )]
    pub valid_time: Option<String>,

    /// Optional write-time provenance bundle (source, confidence, note, correlation_id).
    #[schemars(
        description = "Optional write-time provenance bundle recording source, confidence \
                       ([0.0, 1.0]), a free-text note, and/or a correlation_id grouping co-committed \
                       writes. Retrievable later via get_node/get_node_history. Omit for no provenance."
    )]
    pub provenance: Option<ProvenanceRequest>,
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

    /// Optional valid time: when this update became true in the real world.
    #[schemars(
        description = "Optional valid time: when this update became true in the real world, as an \
                       ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or integer \
                       microseconds since epoch. Omit to default to the transaction time (today's \
                       behavior). Must not precede the node's own creation time. Transaction time \
                       is always system-assigned and cannot be set."
    )]
    pub valid_time: Option<String>,

    /// Optional write-time provenance bundle for this version. Independent of
    /// whatever provenance the prior version carried.
    #[schemars(
        description = "Optional write-time provenance bundle recording source, confidence \
                       ([0.0, 1.0]), a free-text note, and/or a correlation_id for this version. \
                       Not inherited from the version being updated. Omit for no provenance."
    )]
    pub provenance: Option<ProvenanceRequest>,
}

/// Request to delete a node.
///
/// Safe-by-default (Issue #3209): if the node has connected edges and `detach`
/// is not `true`, the deletion is refused and the response reports the number of
/// connected edges (mirrors Cypher's `DETACH DELETE` contract). Set `detach:
/// true` to delete the node together with all of its connected edges.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeleteNodeRequest {
    /// The unique identifier of the node to delete.
    #[schemars(description = "The unique identifier of the node to delete")]
    pub node_id: u64,

    /// When `true`, also delete every edge connected to the node (cascade /
    /// detach delete). When omitted or `false`, deleting a node that has
    /// connected edges is refused and the response reports `connected_edges`.
    #[schemars(
        description = "When true, also delete all edges connected to the node (detach delete). \
                       When false/omitted, deletion is refused if the node has connected edges, \
                       and the response reports the connected edge count so the caller can decide."
    )]
    pub detach: Option<bool>,

    /// Optional valid time: when this fact stopped being true in the real world.
    #[schemars(
        description = "Optional valid time: when this fact stopped being true in the real world, \
                       as an ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or integer \
                       microseconds since epoch. Omit to default to the transaction time (today's \
                       behavior). Must not precede the node's own creation time. Not supported \
                       together with detach:true (cascade delete does not support backdating). \
                       Transaction time is always system-assigned and cannot be set."
    )]
    pub valid_time: Option<String>,
}

/// Request to retract a node: close its valid-time interval without
/// deleting its history (Issue #3230).
///
/// Safe-by-default (mirrors the #3209 `delete_node` DETACH contract): if the
/// node has connected edges and `detach` is not `true`, the retraction is
/// refused and the response reports `connected_edges`. Set `detach: true` to
/// co-retract every connected edge at the same valid time.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RetractNodeRequest {
    /// The unique identifier of the node to retract.
    #[schemars(description = "The unique identifier of the node to retract")]
    pub node_id: u64,

    /// Optional valid time: when this fact stopped being true in the real world.
    #[schemars(
        description = "Optional valid time: when this fact stopped being true in the real world, \
                       as an ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or integer \
                       microseconds since epoch. Omit to default to now. Must not precede the \
                       node's valid_from (equality is allowed). History before this instant \
                       remains fully queryable. Transaction time is always system-assigned and \
                       cannot be set."
    )]
    pub valid_time: Option<String>,

    /// When `true`, also retract every edge connected to the node at the
    /// same valid time. When omitted or `false`, retracting a node that has
    /// connected edges is refused and the response reports `connected_edges`.
    #[schemars(
        description = "When true, also retract all edges connected to the node at the same valid \
                       time (detach retraction). When false/omitted, retraction is refused if the \
                       node has connected edges, and the response reports the connected edge count \
                       so the caller can decide."
    )]
    pub detach: Option<bool>,
}

/// Request to retract an edge: close its valid-time interval without
/// deleting its history (Issue #3230).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RetractEdgeRequest {
    /// The unique identifier of the edge to retract.
    #[schemars(description = "The unique identifier of the edge to retract")]
    pub edge_id: u64,

    /// Optional valid time: when this relationship stopped being true in the real world.
    #[schemars(
        description = "Optional valid time: when this relationship stopped being true in the real \
                       world, as an ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or \
                       integer microseconds since epoch. Omit to default to now. Must not precede \
                       the edge's valid_from (equality is allowed). History before this instant \
                       remains fully queryable. Transaction time is always system-assigned and \
                       cannot be set."
    )]
    pub valid_time: Option<String>,
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

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,
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

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,
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

    /// Optional valid time: when this relationship became true in the real world.
    #[schemars(
        description = "Optional valid time: when this relationship became true in the real world, \
                       as an ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or integer \
                       microseconds since epoch. Omit to default to the transaction time (today's \
                       behavior). Backdating records history; up to 1 year in the future is \
                       allowed. Transaction time is always system-assigned and cannot be set."
    )]
    pub valid_time: Option<String>,

    /// Optional write-time provenance bundle (source, confidence, note, correlation_id).
    #[schemars(
        description = "Optional write-time provenance bundle recording source, confidence \
                       ([0.0, 1.0]), a free-text note, and/or a correlation_id grouping co-committed \
                       writes. Retrievable later via get_edge_history. Omit for no provenance."
    )]
    pub provenance: Option<ProvenanceRequest>,
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

    /// Optional valid time: when this update became true in the real world.
    #[schemars(
        description = "Optional valid time: when this update became true in the real world, as an \
                       ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or integer \
                       microseconds since epoch. Omit to default to the transaction time (today's \
                       behavior). Must not precede the edge's own creation time. Transaction time \
                       is always system-assigned and cannot be set."
    )]
    pub valid_time: Option<String>,

    /// Optional write-time provenance bundle for this version. Independent of
    /// whatever provenance the prior version carried.
    #[schemars(
        description = "Optional write-time provenance bundle recording source, confidence \
                       ([0.0, 1.0]), a free-text note, and/or a correlation_id for this version. \
                       Not inherited from the version being updated. Omit for no provenance."
    )]
    pub provenance: Option<ProvenanceRequest>,
}

/// Request to delete an edge.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeleteEdgeRequest {
    /// The unique identifier of the edge to delete.
    #[schemars(description = "The unique identifier of the edge to delete")]
    pub edge_id: u64,

    /// Optional valid time: when this fact stopped being true in the real world.
    #[schemars(
        description = "Optional valid time: when this fact stopped being true in the real world, \
                       as an ISO 8601 / RFC 3339 timestamp (e.g., '2024-01-15T10:00:00Z') or integer \
                       microseconds since epoch. Omit to default to the transaction time (today's \
                       behavior). Must not precede the edge's own creation time. Transaction time \
                       is always system-assigned and cannot be set."
    )]
    pub valid_time: Option<String>,
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

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,
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

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,
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

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,
}

// ============================================================================
// Graph Traversal Operations
// ============================================================================

/// Request to perform graph traversal.
///
/// With no `as_of_*` fields, traversal walks current-state adjacency (identical
/// to prior behavior). With either field supplied, traversal instead follows
/// only edges and nodes valid at that bi-temporal instant -- edges created
/// after the coordinate, or whose valid interval does not contain it, are
/// excluded, and node properties reflect their state at that instant. Each
/// dimension defaults independently to the current time when the *other* one
/// is supplied but it isn't, mirroring `get_schema`'s `as_of_*` convention.
///
/// Marked `#[non_exhaustive]` because Issue #3226 added the `offset` field
/// after this struct's initial release; struct-literal construction from
/// outside this crate would otherwise be a semver break on every future
/// field addition.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[non_exhaustive]
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

    /// Number of results to skip (for pagination).
    #[schemars(
        description = "Number of results to skip (for pagination). Pass the `next_offset` from a \
                       prior response to fetch the next page; the response's `has_more` tells you \
                       whether another page exists."
    )]
    pub offset: Option<usize>,

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,

    /// Optional valid time as ISO 8601 timestamp or microseconds since epoch.
    /// If supplied (alone or with `as_of_transaction_time`), traversal follows
    /// only edges/nodes valid as of this bi-temporal instant instead of the
    /// current state. If omitted while `as_of_transaction_time` is set,
    /// defaults to the current time.
    #[schemars(
        description = "Optional valid time (ISO 8601 or microseconds since epoch). Supplying this or as_of_transaction_time switches to a bi-temporal, point-in-time traversal. If omitted while as_of_transaction_time is set, defaults to the current time."
    )]
    pub as_of_valid_time: Option<String>,

    /// Optional transaction time as ISO 8601 timestamp or microseconds since
    /// epoch. If supplied (alone or with `as_of_valid_time`), traversal
    /// follows only edges/nodes recorded as of this bi-temporal instant
    /// instead of the current state. If omitted while `as_of_valid_time` is
    /// set, defaults to the current time.
    #[schemars(
        description = "Optional transaction time (ISO 8601 or microseconds since epoch). Supplying this or as_of_valid_time switches to a bi-temporal, point-in-time traversal. If omitted while as_of_valid_time is set, defaults to the current time."
    )]
    pub as_of_transaction_time: Option<String>,
}

// ============================================================================
// Vector Operations
// ============================================================================

/// Request to find similar nodes by vector embedding.
///
/// Marked `#[non_exhaustive]` because Issue #3226 added the `offset` field
/// after this struct's initial release; struct-literal construction from
/// outside this crate would otherwise be a semver break on every future
/// field addition.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[non_exhaustive]
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

    /// Number of results to skip (for pagination).
    #[schemars(
        description = "Number of results to skip (for pagination). Pass the `next_offset` from a \
                       prior response to fetch the next page; the response's `has_more` tells you \
                       whether another page exists."
    )]
    pub offset: Option<usize>,

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false). Does not affect the similarity
    /// `score`, which is always returned in full.
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false). The similarity score \
                       is always returned in full regardless of this flag."
    )]
    pub include_vectors: Option<bool>,
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
// Constraint Operations
// ============================================================================

/// Request to enable a uniqueness constraint on a label+property pair.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EnableUniqueConstraintRequest {
    /// The node label the constraint applies to (e.g., "Person").
    #[schemars(description = "The node label the constraint applies to (e.g., 'Person')")]
    pub label: String,

    /// The property name that must be unique within the label (e.g., "email").
    #[schemars(
        description = "The property name that must be unique within the label (e.g., 'email')"
    )]
    pub property: String,
}

/// Request to list all active uniqueness constraints.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListUniqueConstraintsRequest {}

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

/// Request to list graph-wide changes (node & edge versions) committed within a
/// transaction-time window. Read-only; the discovery counterpart to `get_node_history`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListChangesRequest {
    /// Start of the transaction-time window (inclusive).
    #[schemars(
        description = "Start of the transaction-time window (inclusive), ISO 8601 timestamp or microseconds since epoch"
    )]
    pub tx_from: String,

    /// End of the transaction-time window (exclusive).
    #[schemars(
        description = "End of the transaction-time window (exclusive), ISO 8601 timestamp or microseconds since epoch"
    )]
    pub tx_to: String,

    /// Optional valid-time window start (inclusive). Must be paired with `valid_to`.
    #[schemars(
        description = "Optional valid-time window start (inclusive). Must be supplied together with valid_to."
    )]
    pub valid_from: Option<String>,

    /// Optional valid-time window end (exclusive). Must be paired with `valid_from`.
    #[schemars(
        description = "Optional valid-time window end (exclusive). Must be supplied together with valid_from."
    )]
    pub valid_to: Option<String>,

    /// Optional node-label / edge-type filter (exact match).
    #[schemars(description = "Optional node label / edge type filter (exact match)")]
    pub label: Option<String>,

    /// Maximum number of changes to return (default 100, max 10000).
    #[schemars(description = "Maximum number of changes to return (default 100, max 10000)")]
    pub limit: Option<usize>,

    /// Opaque continuation token from a previous response's `next_cursor`.
    #[schemars(
        description = "Opaque continuation token from a previous response's next_cursor; omit for the first page"
    )]
    pub cursor: Option<String>,
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

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false). Does not affect the
    /// `similarity_score`, which is always returned in full.
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false). The similarity_score \
                       is always returned in full regardless of this flag."
    )]
    pub include_vectors: Option<bool>,
}

// ============================================================================
// Declarative Query Operations (read-only Cypher / AQL)
// ============================================================================

/// Request to execute a read-only declarative query (Cypher or AQL).
///
/// This is the single-call counterpart to chaining several structured tools:
/// the LLM emits one Cypher/AQL statement and receives structured rows back.
/// Mutating statements (CREATE/SET/DELETE/MERGE/REMOVE/…) are rejected before
/// execution — the tool never writes.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct QueryRequest {
    /// Query language: "cypher" or "aql".
    #[schemars(description = "Query language to use: \"cypher\" or \"aql\"")]
    pub language: String,

    /// The read-only query string to execute.
    #[schemars(
        description = "The read-only query string (e.g. MATCH (n:Person {name:$name}) RETURN n)"
    )]
    pub query: String,

    /// Optional `$param` bindings (Cypher only). Numbers, strings, booleans,
    /// null, and numeric arrays (treated as embeddings) are supported.
    #[schemars(
        description = "Optional $param bindings for Cypher. Numeric arrays are treated as embeddings."
    )]
    pub params: Option<HashMap<String, serde_json::Value>>,

    /// Maximum number of rows to return (default: 100, capped at 10000).
    #[schemars(description = "Maximum number of rows to return (default: 100, capped at 10000)")]
    pub limit: Option<usize>,
}

// ============================================================================
// Schema Discovery (Issue #3214)
// ============================================================================

/// Request to discover the graph's schema: distinct node labels and edge
/// types, their counts, and the property keys observed on each.
///
/// With no `as_of_*` fields, returns the current-state schema.
///
/// With either field supplied, returns the schema as it existed at that
/// bi-temporal instant. Each dimension defaults independently to the
/// current time when the *other* one is supplied but it isn't -- e.g.
/// `as_of_transaction_time` alone answers "what does the schema look like,
/// using only facts recorded by transaction-time T, for whatever is valid
/// right now" (valid_time defaults to now). This mirrors the Rust API's
/// `get_node_at_valid_time`/`get_node_at_transaction_time` convenience
/// methods, which default the unspecified dimension the same way. If you
/// want a specific instant on *both* axes, supply both fields explicitly.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetSchemaRequest {
    /// Optional valid time as ISO 8601 timestamp or microseconds since epoch.
    /// If supplied (alone or with `as_of_transaction_time`), the schema is
    /// computed as of this bi-temporal instant instead of the current state.
    /// If omitted while `as_of_transaction_time` is set, defaults to the
    /// current time (i.e. "whatever is valid right now").
    #[schemars(
        description = "Optional valid time (ISO 8601 or microseconds since epoch). Supplying this or as_of_transaction_time switches to a bi-temporal schema snapshot. If omitted while as_of_transaction_time is set, defaults to the current time."
    )]
    pub as_of_valid_time: Option<String>,

    /// Optional transaction time as ISO 8601 timestamp or microseconds since epoch.
    /// If supplied (alone or with `as_of_valid_time`), the schema is computed
    /// as of this bi-temporal instant instead of the current state. If
    /// omitted while `as_of_valid_time` is set, defaults to the current time
    /// (i.e. "everything recorded so far").
    #[schemars(
        description = "Optional transaction time (ISO 8601 or microseconds since epoch). Supplying this or as_of_valid_time switches to a bi-temporal schema snapshot. If omitted while as_of_valid_time is set, defaults to the current time."
    )]
    pub as_of_transaction_time: Option<String>,
}

// ============================================================================
// Temporal Extent (Issue #3238)
// ============================================================================

/// Request for the dataset's queryable bi-temporal extent: the earliest and
/// latest valid-time and transaction-time coordinates across recorded
/// history (including expired/superseded versions), covering everything
/// recorded during the current process lifetime plus hot-tier history
/// restored at startup (versions cold-migrated before the last restart are
/// excluded).
///
/// No required arguments. Pass `by_label: true` to additionally receive the
/// same bounds per node label and per edge type.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TemporalExtentRequest {
    /// When `true`, additionally break the bounds down per node label and
    /// per edge/relationship type. Defaults to `false` (overall bounds only).
    ///
    /// Per-label bounds are computed from hot-tier history only: on
    /// databases with cold-storage migration they may be narrower than the
    /// overall bounds (or a label may be absent entirely).
    #[schemars(
        description = "When true, additionally return the same {valid_time, transaction_time} bounds per node label (node_labels) and per edge type (edge_types), so calibration can be scoped to the labels being queried. Defaults to false. Per-label bounds are computed from hot-tier history only and may be narrower than the overall bounds (or a label absent) after cold-storage migration."
    )]
    pub by_label: Option<bool>,
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
    /// The current version's write-time provenance bundle, if any. Omitted
    /// (never a fabricated `null`) when the version has none (Issue #3224).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provenance: Option<crate::core::provenance::Provenance>,
}

/// Serializable edge representation for MCP responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeResponse {
    pub id: u64,
    pub source_id: u64,
    pub target_id: u64,
    pub label: String,
    pub properties: HashMap<String, serde_json::Value>,
    /// The current version's write-time provenance bundle, if any. Omitted
    /// (never a fabricated `null`) when the version has none (Issue #3224).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provenance: Option<crate::core::provenance::Provenance>,
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
