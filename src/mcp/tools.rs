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
///
/// Note: the stored provenance's `principal` field (Issue #3350) is
/// **deliberately absent** here -- it is stamped server-side from the
/// verified session credential and cannot be supplied (or forged) by the
/// caller.
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
// Derivation lineage (Issue #3371)
// ============================================================================

/// A version-pinned reference to a single existing fact, used to declare
/// derivation lineage on a write (`derived_from`) and as the root of a
/// lineage query.
///
/// Pins **both** the entity and the exact version that was read, so lineage
/// refers to precisely the fact a derivation consumed — immune to later
/// updates of that entity.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct LineageRefRequest {
    /// Whether the reference points at a node or an edge.
    #[schemars(description = "The kind of entity referenced: 'node' or 'edge'.")]
    pub entity_kind: String,

    /// The referenced entity's id (node id or edge id).
    #[schemars(
        description = "The referenced entity's id (a node id when entity_kind='node', an edge id when 'edge')."
    )]
    pub id: u64,

    /// The specific version id of that entity that was read/derived from.
    #[schemars(
        description = "The specific version id of the entity that was read. Lineage pins the \
                       exact version, not just the entity, so it stays precise across later updates. \
                       Obtain it from get_node_history / get_edge_history (each entry's version_id)."
    )]
    pub version: u64,
}

/// Request for a lineage closure query (`lineage_upstream` / `lineage_downstream`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct LineageQueryRequest {
    /// Whether the root fact is a node or an edge.
    #[schemars(description = "The root fact's entity kind: 'node' or 'edge'.")]
    pub entity_kind: String,

    /// The root entity's id.
    #[schemars(description = "The root entity's id (node id or edge id per entity_kind).")]
    pub id: u64,

    /// The root entity's version id (lineage is version-pinned).
    #[schemars(
        description = "The root fact's version id. Lineage is version-pinned; use the version \
                       whose upstream/downstream closure you want."
    )]
    pub version: u64,

    /// Maximum transitive depth to expand (hops from the root).
    #[schemars(
        description = "Maximum transitive hop depth from the root (1 = direct parents/children). \
                       Defaults to the store's default depth cap. Hitting the cap sets has_more."
    )]
    pub max_depth: Option<usize>,

    /// Maximum number of entries to return in this page.
    #[schemars(
        description = "Maximum number of closure entries to return (default 100). Hitting the \
                       limit sets has_more and next_offset."
    )]
    pub limit: Option<usize>,

    /// Number of entries to skip (breadth-first order) for pagination.
    #[schemars(
        description = "Number of closure entries to skip (breadth-first order) for pagination (default 0)."
    )]
    pub offset: Option<usize>,

    /// Optional AS OF transaction-time bound: only follow lineage records
    /// recorded at or before this transaction time.
    #[schemars(
        description = "Optional AS OF transaction-time coordinate (ISO 8601 / RFC 3339 or integer \
                       microseconds since epoch): only lineage records recorded at or before this \
                       transaction time are followed, so the closure reflects lineage as it was \
                       recorded by that time. Omit for all records."
    )]
    pub as_of_transaction_time: Option<String>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace scope. Omitted = the `default` namespace only \
                       (isolated-by-default); for a pre-namespace database (all data is `default`) \
                       that still returns all of it. Pass a single namespace name (string) to scope \
                       to it, an array of names for a union (the read sees any of them), or the \
                       string \"all\" for every namespace (no filter). An out-of-scope entity is \
                       reported NOT_FOUND (indistinguishable from missing). An unknown namespace is \
                       NOT_FOUND (details.namespace); an empty array is INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
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

    /// Optional derivation lineage: the existing fact versions this new fact
    /// was computed from (Issue #3371).
    #[schemars(
        description = "Optional derivation lineage (Issue #3371): an array of version-pinned \
                       references to existing facts this node was computed/derived from, each \
                       {entity_kind:'node'|'edge', id, version}. A nonexistent reference fails the \
                       write with a structured error before any commit — no silent dangling lineage. \
                       Omit for no lineage. Query it later via lineage_upstream/lineage_downstream."
    )]
    pub derived_from: Option<Vec<LineageRefRequest>>,

    /// Optional namespace to create this node in (Issue #3349).
    #[schemars(
        description = "Optional namespace (agent/session scope) to create this node in, e.g. \
                       'agent:planner'. Omit to use the default namespace ('default'), which is \
                       byte-identical to pre-namespace behavior. An unknown namespace is \
                       auto-registered. The namespace is fixed at creation and immutable \
                       thereafter; it is surfaced back as the first-class `namespace` field on \
                       reads and never as a user property. Charset [A-Za-z0-9._:/-], max 128 bytes."
    )]
    pub namespace: Option<String>,
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

    /// Optional derivation lineage for the *new* version (Issue #3371).
    #[schemars(
        description = "Optional derivation lineage (Issue #3371): the existing fact versions the \
                       NEW version was derived from, each {entity_kind:'node'|'edge', id, version}. \
                       Recorded against the new version; a nonexistent reference fails the write \
                       before any commit. Omit for no lineage."
    )]
    pub derived_from: Option<Vec<LineageRefRequest>>,

    /// Optional namespace (Issue #3349). A namespace is immutable after
    /// creation, so supplying one here is rejected.
    #[schemars(
        description = "A namespace is fixed at creation and immutable, so supplying one on an \
                       update is rejected with INVALID_ARGUMENT rather than silently ignored. \
                       Omit it; the node keeps its original namespace, echoed back as the \
                       first-class `namespace` field."
    )]
    pub namespace: Option<String>,
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

    /// Optional namespace (Issue #3349). A namespace is immutable, so supplying
    /// one here is rejected.
    #[schemars(
        description = "A namespace is fixed at creation and immutable, so supplying one on a \
                       delete is rejected with INVALID_ARGUMENT rather than silently ignored. \
                       Omit it."
    )]
    pub namespace: Option<String>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Omit for the current, unscoped \
                       behavior. When set, only nodes whose namespace is in scope are listed. An \
                       unknown namespace is NOT_FOUND (details.namespace); an empty array is \
                       INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Omit for the current, unscoped \
                       behavior. Out of scope, the edge is reported NOT_FOUND. An unknown namespace \
                       is NOT_FOUND (details.namespace); an empty array is INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
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

    /// Optional derivation lineage: the existing fact versions this new edge
    /// was computed from (Issue #3371).
    #[schemars(
        description = "Optional derivation lineage (Issue #3371): an array of version-pinned \
                       references to existing facts this edge was derived from, each \
                       {entity_kind:'node'|'edge', id, version}. A nonexistent reference fails the \
                       write with a structured error before any commit. Omit for no lineage."
    )]
    pub derived_from: Option<Vec<LineageRefRequest>>,

    /// Optional namespace to create this edge in (Issue #3349).
    #[schemars(
        description = "Optional namespace (agent/session scope) to create this edge in, e.g. \
                       'agent:planner'. Omit to use the default namespace ('default'). An unknown \
                       namespace is auto-registered. A cross-namespace edge (endpoints in other \
                       namespaces) is legal; the edge's own namespace governs whether a scoped \
                       traversal crosses it. Immutable after creation; echoed back as the \
                       first-class `namespace` field. Charset [A-Za-z0-9._:/-], max 128 bytes."
    )]
    pub namespace: Option<String>,
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

    /// Optional derivation lineage for the *new* edge version (Issue #3371).
    #[schemars(
        description = "Optional derivation lineage (Issue #3371): the existing fact versions the \
                       NEW edge version was derived from, each {entity_kind:'node'|'edge', id, \
                       version}. Recorded against the new version; a nonexistent reference fails the \
                       write before any commit. Omit for no lineage."
    )]
    pub derived_from: Option<Vec<LineageRefRequest>>,

    /// Optional namespace (Issue #3349). A namespace is immutable, so supplying
    /// one here is rejected.
    #[schemars(
        description = "A namespace is fixed at creation and immutable, so supplying one on an \
                       update is rejected with INVALID_ARGUMENT rather than silently ignored. \
                       Omit it; the edge keeps its original namespace, echoed back as the \
                       first-class `namespace` field."
    )]
    pub namespace: Option<String>,
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

    /// Optional namespace (Issue #3349). A namespace is immutable, so supplying
    /// one here is rejected.
    #[schemars(
        description = "A namespace is fixed at creation and immutable, so supplying one on a \
                       delete is rejected with INVALID_ARGUMENT rather than silently ignored. \
                       Omit it."
    )]
    pub namespace: Option<String>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). NOTE: list_edges does not enumerate \
                       edges by namespace in v1; supplying a scope other than \"all\" returns a \
                       structured INVALID_ARGUMENT rather than a silently-unscoped result. Use the \
                       scoped adjacency/traversal reads instead."
    )]
    pub namespace: Option<serde_json::Value>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Omit for the current, unscoped \
                       traversal. With an explicit NON-DEFAULT scope, an edge is crossed only if \
                       BOTH the edge's own namespace AND the target node's namespace are in scope, \
                       so a non-default scope is boundary-enforced and can never leak across the \
                       boundary. CAVEAT: an omitted scope or an explicit 'default' scope only \
                       filters non-default nodes out of the RESULT — the underlying traversal still \
                       routes THROUGH non-default (hidden) nodes to reach default ones, so \
                       reachability can be inferred through data that is never returned. Use an \
                       explicit non-default scope when you need boundary-enforced traversal. An \
                       unknown namespace is NOT_FOUND (details.namespace); an empty array is \
                       INVALID_ARGUMENT. In v1 a scoped traverse does not compose with the #3360 \
                       cursor / #3353 token-budget shaping (offset paging still applies)."
    )]
    pub namespace: Option<serde_json::Value>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Omit for the current, unscoped \
                       search. When set, the k-NN search is filter-complete: it over-fetches until \
                       it has k genuinely in-scope results (never k-then-drop), with scores and \
                       ordering unchanged. An unknown namespace is NOT_FOUND (details.namespace); \
                       an empty array is INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
}

// ============================================================================
// Embedding Generation & Text Semantic Search (Issue #2906)
// ============================================================================

/// Request to embed a single text string into a dense vector (`embed_query`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EmbedQueryRequest {
    /// The text to embed into a single dense vector.
    #[schemars(description = "The text to embed into a single dense embedding vector.")]
    pub text: String,

    /// Optional model identifier (reserved; the server uses its configured model).
    #[schemars(
        description = "Optional model identifier. Reserved for forward compatibility: v1 always \
                       uses the model the server was configured with, so this field is currently \
                       advisory. Omit it."
    )]
    pub model: Option<String>,
}

/// Request to embed multiple texts (chunk-expanded) into dense vectors
/// (`embed_text`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EmbedTextRequest {
    /// The texts to embed. Each entry is chunk-expanded into one or more
    /// per-chunk embeddings.
    #[schemars(
        description = "The texts to embed. Each entry is split into contiguous character-window \
                       chunks and every chunk is embedded independently, so a long document yields \
                       MULTIPLE per-chunk embeddings (never truncated to one). Each embedding is \
                       aligned to its source chunk text and metadata via the model's own chunk \
                       output (never positionally zipped); the metadata carries the originating \
                       source_index and chunk_index."
    )]
    pub texts: Vec<String>,

    /// Optional model identifier (reserved; the server uses its configured model).
    #[schemars(
        description = "Optional model identifier. Reserved for forward compatibility: v1 uses the \
                       server-configured model (advisory). Omit it."
    )]
    pub model: Option<String>,

    /// Optional cap on the number of returned chunk embeddings.
    #[schemars(
        description = "Optional hard cap on the number of returned per-chunk embeddings across all \
                       inputs. Clamped to the server maximum. When the cap trims the chunk \
                       expansion the response sets `truncated: true`. A value of 0 is rejected as \
                       INVALID_ARGUMENT."
    )]
    pub max_chunks: Option<usize>,
}

/// Request to embed a natural-language query and run vector similarity search
/// (`semantic_search`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SemanticSearchRequest {
    /// The property name that holds the indexed vector embedding.
    #[schemars(
        description = "The property name that holds the indexed vector embedding to search \
                       against (e.g., 'embedding'). A vector index must already be enabled for it."
    )]
    pub property_name: String,

    /// The natural-language query text to embed and search with.
    #[schemars(description = "The natural-language query text to embed and search with.")]
    pub query_text: String,

    /// Number of similar results to return (default 10).
    #[schemars(description = "Number of similar results to return (default: 10).")]
    pub k: Option<usize>,

    /// Number of results to skip (offset pagination).
    #[schemars(
        description = "Number of results to skip (offset pagination). Pass the `next_offset` from \
                       a prior response to fetch the next page."
    )]
    pub offset: Option<usize>,

    /// Return full embedding arrays instead of the elided descriptor.
    #[schemars(
        description = "When true, return full embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false). Never affects the \
                       similarity score."
    )]
    pub include_vectors: Option<bool>,

    /// Optional model identifier (reserved; the server uses its configured model).
    #[schemars(
        description = "Optional model identifier. Reserved for forward compatibility: v1 uses the \
                       server-configured model (advisory). Omit it."
    )]
    pub model: Option<String>,
}

/// Request to create a node whose embedding is generated from text
/// (`create_node_with_embedding`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateNodeWithEmbeddingRequest {
    /// The label/type of the node (e.g., "Document").
    #[schemars(description = "The label/type of the node (e.g., 'Document').")]
    pub label: String,

    /// The text to embed and store as the node's embedding property.
    #[schemars(description = "The text to embed and store as the node's embedding property.")]
    pub text: String,

    /// The property name under which to store the generated embedding vector.
    #[schemars(
        description = "The property name under which to store the generated embedding vector \
                       (e.g., 'embedding'). Compatible with enable_vector_index / find_similar / \
                       semantic_search."
    )]
    pub embedding_property: String,

    /// Optional additional properties to set on the node.
    #[schemars(
        description = "Optional additional non-embedding properties to set on the node as \
                       key-value pairs."
    )]
    pub properties: Option<HashMap<String, serde_json::Value>>,

    /// Optional valid time: when this fact became true in the real world.
    #[schemars(
        description = "Optional valid time (ISO 8601 / RFC 3339 or microseconds since epoch). Omit \
                       to default to the transaction time. Transaction time is always \
                       system-assigned."
    )]
    pub valid_time: Option<String>,

    /// Optional write-time provenance bundle.
    #[schemars(
        description = "Optional write-time provenance bundle (source, confidence, note, \
                       correlation_id)."
    )]
    pub provenance: Option<ProvenanceRequest>,
}

/// Request to (re)generate a node's embedding property from text
/// (`update_node_embedding`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdateNodeEmbeddingRequest {
    /// The unique identifier of the node to update.
    #[schemars(description = "The unique identifier of the node to update.")]
    pub node_id: u64,

    /// The text to embed and store as the node's embedding property.
    #[schemars(description = "The text to embed and store as the node's embedding property.")]
    pub text: String,

    /// The property name under which to store the regenerated embedding vector.
    #[schemars(
        description = "The property name under which to store the regenerated embedding vector. \
                       All OTHER existing properties of the node are preserved (update_node \
                       otherwise replaces every property)."
    )]
    pub embedding_property: String,

    /// Optional valid time: when this update became true in the real world.
    #[schemars(
        description = "Optional valid time (ISO 8601 / RFC 3339 or microseconds since epoch). Omit \
                       to default to the transaction time. Transaction time is always \
                       system-assigned."
    )]
    pub valid_time: Option<String>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Omit for the current, unscoped \
                       behavior. The point-in-time reconstruction runs first, then the namespace \
                       filter (immutable membership). Out of scope ⇒ NOT_FOUND. An unknown \
                       namespace is NOT_FOUND (details.namespace); an empty array is \
                       INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Omit for the current, unscoped \
                       behavior. The point-in-time reconstruction runs first, then the namespace \
                       filter (immutable membership). Out of scope ⇒ NOT_FOUND. An unknown \
                       namespace is NOT_FOUND (details.namespace); an empty array is \
                       INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
}

/// Request to find nodes by label (and optional exact property match) as of
/// a bi-temporal point (Issue #3236). The entry-point resolver for
/// "the Person named Alice, as of 2024-01-01" when no `NodeId` is known.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindNodesAtTimeRequest {
    /// The node label to match (required).
    #[schemars(description = "The node label to match (required)")]
    pub label: String,

    /// Filter by property key (requires property_value).
    #[schemars(
        description = "Filter by property key. Must be used together with 'property_value'."
    )]
    pub property_key: Option<String>,

    /// Filter by exact property value (requires property_key).
    /// Supports: strings, integers, floats, booleans, and null.
    #[schemars(
        description = "Filter by exact property value (JSON). Must be used together with 'property_key'."
    )]
    pub property_value: Option<serde_json::Value>,

    /// Valid time as ISO 8601 timestamp (when the fact was true in reality).
    #[schemars(
        description = "Valid time (required) as ISO 8601 / RFC 3339 timestamp (e.g., \
                       '2024-01-15T10:00:00Z') or microseconds since epoch: when the fact was \
                       true in reality"
    )]
    pub valid_time: String,

    /// Transaction time as ISO 8601 timestamp (when the fact was recorded).
    /// If not provided, uses current time.
    #[schemars(
        description = "Transaction time as ISO 8601 / RFC 3339 timestamp or microseconds since \
                       epoch (when recorded). If not provided, uses current time. Note: recalling \
                       a since-deleted node requires anchoring BOTH dimensions before the \
                       deletion, not just valid_time."
    )]
    pub transaction_time: Option<String>,

    /// Maximum number of nodes to return (default: 100).
    #[schemars(description = "Maximum number of nodes to return (default: 100)")]
    pub limit: Option<usize>,

    /// Number of matching nodes to skip (for pagination).
    #[schemars(description = "Number of matching nodes to skip (for pagination)")]
    pub offset: Option<usize>,

    /// When true, return full vector/embedding properties instead of the
    /// elided descriptor (default: false).
    #[schemars(
        description = "When true, return full vector/embedding float arrays instead of the elided \
                       {type, dim, elided:true} descriptor (default: false)"
    )]
    pub include_vectors: Option<bool>,

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Omit for the current, unscoped \
                       behavior. Each candidate is reconstructed at (valid_time, transaction_time) \
                       first, then filtered by its immutable namespace. An unknown namespace is \
                       NOT_FOUND (details.namespace); an empty array is INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
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

/// Request to long-poll for committed changes (the push changefeed's blocking
/// surface, Issue #3375). Read-only; the streaming counterpart to `list_changes`.
///
/// A single call subscribes to the push feed, optionally catches up from a prior
/// `from_token` via `list_changes`, and otherwise blocks up to `timeout_ms` for
/// the next matching commit. It is **stateless** across calls: resume by passing
/// the previous response's `resume_token` back as `from_token`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AwaitChangesRequest {
    /// Restrict to node changes whose label is one of these (exact match).
    #[schemars(
        description = "Optional list of node labels to match (exact). If set, only matching node \
                       changes are delivered; combine with edge_types to receive both kinds."
    )]
    pub node_labels: Option<Vec<String>>,

    /// Restrict to edge changes whose type is one of these (exact match).
    #[schemars(
        description = "Optional list of edge types to match (exact). If set, only matching edge \
                       changes are delivered; combine with node_labels to receive both kinds."
    )]
    pub edge_types: Option<Vec<String>>,

    /// Restrict to these change types: "created" / "modified" / "deleted".
    #[schemars(
        description = "Optional list of change types to match: \"created\", \"modified\", \
                       \"deleted\". Independent AND with the label/type filters."
    )]
    pub change_types: Option<Vec<String>>,

    /// Opaque resume token from a prior response's `resume_token`. When set, the
    /// call first catches up via `list_changes` and returns immediately if any
    /// changes are already available.
    #[schemars(
        description = "Opaque resume token from a prior response's resume_token; catch up from \
                       exactly after it. Omit to block for the next new change."
    )]
    pub from_token: Option<String>,

    /// Maximum time to block for a change, in milliseconds (default 25000, hard
    /// cap 60000).
    #[schemars(
        description = "Maximum time to block for a change, in milliseconds (default 25000, capped \
                       at 60000). A call that times out returns an empty changes array with \
                       timed_out:true and a resume_token to poll again."
    )]
    pub timeout_ms: Option<u64>,

    /// Maximum number of changes to return in the catch-up path (default 100,
    /// max 10000).
    #[schemars(
        description = "Maximum number of changes to return in the catch-up path (default 100, max \
                       10000)."
    )]
    pub limit: Option<usize>,

    /// Optional namespace scope (Issue #3349): a single namespace name, an array
    /// of names (union), or "all". Omit to receive changes from the default
    /// namespace only (isolated-by-default); use "all" for every namespace.
    #[schemars(
        description = "Optional namespace scope: a single namespace name, an array of names \
                       (union), or \"all\". Omit for the default namespace only; \"all\" for every \
                       namespace. A subscription scoped to a namespace never receives another \
                       namespace's changes."
    )]
    pub namespace: Option<serde_json::Value>,
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

/// Request for a belief-revision audit of an entity (Issue #3362).
///
/// Reader-class. Scopes an entity's stored bi-temporal history into a
/// classified revision sequence (correction / world-change / retraction /
/// reaffirmation) plus its confidence trajectory.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetBeliefRevisionsRequest {
    /// The entity kind to audit: `node` or `edge`.
    #[schemars(description = "Entity kind to audit: 'node' or 'edge'")]
    pub entity_kind: String,

    /// The unique identifier of the node or edge to audit.
    #[schemars(description = "The unique identifier of the node or edge to audit")]
    pub id: u64,

    /// Optionally scope the audit to a single property key. Only revisions that
    /// touched this key are emitted; an unknown key is an `INVALID_ARGUMENT`.
    #[serde(default)]
    #[schemars(
        description = "Optional property key to scope the audit to; only revisions touching \
                       this key are returned. A key the entity never had is an INVALID_ARGUMENT."
    )]
    pub property_key: Option<String>,

    /// Optionally time-travel the audit itself: revisions recorded after this
    /// transaction time are excluded (ISO 8601 / RFC 3339 or microseconds since
    /// epoch).
    #[serde(default)]
    #[schemars(
        description = "Optional transaction-time coordinate (ISO 8601 / RFC 3339 or microseconds \
                       since epoch); revisions recorded after it are excluded"
    )]
    pub as_of_transaction_time: Option<String>,

    /// Maximum number of revisions to return (default 100, max 1000). A `limit`
    /// of 0 is an `INVALID_ARGUMENT`.
    #[serde(default)]
    #[schemars(description = "Maximum revisions to return (default 100, max 1000); 0 is rejected")]
    pub limit: Option<usize>,
}

/// Request to produce a signed audit export of an entity's history (Issue #3358).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AuditExportRequest {
    /// The entity kind to export: `node` or `edge`.
    #[schemars(description = "Entity kind to export: 'node' or 'edge'")]
    pub entity_type: String,

    /// The unique identifier of the entity.
    #[schemars(description = "The unique identifier of the node or edge to export")]
    pub entity_id: u64,

    /// Operator-supplied database identity recorded in the artifact.
    #[serde(default)]
    #[schemars(description = "Optional database identity recorded in the artifact metadata")]
    pub database_id: Option<String>,

    /// Property keys to redact at export (values omitted, redaction recorded).
    #[serde(default)]
    #[schemars(
        description = "Optional list of property keys to redact at export; their values are \
                       omitted but the redaction is recorded and remains verifiable"
    )]
    pub redact_keys: Vec<String>,
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

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). Results are filtered to the scope \
                       and vector ranking is filter-complete (over-fetched so the returned top-k \
                       are genuinely in-scope; scores/order preserved, results never dropped or \
                       reordered to meet the scope). Omitted ⇒ the \"default\" namespace only \
                       (isolated-by-default); unknown ns ⇒ NOT_FOUND; empty array ⇒ \
                       INVALID_ARGUMENT."
    )]
    pub namespace: Option<serde_json::Value>,
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

    /// Optional per-call resource-limit overrides (Issue #3368): `timeout_ms`,
    /// `max_result_rows`, and `max_response_bytes`. Each is folded against the
    /// server default and an operator ceiling; an override above the ceiling is
    /// rejected with `INVALID_ARGUMENT`. Omitting `limits` uses the server
    /// defaults unchanged.
    #[schemars(
        description = "Optional per-call resource-limit overrides: {timeout_ms, max_result_rows, \
                       max_response_bytes}. Each is bounded by an operator ceiling (over-ceiling \
                       values are rejected with INVALID_ARGUMENT). 0 means unlimited (only under \
                       an unbounded ceiling)."
    )]
    pub limits: Option<super::limits::QueryLimitsOverride>,

    /// Optional namespace read scope (Issue #3349).
    #[schemars(
        description = "Optional namespace read scope: a single namespace name (string), an array \
                       of names (union), or \"all\" (no filter). The query executes scoped — \
                       produced entities are filtered to the scope and traversal never crosses an \
                       out-of-scope edge/node. Omitted ⇒ the \"default\" namespace only \
                       (isolated-by-default). Unknown namespace ⇒ NOT_FOUND; empty array ⇒ \
                       INVALID_ARGUMENT. A restricting scope on a multi-variable-binding or \
                       mutating statement returns a structured unsupported_construct error \
                       (never silently unscoped)."
    )]
    pub namespace: Option<serde_json::Value>,
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
// Database Stats (Issue #3222)
// ============================================================================

/// Request for a holistic database statistics snapshot.
///
/// Takes no arguments — the tool always returns the full snapshot in a
/// single call.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct DatabaseStatsRequest {}

// ============================================================================
// Namespace management (Issue #3349, PR3b)
// ============================================================================

/// Request to create (register) an agent-scoped namespace.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateNamespaceRequest {
    /// The namespace name to register (e.g. "agent:planner").
    #[schemars(
        description = "The namespace name to register, e.g. 'agent:planner'. Charset \
                       [A-Za-z0-9._:/-], max 128 bytes. The implicit 'default' namespace and the \
                       reserved 'all' selector cannot be created (CONFLICT / INVALID_ARGUMENT). \
                       Registering a namespace up front makes an empty one listable/describable; \
                       writing to an unknown namespace also auto-registers it."
    )]
    pub name: String,

    /// Optional human-readable description.
    #[schemars(description = "Optional human-readable description of the namespace's purpose")]
    pub description: Option<String>,
}

/// Request to list all registered namespaces. Takes no arguments.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ListNamespacesRequest {}

/// Request to describe a single namespace by name.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DescribeNamespaceRequest {
    /// The namespace name to describe.
    #[schemars(
        description = "The namespace name to describe. The implicit 'default' namespace always \
                       resolves; an unregistered name is NOT_FOUND (details.namespace)."
    )]
    pub name: String,
}

// ============================================================================
// GDPR crypto-shred (Issue #3359, Slice 4b) — ADMIN-class tools
// ============================================================================

/// One designation target for `designate_subject`: a whole entity, or a
/// specific set of property keys on that entity.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DesignationTargetInput {
    /// Entity kind: `"node"` or `"edge"`.
    #[schemars(description = "Entity kind: \"node\" or \"edge\"")]
    pub entity_kind: String,

    /// The entity id to designate.
    #[schemars(description = "The id of the node or edge to designate under the subject")]
    pub id: u64,

    /// Optional property keys to seal. Omit (or empty) to seal the whole entity.
    #[schemars(
        description = "Optional property keys to seal under the subject. Omit or leave empty to \
                       seal the WHOLE entity; provide keys to seal only those specific properties."
    )]
    pub keys: Option<Vec<String>>,
}

/// Arguments for the admin-gated `designate_subject` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DesignateSubjectRequest {
    /// The erasure-subject id to designate targets under.
    #[schemars(
        description = "The erasure-subject id (non-empty, <=256 bytes, no control characters). \
                       Designating an already-active subject merges the new targets into it."
    )]
    pub subject_id: String,

    /// One or more designation targets (whole entities and/or property keys).
    #[schemars(
        description = "One or more designation targets (whole nodes/edges and/or specific \
                       property keys) to seal under the subject. Must be non-empty."
    )]
    pub targets: Vec<DesignationTargetInput>,
}

/// Arguments for the admin-gated `erase_subject` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EraseSubjectRequest {
    /// The erasure-subject id to irreversibly erase.
    #[schemars(
        description = "The erasure-subject id to irreversibly erase. Destroys the subject's key \
                       material so its sealed payload becomes permanently undecryptable and \
                       returns a signed erasure attestation. Re-erasing is an idempotent no-op \
                       returning the recorded attestation."
    )]
    pub subject_id: String,
}

// ============================================================================
// Provenance hash chain (Issue #3351)
// ============================================================================

/// Request to verify the tamper-evident provenance hash chain.
///
/// Three independent modes, resolved in this precedence order:
///
/// 1. **Anchor extension** — pass `against` (a previously exported chain head,
///    the object `export_chain_head` returns) to prove the current chain is an
///    append-only extension of that anchor, detecting rollback (truncation)
///    and fork (divergence).
/// 2. **Entity-scoped** — pass `entity_kind` (`"node"`/`"edge"`) and `id` to
///    recompute only that entity's contribution to the chain.
/// 3. **Full** — pass nothing to walk the whole chain from genesis and
///    localize the earliest broken sequence number on tamper.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct VerifyChainRequest {
    /// Entity kind for an entity-scoped verify: `node` or `edge`.
    #[serde(default)]
    #[schemars(
        description = "For an entity-scoped verify, the entity kind: 'node' or 'edge' \
                       (requires `id`). Omit for a full-chain verify."
    )]
    pub entity_kind: Option<String>,

    /// Entity id for an entity-scoped verify.
    #[serde(default)]
    #[schemars(
        description = "For an entity-scoped verify, the entity id (requires `entity_kind`)."
    )]
    pub id: Option<u64>,

    /// A previously exported chain head to verify append-only extension
    /// against (as returned by `export_chain_head`).
    #[serde(default)]
    #[schemars(
        description = "Optional previously-exported chain head object (as returned by \
                       export_chain_head) to verify the current chain append-only-extends it, \
                       detecting rollback (truncation) and fork (divergence)."
    )]
    pub against: Option<serde_json::Value>,
}

/// Request to export the current chain head as an external anchor.
///
/// Takes no arguments — the tool always returns the current head checkpoint.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ExportChainHeadRequest {}

// ============================================================================
// Response Types (for serialization)
// ============================================================================

/// Bi-temporal bounds of the exact version a read response reflects
/// (Issue #3232).
///
/// Stamped on every node/edge read response so an LLM/caller always knows
/// *when* the returned fact was true in reality (valid time) and *when* it
/// was recorded (transaction time), without a follow-up history call.
///
/// Bounds are RFC 3339 strings with microsecond precision. Open-ended bounds
/// (a still-valid fact / a still-recorded version) are serialized as explicit
/// JSON `null` -- the keys are always present, never omitted. Intervals are
/// half-open (`[start, end)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalBounds {
    /// When the fact became true in reality (RFC 3339).
    pub valid_from: String,
    /// When the fact stopped being true, or `null` if still valid.
    pub valid_to: Option<String>,
    /// When this version was recorded in the database (RFC 3339).
    pub transaction_from: String,
    /// When this version was superseded/removed, or `null` if still current
    /// in the database.
    pub transaction_to: Option<String>,
    /// `true` iff the version's transaction interval is still open (it *is*
    /// the currently-recorded version) AND the current wallclock time falls
    /// within its valid interval -- i.e. the returned version is the live,
    /// current version. `false` for superseded versions returned by
    /// point-in-time reads and for facts whose valid time has ended (or not
    /// yet begun). The valid-time comparison is at wallclock (microsecond)
    /// granularity, so the HLC logical component of a commit timestamp never
    /// affects the answer.
    pub is_current: bool,
}

impl TemporalBounds {
    /// Format a timestamp's wallclock microseconds as an RFC 3339 string
    /// with microsecond precision (e.g. `2026-07-07T12:00:00.000000Z`).
    ///
    /// Total: a wallclock value outside chrono's representable range falls
    /// back to the raw microsecond count as a string (mirroring
    /// `time::to_iso8601`'s totality) instead of silently rendering the
    /// 1970 epoch.
    fn to_rfc3339_micros(ts: crate::core::temporal::Timestamp) -> String {
        let micros = ts.wallclock();
        match chrono::DateTime::from_timestamp_micros(micros) {
            Some(dt) => dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            None => micros.to_string(),
        }
    }

    /// Convert a range end bound: `TIMESTAMP_MAX` (open-ended) becomes
    /// `None` (serialized as JSON `null`), anything else an RFC 3339 string.
    fn end_bound(ts: crate::core::temporal::Timestamp) -> Option<String> {
        if ts == crate::core::temporal::TIMESTAMP_MAX {
            None
        } else {
            Some(Self::to_rfc3339_micros(ts))
        }
    }

    /// Build the bounds for a version, evaluating `is_current` at an explicit
    /// `now` (Issue #3391): a request serializing many entities captures its
    /// wallclock once and evaluates every entity against that same instant,
    /// so entities in one response can never disagree about when "now" is.
    ///
    /// `is_current` semantics (Issue #3232):
    ///
    /// Transaction dimension: current iff the transaction interval is
    /// open-ended. An open transaction interval IS by definition the
    /// currently-recorded version; comparing the HLC commit timestamp
    /// against SystemTime-now would be a clock artifact (HLC stamps carry
    /// a logical component that orders after SystemTime-now within the
    /// same microsecond, and the HLC frontier can legitimately run ahead
    /// of SystemTime for a while after a tolerated backward OS-clock
    /// step), transiently misreporting a live head version as not
    /// current.
    ///
    /// Valid dimension: still distinguishes not-yet-valid (future
    /// valid_from) and expired (past valid_to) facts, but compared at
    /// wallclock (microsecond) granularity so the HLC logical component
    /// cannot flip the answer within the current microsecond.
    /// Sub-microsecond edge rounding toward "current" is acceptable for a
    /// convenience boolean; the authoritative data is the bounds
    /// themselves.
    pub fn from_interval_at(
        interval: &crate::core::temporal::BiTemporalInterval,
        now: crate::core::temporal::Timestamp,
    ) -> Self {
        let valid = interval.valid_time();
        let tx_open = interval.transaction_time().is_current();
        let valid_contains_now = valid.start().wallclock() <= now.wallclock()
            && (valid.is_current() || now.wallclock() < valid.end().wallclock());
        TemporalBounds {
            valid_from: Self::to_rfc3339_micros(valid.start()),
            valid_to: Self::end_bound(valid.end()),
            transaction_from: Self::to_rfc3339_micros(interval.transaction_time().start()),
            transaction_to: Self::end_bound(interval.transaction_time().end()),
            is_current: tx_open && valid_contains_now,
        }
    }
}

impl From<&crate::core::temporal::BiTemporalInterval> for TemporalBounds {
    /// Single-entity convenience: evaluates at a fresh wallclock read. Bulk
    /// responses should call [`TemporalBounds::from_interval_at`] with one
    /// request-scoped `now` instead (Issue #3391).
    fn from(interval: &crate::core::temporal::BiTemporalInterval) -> Self {
        Self::from_interval_at(interval, crate::core::temporal::time::now())
    }
}

/// Serializable node representation for MCP responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResponse {
    pub id: u64,
    pub label: String,
    pub properties: HashMap<String, serde_json::Value>,
    /// The node's immutable namespace (Issue #3349), surfaced as a first-class
    /// field (the ride-along property is elided). Omitted for `default`-namespace
    /// entities so a single-agent (`default`-only) response stays byte-identical
    /// to pre-namespace behavior (design AC1); present whenever the namespace is
    /// non-default.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub namespace: Option<String>,
    /// The current version's write-time provenance bundle, if any. Omitted
    /// (never a fabricated `null`) when the version has none (Issue #3224).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provenance: Option<crate::core::provenance::Provenance>,
    /// Bi-temporal bounds of the exact version this response reflects
    /// (Issue #3232). Always present on normal reads; `None` only as a
    /// best-effort degrade when the version metadata could not be loaded.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub temporal: Option<TemporalBounds>,
}

/// Serializable edge representation for MCP responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeResponse {
    pub id: u64,
    pub source_id: u64,
    pub target_id: u64,
    pub label: String,
    pub properties: HashMap<String, serde_json::Value>,
    /// The edge's immutable namespace (Issue #3349), surfaced as a first-class
    /// field (the ride-along property is elided). Omitted for `default`-namespace
    /// entities so a single-agent (`default`-only) response stays byte-identical
    /// to pre-namespace behavior (design AC1); present whenever the namespace is
    /// non-default.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub namespace: Option<String>,
    /// The current version's write-time provenance bundle, if any. Omitted
    /// (never a fabricated `null`) when the version has none (Issue #3224).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provenance: Option<crate::core::provenance::Provenance>,
    /// Bi-temporal bounds of the exact version this response reflects
    /// (Issue #3232). Always present on normal reads; `None` only as a
    /// best-effort degrade when the version metadata could not be loaded.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub temporal: Option<TemporalBounds>,
}

/// Similarity search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityResult {
    pub node: NodeResponse,
    pub score: f32,
    /// Provenance-weighted fusion breakdown (Issue #3372), present only when a
    /// `fusion_policy` was supplied on the request. Omitted (never a fabricated
    /// `null`) otherwise, so omitting `fusion_policy` reproduces prior bytes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub score_breakdown: Option<serde_json::Value>,
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
    /// Provenance-weighted fusion breakdown (Issue #3372), present only when a
    /// `fusion_policy` was supplied on the request. Omitted otherwise so prior
    /// behavior is byte-identical.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub score_breakdown: Option<serde_json::Value>,
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

// ============================================================================
// Semantic-search analysis tools (Issue #2907)
//
// Six read-only tools exposing the stable `semantic-search` cohort's analysis
// primitives over MCP. They are advertised unconditionally (Design A); when the
// `semantic-search` feature is not compiled in, the handler bodies return a
// structured `FAILED_PRECONDITION` (`required_feature: "semantic-search"`)
// rather than being absent from the catalog.
// ============================================================================

/// Request for `semantic_path` — vector-similarity-guided A* pathfinding.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SemanticPathRequest {
    /// Starting node id.
    #[schemars(description = "Starting node id for the path search.")]
    pub start: u64,

    /// Target node id.
    #[schemars(description = "Target node id for the path search.")]
    pub end: u64,

    /// The property holding the vector embedding used to guide the search.
    #[schemars(
        description = "The property name holding the vector embedding used to guide the A* search \
                       (e.g. 'embedding'). A vector index must be enabled for this property."
    )]
    pub property_name: String,

    /// Optional maximum traversal depth (clamped to the server maximum).
    #[schemars(
        description = "Optional maximum traversal depth. Clamped to the server maximum (20). Used \
                       to derive a bounded node-expansion budget so the search cannot run \
                       unbounded on large graphs."
    )]
    pub max_depth: Option<usize>,
}

/// Request for `concept_analogy` — vector analogy `a : b :: c : ?`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ConceptAnalogyRequest {
    /// First node of the analogy pair (`a`).
    #[schemars(description = "Node id 'a' in the analogy a : b :: c : ?")]
    pub a: u64,

    /// Second node of the analogy pair (`b`).
    #[schemars(description = "Node id 'b' in the analogy a : b :: c : ?")]
    pub b: u64,

    /// Third node — the base of the completion (`c`).
    #[schemars(description = "Node id 'c' in the analogy a : b :: c : ? (the completion base)")]
    pub c: u64,

    /// The property holding the vector embedding.
    #[schemars(
        description = "The property name holding the vector embedding (e.g. 'embedding'). A vector \
                       index must be enabled for this property."
    )]
    pub property_name: String,

    /// Number of results to return (clamped to the server maximum).
    #[schemars(description = "Number of results to return (default 10, clamped to 1000).")]
    pub k: Option<usize>,
}

/// Request for `concept_mean` — nearest neighbours to the centroid of a set.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ConceptMeanRequest {
    /// The node ids whose embeddings are averaged.
    #[schemars(
        description = "Node ids whose embeddings are averaged; the result ranks nodes nearest the \
                       centroid. Capped at 10000 ids."
    )]
    pub nodes: Vec<u64>,

    /// The property holding the vector embedding.
    #[schemars(
        description = "The property name holding the vector embedding (e.g. 'embedding'). A vector \
                       index must be enabled for this property."
    )]
    pub property_name: String,

    /// Number of results to return (clamped to the server maximum).
    #[schemars(description = "Number of results to return (default 10, clamped to 1000).")]
    pub k: Option<usize>,
}

/// Request for `find_duplicate_candidates` — near-duplicate entity detection.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindDuplicateCandidatesRequest {
    /// The node whose near-duplicates are sought.
    #[schemars(description = "The node id whose near-duplicate candidates are sought.")]
    pub node_id: u64,

    /// The property whose vector index is used.
    ///
    /// v1 note: the underlying similarity query resolves against the node's own
    /// indexed embedding; `property_name` selects the index whose existence is
    /// validated. A per-property selector is a documented follow-up.
    #[schemars(
        description = "The property name whose vector index is validated (e.g. 'embedding'). v1 \
                       note: the search uses the node's indexed embedding; property_name selects \
                       the index to validate rather than an arbitrary property vector."
    )]
    pub property_name: String,

    /// Minimum cosine similarity for a candidate to be reported.
    #[schemars(
        description = "Minimum cosine similarity in [0, 1] for a candidate to be reported \
                       (default 0.9)."
    )]
    pub threshold: Option<f32>,

    /// Maximum number of candidates to return (clamped to the server maximum).
    #[schemars(
        description = "Maximum number of candidates to return (default 10, clamped to 1000)."
    )]
    pub limit: Option<usize>,
}

/// Request for `semantic_horizon` — semantic event-horizon mapping.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SemanticHorizonRequest {
    /// The seed node the horizon is mapped from.
    #[schemars(description = "The seed node id the semantic event horizon is mapped from.")]
    pub seed: u64,

    /// The property holding the vector embedding.
    #[schemars(
        description = "The property name holding the vector embedding (e.g. 'embedding'). A vector \
                       index must be enabled for this property."
    )]
    pub property_name: String,

    /// Similarity threshold in [0, 1]; neighbours below it form the horizon.
    #[schemars(
        description = "Similarity threshold in [0, 1]. Neighbours at or above it are interior; \
                       the first neighbours to fall below form the horizon (boundary)."
    )]
    pub threshold: f32,

    /// Optional maximum traversal depth (clamped to the server maximum).
    #[schemars(
        description = "Optional maximum traversal depth from the seed (default 20, clamped to 20)."
    )]
    pub max_depth: Option<usize>,
}

/// Request for `context_aspects` — disentangled neighbourhood aspects.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ContextAspectsRequest {
    /// The node whose neighbourhood is decomposed.
    #[schemars(description = "The node id whose local context (neighbourhood) is decomposed.")]
    pub node_id: u64,

    /// The property holding the vector embedding.
    #[schemars(
        description = "The property name holding the vector embedding (e.g. 'embedding'). A vector \
                       index must be enabled for this property."
    )]
    pub property_name: String,

    /// Number of aspects (clusters) to extract (clamped to the server maximum).
    #[schemars(
        description = "Number of semantic aspects (clusters) to extract (default 10, clamped to 1000)."
    )]
    pub k: Option<usize>,

    /// When true, return each aspect's full centroid vector instead of the
    /// elided descriptor.
    #[schemars(
        description = "When true, return each aspect's full centroid float array instead of the \
                       elided {type, dim, elided:true} descriptor (default false)."
    )]
    pub include_vectors: Option<bool>,
}

// ============================================================================
// Temporal drift-alarm management (Issue #3367, `semantic-temporal`)
// ============================================================================

/// Request to declare a drift monitor (`create_drift_monitor`, Write).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateDriftMonitorRequest {
    /// The vector property key to watch (e.g. `"embedding"`).
    #[schemars(
        description = "The vector property key to watch (e.g. 'embedding'). A vector \
                             index must be enabled for this property."
    )]
    pub property_key: String,

    /// Optional node-label restriction. Required when `target` is
    /// `label_centroid`.
    #[serde(default)]
    #[schemars(
        description = "Optional node-label restriction. Required when target is \
                             'label_centroid'."
    )]
    pub label: Option<String>,

    /// Optional explicit set of node ids to watch.
    #[serde(default)]
    #[schemars(
        description = "Optional explicit set of node ids to watch. An empty array is \
                             an INVALID_ARGUMENT."
    )]
    pub entities: Option<Vec<u64>>,

    /// Distance metric: `cosine`, `euclidean`, or `angular`. Must match the
    /// property's vector-index metric.
    #[schemars(
        description = "Distance metric: 'cosine', 'euclidean', or 'angular'. Must be \
                             consistent with the property's vector-index metric."
    )]
    pub metric: String,

    /// Firing threshold (strict `>`); must be positive and finite.
    #[schemars(description = "Firing threshold (strict >). Must be positive and finite.")]
    pub threshold: f32,

    /// Comparison window in microseconds: current embedding vs the embedding as
    /// of `now - window`.
    #[schemars(
        description = "Comparison window in microseconds: the current embedding is \
                             compared against the embedding as of (now - window). Must be \
                             non-zero."
    )]
    pub window_micros: u64,

    /// Firing target: `per_entity` or `label_centroid`.
    #[serde(default = "default_drift_target")]
    #[schemars(
        description = "Firing target: 'per_entity' (default) fires per drifted entity; \
                             'label_centroid' fires once for the label's population centroid \
                             (requires label)."
    )]
    pub target: String,

    /// Evaluation mode: `on_write` or `scheduled`.
    #[serde(default = "default_drift_mode")]
    #[schemars(
        description = "Evaluation mode: 'on_write' (default, reactive) or 'scheduled' \
                             (fixed cadence; provide scheduled_interval_micros)."
    )]
    pub mode: String,

    /// Evaluation interval in microseconds when `mode` is `scheduled`.
    #[serde(default)]
    #[schemars(
        description = "Evaluation interval in microseconds; required when mode is \
                             'scheduled', ignored otherwise."
    )]
    pub scheduled_interval_micros: Option<u64>,
}

fn default_drift_target() -> String {
    "per_entity".to_string()
}
fn default_drift_mode() -> String {
    "on_write".to_string()
}

/// Request to list all declared drift monitors (`list_drift_monitors`, Read).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListDriftMonitorsRequest {}

/// Request to delete a drift monitor (`delete_drift_monitor`, Write).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeleteDriftMonitorRequest {
    /// The id of the monitor to delete.
    #[schemars(
        description = "The id of the drift monitor to delete (from create/list). \
                             An unknown id is a NOT_FOUND."
    )]
    pub id: u64,
}

/// Request to query fired drift alarms (`query_drift_alarms`, Read).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct QueryDriftAlarmsRequest {
    /// Restrict to a single monitor id.
    #[serde(default)]
    #[schemars(description = "Optional: restrict to alarms fired by this monitor id.")]
    pub monitor_id: Option<u64>,

    /// Restrict to a single label.
    #[serde(default)]
    #[schemars(description = "Optional: restrict to alarms carrying this label.")]
    pub label: Option<String>,

    /// Restrict by resolved state (omit for both).
    #[serde(default)]
    #[schemars(description = "Optional: restrict by resolved state (true/false). Omit for both.")]
    pub resolved: Option<bool>,

    /// Inclusive lower bound (transaction time) on fire time.
    #[serde(default)]
    #[schemars(
        description = "Optional inclusive lower bound (ISO 8601 / RFC 3339 or integer \
                             microseconds since epoch) on the alarm fire (transaction) time. \
                             Both time-range bounds must be supplied together."
    )]
    pub time_range_start: Option<String>,

    /// Exclusive upper bound (transaction time) on fire time.
    #[serde(default)]
    #[schemars(
        description = "Optional exclusive upper bound (ISO 8601 / RFC 3339 or integer \
                             microseconds since epoch) on the alarm fire (transaction) time."
    )]
    pub time_range_end: Option<String>,

    /// Maximum alarms to return (clamped to 1000).
    #[serde(default)]
    #[schemars(description = "Maximum alarms to return (default 100, clamped to 1000).")]
    pub limit: Option<usize>,
}

/// Request to resolve a drift alarm (`resolve_drift_alarm`, Write).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ResolveDriftAlarmRequest {
    /// The graph node id of the alarm to resolve.
    #[schemars(
        description = "The node id of the drift alarm to resolve (a recorded, AS OF-stable \
                             update; the alarm is never deleted)."
    )]
    pub alarm_id: u64,
}

// ============================================================================
// Contradiction genealogy (Issue #3352, `semantic-temporal`)
// ============================================================================

/// A version-pinned competing-claim reference for `contradiction_genealogy`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ClaimRefRequest {
    /// Whether the claim belongs to a node or an edge.
    #[schemars(description = "The claim's entity kind: 'node' or 'edge'.")]
    pub entity_kind: String,
    /// The entity id the claim belongs to.
    #[schemars(description = "The entity id the claim belongs to (node id or edge id).")]
    pub id: u64,
    /// The version id that asserted the claim.
    #[schemars(description = "The version id that asserted the claim.")]
    pub version: u64,
}

/// Request to reconstruct a contradiction genealogy (`contradiction_genealogy`,
/// Read). Provide EITHER `entity_kind` + `id` + `property` (a single entity's
/// property) OR an explicit `claims` set.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ContradictionGenealogyRequest {
    /// The entity kind when targeting one entity's property: 'node' or 'edge'.
    #[serde(default)]
    #[schemars(
        description = "Entity kind ('node' or 'edge') when targeting a single entity's \
                             property. Provide with id and property, or use claims instead."
    )]
    pub entity_kind: Option<String>,

    /// The entity id when targeting one entity's property.
    #[serde(default)]
    #[schemars(description = "Entity id when targeting a single entity's property.")]
    pub id: Option<u64>,

    /// The property key when targeting one entity's property.
    #[serde(default)]
    #[schemars(description = "Property key when targeting a single entity's property.")]
    pub property: Option<String>,

    /// An explicit competing-claim set (may span entities); alternative to
    /// `entity_kind`/`id`/`property`.
    #[serde(default)]
    #[schemars(
        description = "Explicit competing-claim set (may span entities). Alternative to \
                             entity_kind/id/property; an empty array is an INVALID_ARGUMENT."
    )]
    pub claims: Option<Vec<ClaimRefRequest>>,

    /// Time-travel: only claims recorded at or before this transaction time.
    #[serde(default)]
    #[schemars(
        description = "Optional AS OF transaction-time coordinate (ISO 8601 / RFC 3339 or \
                             integer microseconds since epoch): only claims recorded at or before \
                             it are considered."
    )]
    pub as_of_transaction_time: Option<String>,

    /// Bound the number of competing claims returned.
    #[serde(default)]
    #[schemars(
        description = "Optional cap on the number of competing claims returned (sets \
                             truncated)."
    )]
    pub max_claims: Option<usize>,

    /// Bound the number of source summaries returned.
    #[serde(default)]
    #[schemars(
        description = "Optional cap on the number of per-source summaries returned (sets \
                             truncated)."
    )]
    pub max_sources: Option<usize>,
}

/// Request to scan for contradictions (`find_contradictions`, Read).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindContradictionsRequest {
    /// Which entity kinds to scan: `nodes`, `edges`, or `both`.
    #[serde(default = "default_entity_kind_scope")]
    #[schemars(description = "Entity kinds to scan: 'nodes', 'edges', or 'both' (default).")]
    pub entity_kind: String,

    /// Optional node-label / edge-type filter.
    #[serde(default)]
    #[schemars(description = "Optional node-label / edge-type filter.")]
    pub label: Option<String>,

    /// Optional single property to analyze.
    #[serde(default)]
    #[schemars(
        description = "Optional single property to analyze (otherwise every property that \
                             ever appeared is analyzed)."
    )]
    pub property: Option<String>,

    /// Keep only contradictions whose divergence valid-time is in this window.
    #[serde(default)]
    #[schemars(
        description = "Optional valid-time window lower bound (ISO 8601 / RFC 3339 or \
                             microseconds); both window bounds must be supplied together."
    )]
    pub valid_time_start: Option<String>,
    /// Valid-time window upper bound.
    #[serde(default)]
    #[schemars(description = "Optional valid-time window upper bound.")]
    pub valid_time_end: Option<String>,

    /// Keep only contradictions whose divergence transaction-time is in this
    /// window.
    #[serde(default)]
    #[schemars(
        description = "Optional transaction-time window lower bound; both bounds together."
    )]
    pub transaction_time_start: Option<String>,
    /// Transaction-time window upper bound.
    #[serde(default)]
    #[schemars(description = "Optional transaction-time window upper bound.")]
    pub transaction_time_end: Option<String>,

    /// Page size (default 100, clamped to 1000).
    #[serde(default)]
    #[schemars(description = "Page size (default 100, clamped to 1000).")]
    pub limit: Option<usize>,

    /// Page offset.
    #[serde(default)]
    #[schemars(description = "Page offset for pagination (default 0).")]
    pub offset: Option<usize>,
}

fn default_entity_kind_scope() -> String {
    "both".to_string()
}

// ============================================================================
// Counterfactual replay (Issue #3357, `semantic-temporal`)
// ============================================================================

/// Request to replay history excluding a source (`counterfactual_replay`,
/// Read). The real database is never mutated; the response reports the
/// blast-radius divergence and carries a `counterfactual: true` marker.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CounterfactualReplayRequest {
    /// A human-readable name for the counterfactual view.
    #[schemars(description = "A human-readable name for the counterfactual view.")]
    pub name: String,

    /// Exclude all writes attributed to this single source.
    #[serde(default)]
    #[schemars(
        description = "Exclude all writes attributed to this single source. Provide \
                             exactly one of exclude_source or exclude_sources."
    )]
    pub exclude_source: Option<String>,

    /// Exclude all writes attributed to any source in this set (any-of).
    #[serde(default)]
    #[schemars(
        description = "Exclude all writes attributed to any source in this set (any-of). \
                             Provide exactly one of exclude_source or exclude_sources."
    )]
    pub exclude_sources: Option<Vec<String>>,

    /// Bound exclusion to writes recorded at or after this transaction time.
    #[serde(default)]
    #[schemars(
        description = "Optional inclusive lower bound (ISO 8601 / RFC 3339 or microseconds) \
                             on a write's transaction time for it to be excluded."
    )]
    pub within_transaction_from: Option<String>,

    /// Bound exclusion to writes recorded before this transaction time.
    #[serde(default)]
    #[schemars(
        description = "Optional exclusive upper bound (ISO 8601 / RFC 3339 or microseconds) \
                             on a write's transaction time for it to be excluded."
    )]
    pub within_transaction_to: Option<String>,

    /// Maximum recorded versions to materialize before failing fast.
    #[serde(default)]
    #[schemars(
        description = "Optional cap on recorded versions to materialize; exceeding it is a \
                             FAILED_PRECONDITION (history too large)."
    )]
    pub max_replay_versions: Option<usize>,
}

// ============================================================================
// Trust propagation (Issue #3382, `semantic-reasoning`)
// ============================================================================

/// Request for a computed-trust breakdown (`trust_breakdown`, Read).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TrustBreakdownRequest {
    /// The root fact's entity kind: 'node' or 'edge'.
    #[schemars(description = "The root fact's entity kind: 'node' or 'edge'.")]
    pub entity_kind: String,

    /// The root entity's id.
    #[schemars(description = "The root entity's id (node id or edge id per entity_kind).")]
    pub id: u64,

    /// The root fact's version id (trust is version-pinned).
    #[schemars(
        description = "The root fact's version id. Trust is version-pinned; use the \
                             version whose computed-confidence breakdown you want."
    )]
    pub version: u64,

    /// Maximum transitive depth to expand before truncating.
    #[serde(default)]
    #[schemars(
        description = "Maximum transitive depth to expand before marking a subtree \
                             truncated (defaults to the store's depth cap)."
    )]
    pub max_depth: Option<usize>,

    /// Maximum descendant nodes to serialize before truncating.
    #[serde(default)]
    #[schemars(
        description = "Maximum descendant breakdown nodes to serialize (the root is always \
                             emitted and does not count); defaults to the store's node cap."
    )]
    pub limit: Option<usize>,

    /// Optional AS OF transaction-time coordinate.
    #[serde(default)]
    #[schemars(
        description = "Optional AS OF transaction-time coordinate (ISO 8601 / RFC 3339 or \
                             microseconds): evaluate lineage and confidences as recorded by then."
    )]
    pub as_of_transaction_time: Option<String>,
}

/// Request to list active trust policies (`list_trust_policies`, Read).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListTrustPoliciesRequest {}
