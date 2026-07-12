//! AletheiaDB MCP Server implementation.
//!
//! This module implements the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server
//! for AletheiaDB, enabling Large Language Models (LLMs) to interact with the database.
//!
//! # Overview
//!
//! The MCP server exposes AletheiaDB's capabilities as a set of "tools" that an LLM can invoke.
//! This allows AI agents to:
//! - **Query Knowledge**: Retrieve nodes, edges, and their properties.
//! - **Modify Graph**: Create, update, and delete nodes and edges.
//! - **Semantic Search**: Find similar nodes using vector embeddings.
//! - **Time Travel**: Query the graph as it existed at any point in history.
//! - **Hybrid Search**: Combine graph traversal, vector search, and temporal filtering.
//!
//! # Available Tools
//!
//! The server exposes over 20 tools, categorized by function:
//!
//! | Category | Key Tools | Description |
//! |----------|-----------|-------------|
//! | **Nodes** | `get_node`, `create_node`, `update_node` | CRUD operations for nodes |
//! | **Edges** | `get_edge`, `create_edge`, `traverse` | CRUD and traversal for edges |
//! | **Vector** | `find_similar`, `enable_vector_index` | Semantic similarity search |
//! | **Temporal** | `get_node_at_time`, `find_nodes_at_time`, `get_node_history`, `temporal_extent` | Time-travel queries, history, and dataset extent |
//! | **Hybrid** | `hybrid_query` | Combined graph + vector + temporal queries |
//! | **Schema** | `get_schema` | Discover node labels, edge types, and property keys (optionally bi-temporal) |
//!
//! # Examples
//!
//! Below are examples of how to interact with the key tools using JSON-RPC requests (the underlying protocol of MCP).
//!
//! ## 1. Creating a Node
//!
//! **Request:** `create_node`
//! ```json
//! {
//!   "label": "Person",
//!   "properties": {
//!     "name": "Alice",
//!     "age": 30,
//!     "interests": ["Rust", "Graphs"]
//!   }
//! }
//! ```
//!
//! **Response:**
//! ```json
//! {
//!   "id": 1,
//!   "label": "Person",
//!   "properties": {
//!     "name": "Alice",
//!     "age": 30,
//!     "interests": ["Rust", "Graphs"]
//!   }
//! }
//! ```
//!
//! ## 2. Vector Search
//!
//! **Request:** `find_similar`
//! ```json
//! {
//!   "property_name": "embedding",
//!   "embedding": [0.1, 0.2, 0.3, ...],
//!   "k": 5
//! }
//! ```
//!
//! **Response:**
//! ```json
//! {
//!   "results": [
//!     {
//!       "node": {
//!         "id": 42,
//!         "label": "Document",
//!         "properties": { "title": "Rust Guide", ... }
//!       },
//!       "score": 0.98
//!     },
//!     ...
//!   ],
//!   "count": 5
//! }
//! ```
//!
//! ## 3. Hybrid Query (Graph + Vector + Time)
//!
//! **Request:** `hybrid_query`
//! ```json
//! {
//!   "start_node_id": 1,
//!   "traverse_edge": "KNOWS",
//!   "query_embedding": [0.1, 0.2, ...],
//!   "valid_time": "2024-01-01T00:00:00Z"
//! }
//! ```
//!
//! # Usage
//!
//! The server is typically run as a standalone binary communicating over stdio:
//!
//! ```bash
//! cargo run --bin aletheia-mcp --features mcp-server
//! ```
//!
//! Or embedded within a larger application:
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::mcp::AletheiaMcpServer;
//! // Requires 'rmcp' dependency
//! use rmcp::{ServiceExt, transport::stdio};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let db = Arc::new(AletheiaDB::new()?);
//! let server = AletheiaMcpServer::new(db);
//!
//! // Run the server over stdio (blocks until stdin closes)
//! let service = server.serve(stdio()).await?;
//! service.waiting().await?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use chrono::{DateTime, Utc};
use rmcp::{
    ErrorData as RmcpErrorData, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
};
use serde_json::json;

use crate::api::transaction::WriteOps;
use crate::core::temporal::time;
use crate::core::{
    ChangeFeedQuery, EdgeId, GLOBAL_INTERNER, NodeId, PropertyMap, PropertyMapBuilder,
    PropertyValue, Provenance, Timestamp, VersionId,
};
use crate::db::AletheiaDB;
use crate::index::vector::{DistanceMetric, HnswConfig};
use crate::query::executor::{EntityId as ResultEntityId, EntityResult};

use super::auth::{McpAuthConfig, SessionAuth};
use super::batch::ApplyBatchRequest;
use super::budget;
use super::cursor::{CursorManager, CursorPayload};
use super::error::{McpError, McpErrorCode, query_kind_classification};
use super::tools::*;
use crate::auth::{AuthMode, Principal};

// ============================================================================
// Resource Limits (to prevent DoS attacks)
// ============================================================================

/// Maximum traversal depth to prevent stack overflow and excessive computation.
/// Increased from 10 to 20 to support business scenarios:
/// - Social network analysis (6 degrees of separation)
/// - Supply chain tracking (can exceed 10 hops)
/// - Genealogy and multi-generation queries
///
///   Still provides DoS protection via query timeouts and result limits.
const MAX_TRAVERSAL_DEPTH: usize = 20;

/// Maximum number of results to return in a single query.
const MAX_RESULT_LIMIT: usize = 10_000;

/// Default number of results to return.
const DEFAULT_RESULT_LIMIT: usize = 100;

/// Maximum pagination offset to prevent excessive memory usage.
/// Since we fetch limit+offset rows and then skip, this limits total rows fetched.
const MAX_PAGINATION_OFFSET: usize = 10_000;

/// Maximum k for vector similarity search.
const MAX_VECTOR_K: usize = 1000;

/// Default k for vector similarity search.
const DEFAULT_VECTOR_K: usize = 10;

/// Default transaction time placeholder string.
const TRANSACTION_TIME_NOW: &str = "now";

/// Default maximum number of operations accepted by a single `apply_batch`
/// call (Issue #3231). Deliberately far below the core transaction buffer's
/// own DoS cap (`WriteBuffer::DEFAULT_MAX_OPERATIONS` = 50,000), so the MCP
/// surface bound is always the one that fires, with the limit echoed in the
/// rejection per the #3226 completeness convention.
pub(crate) const DEFAULT_MAX_BATCH_OPERATIONS: usize = 1000;

/// AletheiaDB MCP Server.
///
/// Exposes AletheiaDB's graph, vector, and temporal capabilities through the Model Context Protocol.
/// This struct implements `rmcp::ServerHandler` to handle tool calls from an MCP client (typically an LLM).
///
/// # Resource Limits
///
/// To prevent Denial of Service (DoS) attacks and excessive resource usage, the server enforces several limits:
/// - **Max Traversal Depth**: 20 hops (prevents infinite loops and stack overflows)
/// - **Max Results**: 10,000 items (prevents OOM on large result sets)
/// - **Max Vector K**: 1,000 nearest neighbors (limits expensive similarity calculations)
///
/// # Thread Safety
///
/// The server is thread-safe and can handle concurrent requests. It holds an `Arc` to the underlying
/// database instance, which handles its own concurrency control via MVCC and lock-free structures.
#[derive(Clone)]
pub struct AletheiaMcpServer {
    db: Arc<AletheiaDB>,
    /// Maximum operations accepted by one `apply_batch` call (Issue #3231).
    pub(crate) max_batch_operations: usize,
    auth: SessionAuth,
    /// Snapshot-anchored keyset continuation cursors (Issue #3360). Shared
    /// (one manager == one MCP connection) so its live-cursor cap is a
    /// per-connection cap.
    cursors: Arc<CursorManager>,
}

impl AletheiaMcpServer {
    /// Create a new MCP server wrapping a AletheiaDB instance.
    ///
    /// # Authentication
    ///
    /// This constructor is the **embedded/programmatic** entry point and is
    /// deliberately source-compatible with pre-#3350 behavior: it runs in
    /// anonymous mode (no authentication — a Rust caller holding the
    /// `Arc<AletheiaDB>` can already do anything the tools can). Serving
    /// deployments should use [`with_auth`](Self::with_auth); the
    /// `aletheia-mcp` binary requires authentication by default.
    ///
    /// # Arguments
    ///
    /// * `db` - An `Arc` wrapping the initialized database instance.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use aletheiadb::AletheiaDB;
    /// use aletheiadb::mcp::AletheiaMcpServer;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = Arc::new(AletheiaDB::new()?);
    /// let server = AletheiaMcpServer::new(db);
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        Self {
            db,
            max_batch_operations: DEFAULT_MAX_BATCH_OPERATIONS,
            auth: SessionAuth::Anonymous,
            cursors: Arc::new(CursorManager::new()),
        }
    }

    /// Override the cursor lifecycle bounds (Issue #3360): the continuation
    /// cursor TTL and the per-connection cap on concurrently live cursors.
    /// Defaults are a 5-minute TTL and 128 live cursors.
    #[must_use]
    pub fn with_cursor_config(mut self, ttl: std::time::Duration, max_live_cursors: usize) -> Self {
        self.cursors = Arc::new(CursorManager::with_config(ttl, max_live_cursors));
        self
    }

    /// Override the maximum number of operations accepted by a single
    /// `apply_batch` call (default: 1000, Issue #3231). An over-limit batch
    /// is rejected before any operation runs, with the limit echoed in the
    /// structured error's `details` (per the #3226 completeness convention).
    ///
    /// The cap is an MCP-surface payload bound; the core transaction buffer
    /// keeps its own independent DoS ceiling (50,000 ops).
    #[must_use]
    pub fn with_max_batch_operations(mut self, max_batch_operations: usize) -> Self {
        self.max_batch_operations = max_batch_operations;
        self
    }

    /// Create an MCP server with authentication and role-based
    /// authorization (Issue #3350, Phase 2).
    ///
    /// The MCP transport is stdio, so the credential is **session-scoped**:
    /// supplied once at construction (see
    /// [`McpAuthConfig::with_credential`]) and re-verified against the
    /// [`AuthStore`](crate::auth::AuthStore) on every tool call, so a
    /// revocation in the (possibly HTTP-shared) store takes effect on the
    /// next call.
    ///
    /// Behavior per [`AuthMode`]:
    ///
    /// - `Required` + valid credential → every tool call is authorized
    ///   against the principal's role (see the matrix in
    ///   `docs/guides/access-control-matrix.md`).
    /// - `Required` + missing/invalid/revoked credential → the server still
    ///   serves, but **every** tool call (including unknown tool names)
    ///   returns the uniform `UNAUTHENTICATED` error.
    /// - `Anonymous` → full access, exactly like [`new`](Self::new); a
    ///   prominent warning is emitted on stderr (never stdout — that is the
    ///   MCP protocol channel).
    pub fn with_auth(db: Arc<AletheiaDB>, auth: McpAuthConfig) -> Self {
        if auth.mode() == AuthMode::Anonymous {
            // PROMINENT warning: the operator explicitly opted out of auth.
            // stderr only — stdout carries the MCP protocol.
            eprintln!(
                "WARNING: AUTHENTICATION IS DISABLED (auth mode: anonymous). \
                 Every MCP tool call has full, unauthenticated access to the \
                 database. Do not expose this server to untrusted callers."
            );
        }
        Self {
            db,
            max_batch_operations: DEFAULT_MAX_BATCH_OPERATIONS,
            auth: SessionAuth::from(auth),
            cursors: Arc::new(CursorManager::new()),
        }
    }

    /// The verified session principal, if any.
    ///
    /// Re-verifies the session credential against the store, so a revoked
    /// key yields `None`. Always `None` in anonymous mode. Principal `id`
    /// and `name` are safe to log or stamp into provenance; key material
    /// never reaches this type.
    pub fn session_principal(&self) -> Option<Principal> {
        self.auth.principal()
    }

    /// Get a reference to the underlying database.
    ///
    /// Useful if you need to access the database directly for operations not exposed via MCP.
    pub fn db(&self) -> &Arc<AletheiaDB> {
        &self.db
    }

    // ========================================================================
    // Public Tool Methods (for testing and direct access)
    // ========================================================================

    /// Extract text content from a CallToolResult.
    ///
    /// Returns an error JSON string if the result contains no text content.
    pub(crate) fn extract_text(result: CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text().map(|s| s.text.clone()))
            .unwrap_or_else(|| {
                json!({
                    "error": McpError::new(McpErrorCode::Internal, "No content in response")
                        .to_json()
                })
                .to_string()
            })
    }

    /// Get a node by its ID.
    ///
    /// Returns the node's label and all properties in JSON format.
    ///
    /// # Output Format
    ///
    /// ```json
    /// {
    ///   "id": 123,
    ///   "label": "Person",
    ///   "properties": {
    ///     "name": "Alice",
    ///     "age": 30
    ///   }
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a JSON object with a structured "error" object if the node is
    /// not found (Issue #3234 error contract):
    /// ```json
    /// {
    ///   "error": {
    ///     "code": "NOT_FOUND",
    ///     "message": "Storage error: Node not found: Node(123)",
    ///     "retriable": false
    ///   }
    /// }
    /// ```
    pub fn get_node(&self, req: GetNodeRequest) -> String {
        Self::extract_text(self.handle_get_node(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Create a new node.
    ///
    /// Creates a node with the specified label and optional properties.
    /// Returns the created node's ID and details in JSON format.
    ///
    /// # Example Request
    ///
    /// ```rust
    /// use aletheiadb::mcp::CreateNodeRequest;
    /// use std::collections::HashMap;
    /// use serde_json::json;
    ///
    /// let mut props = HashMap::new();
    /// props.insert("name".to_string(), json!("Alice"));
    ///
    /// let req = CreateNodeRequest {
    ///     label: "Person".to_string(),
    ///     properties: Some(props),
    ///     valid_time: None,
    ///     provenance: None,
    ///     derived_from: None,
    /// };
    /// ```
    ///
    /// # Output Format
    ///
    /// Returns the complete node object including the newly assigned ID.
    ///
    /// ```json
    /// {
    ///   "id": 1,
    ///   "label": "Person",
    ///   "properties": { "name": "Alice" }
    /// }
    /// ```
    pub fn create_node(&self, req: CreateNodeRequest) -> String {
        Self::extract_text(self.handle_create_node(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Update a node's properties.
    ///
    /// Merges the provided properties with existing ones.
    /// - New keys are added.
    /// - Existing keys are updated.
    /// - Keys set to `null` are removed (future feature, currently sets to Null).
    ///
    /// # Output Format
    ///
    /// Returns the updated node object.
    pub fn update_node(&self, req: UpdateNodeRequest) -> String {
        Self::extract_text(self.handle_update_node(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Delete a node (safe-by-default).
    ///
    /// Permanently removes the node from the current state. Historical versions
    /// remain accessible via time-travel queries.
    ///
    /// Mirrors Cypher's `DETACH DELETE` contract (Issue #3209): if the node has
    /// connected edges and `detach` is not `true`, the deletion is **refused**
    /// and the JSON response reports `connected_edges` (the number of edges that
    /// would be orphaned) so the caller can decide. Pass `detach: true` to delete
    /// the node together with all connected edges; the response then reports
    /// `edges_removed`. A node with no connected edges always deletes cleanly.
    pub fn delete_node(&self, req: DeleteNodeRequest) -> String {
        Self::extract_text(self.handle_delete_node(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Retract a node as of a valid time (Issue #3230).
    ///
    /// Closes the node's valid-time interval at `valid_time` (default: now)
    /// WITHOUT deleting its history: `AS OF VALID_TIME` queries strictly
    /// before that instant still return the node, and `AS OF SYSTEM_TIME`
    /// queries positioned before the retraction still show it open-ended.
    ///
    /// Mirrors the `delete_node` DETACH contract (Issue #3209): if the node
    /// has connected edges and `detach` is not `true`, the retraction is
    /// **refused** and the JSON response reports `connected_edges`. Pass
    /// `detach: true` to co-retract every connected edge at the same valid
    /// time; the response then reports `edges_retracted`. Re-retracting an
    /// already-retracted node is an idempotent no-op returning the existing
    /// interval with `already_retracted: true`.
    pub fn retract_node(&self, req: RetractNodeRequest) -> String {
        Self::extract_text(self.handle_retract_node(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Retract an edge as of a valid time (Issue #3230).
    ///
    /// See [`retract_node`](Self::retract_node) for the bi-temporal
    /// semantics and idempotency; edges have no detach concern.
    pub fn retract_edge(&self, req: RetractEdgeRequest) -> String {
        Self::extract_text(self.handle_retract_edge(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Delete a node and all its connected edges (cascade delete).
    ///
    /// Removes the node and any edges connected to it, maintaining referential integrity.
    pub fn delete_node_cascade(&self, req: DeleteNodeCascadeRequest) -> String {
        Self::extract_text(self.handle_delete_node_cascade(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// List nodes with optional filtering.
    ///
    /// Supports filtering by label and pagination (limit/offset).
    /// Note: Listing all nodes without filters can be expensive on large graphs.
    pub fn list_nodes(&self, req: ListNodesRequest) -> String {
        Self::extract_text(self.handle_list_nodes(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Count nodes.
    ///
    /// Returns the total number of nodes in the graph, or nodes matching a specific label.
    pub fn count_nodes(&self, req: CountNodesRequest) -> String {
        Self::extract_text(self.handle_count_nodes(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get an edge by its ID.
    ///
    /// Returns the edge's source, target, label, and properties.
    pub fn get_edge(&self, req: GetEdgeRequest) -> String {
        Self::extract_text(self.handle_get_edge(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Create a new edge.
    ///
    /// Establishes a relationship between two existing nodes.
    /// Fails if either source or target node does not exist.
    pub fn create_edge(&self, req: CreateEdgeRequest) -> String {
        Self::extract_text(self.handle_create_edge(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Update an edge's properties.
    ///
    /// Merges properties similar to `update_node`.
    pub fn update_edge(&self, req: UpdateEdgeRequest) -> String {
        Self::extract_text(self.handle_update_edge(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Delete an edge.
    ///
    /// Removes the relationship between two nodes.
    pub fn delete_edge(&self, req: DeleteEdgeRequest) -> String {
        Self::extract_text(self.handle_delete_edge(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// List edges.
    ///
    /// Lists edges with pagination. Note that filtering edges by label without a start node is not currently efficient.
    pub fn list_edges(&self, req: ListEdgesRequest) -> String {
        Self::extract_text(self.handle_list_edges(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Count edges.
    ///
    /// Returns the total number of edges in the graph.
    pub fn count_edges(&self, req: CountEdgesRequest) -> String {
        Self::extract_text(self.handle_count_edges(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get outgoing edges from a node.
    ///
    /// Returns all edges starting from the specified node. Can be filtered by edge label.
    pub fn get_outgoing_edges(&self, req: GetOutgoingEdgesRequest) -> String {
        Self::extract_text(self.handle_get_outgoing_edges(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get incoming edges to a node.
    ///
    /// Returns all edges ending at the specified node. Can be filtered by edge label.
    pub fn get_incoming_edges(&self, req: GetIncomingEdgesRequest) -> String {
        Self::extract_text(self.handle_get_incoming_edges(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get the complete version history of a node.
    ///
    /// Returns all versions in chronological order (oldest first), including
    /// each version's write-time provenance bundle when present (Issue #3224).
    pub fn get_node_history(&self, req: GetNodeHistoryRequest) -> String {
        Self::extract_text(self.handle_get_node_history(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get the complete version history of an edge.
    ///
    /// Returns all versions in chronological order (oldest first), including
    /// each version's write-time provenance bundle when present (Issue #3224).
    pub fn get_edge_history(&self, req: GetEdgeHistoryRequest) -> String {
        Self::extract_text(self.handle_get_edge_history(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Traverse the graph.
    ///
    /// Performs a multi-hop traversal starting from a node, following edges of a specific type.
    /// Returns the path and the final nodes found.
    pub fn traverse(&self, req: TraverseRequest) -> String {
        Self::extract_text(self.handle_traverse(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Find similar nodes.
    ///
    /// Performs a K-Nearest Neighbors (k-NN) search using vector embeddings.
    ///
    /// # Prerequisites
    ///
    /// A vector index must be enabled on the target property using `enable_vector_index`
    /// before this method can be used.
    ///
    /// # Output Format
    ///
    /// Returns a list of matches with their similarity scores.
    ///
    /// ```json
    /// {
    ///   "results": [
    ///     {
    ///       "node": {
    ///         "id": 123,
    ///         "label": "Document",
    ///         "properties": { "title": "..." }
    ///       },
    ///       "score": 0.95
    ///     }
    ///   ],
    ///   "count": 1
    /// }
    /// ```
    pub fn find_similar(&self, req: FindSimilarRequest) -> String {
        Self::extract_text(self.handle_find_similar(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Enable vector index.
    ///
    /// Configures and builds an HNSW index on a specific property, enabling semantic search.
    /// This is a prerequisite for `find_similar`.
    ///
    /// # Arguments
    ///
    /// * `property_name`: The property to index (e.g., "embedding").
    /// * `dimensions`: The size of the vector (e.g., 1536 for OpenAI).
    /// * `distance_metric`: "cosine" (default), "euclidean", or "dot".
    ///
    /// # Output Format
    ///
    /// Returns a success confirmation.
    ///
    /// ```json
    /// {
    ///   "success": true,
    ///   "property_name": "embedding",
    ///   "dimensions": 1536,
    ///   "distance_metric": "cosine"
    /// }
    /// ```
    pub fn enable_vector_index(&self, req: EnableVectorIndexRequest) -> String {
        Self::extract_text(self.handle_enable_vector_index(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// List vector indexes.
    ///
    /// Returns a list of all active vector indexes and their configuration (dimensions, metric).
    ///
    /// # Output Format
    ///
    /// ```json
    /// {
    ///   "indexes": [
    ///     {
    ///       "property_name": "embedding",
    ///       "dimensions": 1536,
    ///       "distance_metric": "Cosine"
    ///     }
    ///   ],
    ///   "count": 1
    /// }
    /// ```
    pub fn list_vector_indexes(&self, req: ListVectorIndexesRequest) -> String {
        Self::extract_text(self.handle_list_vector_indexes(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Enable a uniqueness constraint on a label+property pair.
    pub fn enable_unique_constraint(&self, req: EnableUniqueConstraintRequest) -> String {
        Self::extract_text(self.handle_enable_unique_constraint(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// List all active uniqueness constraints.
    pub fn list_unique_constraints(&self, req: ListUniqueConstraintsRequest) -> String {
        Self::extract_text(self.handle_list_unique_constraints(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get node at a specific time.
    ///
    /// Performs a time-travel query to retrieve the state of a node at a specific valid time and transaction time.
    pub fn get_node_at_time(&self, req: GetNodeAtTimeRequest) -> String {
        Self::extract_text(self.handle_get_node_at_time(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get edge at a specific time.
    ///
    /// Performs a time-travel query to retrieve the state of an edge at a specific valid time and transaction time.
    pub fn get_edge_at_time(&self, req: GetEdgeAtTimeRequest) -> String {
        Self::extract_text(self.handle_get_edge_at_time(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Find nodes by label (and optional exact property match) as of a
    /// bi-temporal point (Issue #3236).
    ///
    /// The entry-point resolver for "the Person named Alice, as of
    /// 2024-01-01" when no `NodeId` is known: each returned node is
    /// reconstructed as it existed at `(valid_time, transaction_time)`.
    pub fn find_nodes_at_time(&self, req: FindNodesAtTimeRequest) -> String {
        Self::extract_text(self.handle_find_nodes_at_time(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Discover the graph's schema: distinct node labels and edge types,
    /// their counts, and the property keys observed on each.
    ///
    /// With no `as_of_*` fields, returns the current-state schema. With
    /// either field set, returns the schema as it existed at that bi-temporal
    /// instant. Never errors on an empty database.
    pub fn get_schema(&self, req: GetSchemaRequest) -> String {
        Self::extract_text(self.handle_get_schema(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Report the dataset's queryable bi-temporal extent: earliest/latest
    /// valid-time and transaction-time bounds across all recorded history
    /// (including expired/superseded versions), as RFC3339 strings.
    ///
    /// Pass `by_label: true` for a per-node-label / per-edge-type breakdown.
    /// An empty database returns explicit `null` bounds, never epoch 0.
    pub fn temporal_extent(&self, req: TemporalExtentRequest) -> String {
        Self::extract_text(self.handle_temporal_extent(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Query the upstream derivation lineage of a fact (Issue #3371): "what
    /// was this fact derived from?", transitively — the evidence chain.
    pub fn lineage_upstream(&self, req: crate::mcp::tools::LineageQueryRequest) -> String {
        Self::extract_text(self.handle_lineage_upstream(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Query the downstream derivation lineage of a fact (Issue #3371): "what
    /// has been derived from this fact?", transitively — the blast radius.
    pub fn lineage_downstream(&self, req: crate::mcp::tools::LineageQueryRequest) -> String {
        Self::extract_text(self.handle_lineage_downstream(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Get a holistic database statistics snapshot.
    ///
    /// Returns current graph size, bi-temporal depth (version counts and
    /// anchor/delta compression), storage-tier presence/distribution
    /// (hot / warm-cache / cold), and WAL durability state in a single call.
    /// Thin aggregator over [`AletheiaDB::stats`] — all values are O(1)/cached
    /// counter reads, never a version scan.
    pub fn database_stats(&self, req: DatabaseStatsRequest) -> String {
        Self::extract_text(self.handle_database_stats(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Test-only access to the raw `database_stats` handler, bypassing typed
    /// request construction so tests can exercise wire-level argument edge
    /// cases (null arguments, non-object arguments, unknown keys) exactly as
    /// `call_tool` delivers them.
    #[cfg(test)]
    pub(crate) fn database_stats_raw(&self, args: serde_json::Value) -> String {
        Self::extract_text(self.handle_database_stats(args))
    }

    /// List graph-wide changes within a transaction-time window.
    ///
    /// Enumerates the nodes and edges whose versions were committed in `[tx_from, tx_to)`,
    /// with optional valid-time and label filtering and stable cursor pagination. This is the
    /// discovery primitive an agent reaches for to answer "what changed since `<time>`?"
    /// without already knowing any entity IDs.
    pub fn list_changes(&self, req: ListChangesRequest) -> String {
        Self::extract_text(self.handle_list_changes(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Execute a hybrid query.
    ///
    /// Combines **graph traversal**, **vector similarity**, and **temporal filtering** into a single query.
    /// This is the most powerful tool for "reasoning" about data.
    ///
    /// # Capabilities
    ///
    /// - **Start**: From a specific node (`start_node_id`) OR by vector search (`query_embedding`).
    /// - **Traverse**: Follow edges (`traverse_edge`) up to `traverse_depth`.
    /// - **Filter**: By time (`valid_time`) or label (`filter_label`).
    /// - **Rank**: Re-rank results by vector similarity (`query_embedding`).
    ///
    /// # Example Scenarios
    ///
    /// 1. **"Find similar documents written by Alice"**
    ///    - Start at Alice (`start_node_id`)
    ///    - Traverse `WROTE` edges
    ///    - Rank by similarity to `query_embedding`
    ///
    /// 2. **"Find papers about AI published last year"**
    ///    - Vector search (`query_embedding`)
    ///    - Filter by `valid_time`
    ///
    /// # Output Format
    ///
    /// Returns a list of hybrid results containing the node, optional score, path, and timestamp.
    ///
    /// ```json
    /// {
    ///   "results": [
    ///     {
    ///       "node": { "id": 10, "label": "Document", "properties": {...} },
    ///       "similarity_score": 0.92,
    ///       "traversal_path": [1, 5, 10],
    ///       "timestamp": "2023-01-01T12:00:00Z"
    ///     }
    ///   ],
    ///   "count": 1
    /// }
    /// ```
    pub fn hybrid_query(&self, req: HybridQueryRequest) -> String {
        Self::extract_text(self.handle_hybrid_query(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Execute a read-only declarative query (Cypher or AQL).
    ///
    /// Returns the engine's structured rows plus column metadata as JSON, or a
    /// structured `{error:{kind,code,retriable,message,clause?,language}}`
    /// payload (`kind` per the query tool's own contract, `code`/`retriable`
    /// per the uniform Issue #3234 error contract). Mutating statements are
    /// rejected before execution and never write.
    pub fn query(&self, req: QueryRequest) -> String {
        Self::extract_text(self.handle_query(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    // ========================================================================
    // Helper methods for converting between AletheiaDB and MCP types
    // ========================================================================

    fn interned_to_string(&self, interned: crate::core::InternedString) -> String {
        GLOBAL_INTERNER.resolve_or_else(interned, || format!("<unknown:{}>", interned.as_u32()))
    }

    /// Look up the provenance bundle and temporal bounds for the *exact*
    /// version already captured in `version_id` (a `Node`/`Edge` snapshot's
    /// `current_version`), rather than re-resolving "whichever version is
    /// current now". This keeps the returned metadata consistent with the
    /// properties already read from that same snapshot, even under a
    /// concurrent write -- and makes the bounds correct for point-in-time
    /// reads, which set `current_version` to the matched historical version.
    ///
    /// A lookup failure (e.g. a corrupted or unreachable cold-storage record)
    /// is logged rather than silently treated as "no metadata": that
    /// distinction matters because an MCP caller has no other way to tell
    /// the two cases apart. Best-effort here (returning `None`s rather than
    /// failing the whole response) is deliberate: a single-node lookup or a
    /// bulk endpoint like `list_nodes`/`traverse` should not fail entirely
    /// because one entry's metadata couldn't be read.
    ///
    /// `now` is the request-scoped wallclock captured once per tool call
    /// (Issue #3391), so every entity in one response evaluates `is_current`
    /// against the same instant.
    fn lookup_node_read_metadata(
        &self,
        version_id: VersionId,
        now: Timestamp,
    ) -> (Option<Provenance>, Option<TemporalBounds>) {
        match self.db.get_node_version_read_metadata(version_id) {
            Ok(Some((provenance, interval))) => (
                provenance,
                Some(TemporalBounds::from_interval_at(&interval, now)),
            ),
            Ok(None) => {
                eprintln!(
                    "Warning: node version {} not found in any tier; omitting provenance/temporal",
                    version_id.as_u64()
                );
                (None, None)
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to load metadata for node version {}: {}",
                    version_id.as_u64(),
                    e
                );
                (None, None)
            }
        }
    }

    /// Edge counterpart of [`lookup_node_read_metadata`](Self::lookup_node_read_metadata).
    fn lookup_edge_read_metadata(
        &self,
        version_id: VersionId,
        now: Timestamp,
    ) -> (Option<Provenance>, Option<TemporalBounds>) {
        match self.db.get_edge_version_read_metadata(version_id) {
            Ok(Some((provenance, interval))) => (
                provenance,
                Some(TemporalBounds::from_interval_at(&interval, now)),
            ),
            Ok(None) => {
                eprintln!(
                    "Warning: edge version {} not found in any tier; omitting provenance/temporal",
                    version_id.as_u64()
                );
                (None, None)
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to load metadata for edge version {}: {}",
                    version_id.as_u64(),
                    e
                );
                (None, None)
            }
        }
    }

    fn node_to_response(
        &self,
        node: &crate::core::Node,
        include_vectors: bool,
        now: Timestamp,
    ) -> NodeResponse {
        let (provenance, temporal) = self.lookup_node_read_metadata(node.current_version, now);
        NodeResponse {
            id: node.id.as_u64(),
            label: self.interned_to_string(node.label),
            properties: self.property_map_to_json(&node.properties, include_vectors),
            provenance,
            temporal,
        }
    }

    fn edge_to_response(
        &self,
        edge: &crate::core::Edge,
        include_vectors: bool,
        now: Timestamp,
    ) -> EdgeResponse {
        let (provenance, temporal) = self.lookup_edge_read_metadata(edge.current_version, now);
        EdgeResponse {
            id: edge.id.as_u64(),
            source_id: edge.source.as_u64(),
            target_id: edge.target.as_u64(),
            label: self.interned_to_string(edge.label),
            properties: self.property_map_to_json(&edge.properties, include_vectors),
            provenance,
            temporal,
        }
    }

    fn property_map_to_json(
        &self,
        props: &PropertyMap,
        include_vectors: bool,
    ) -> HashMap<String, serde_json::Value> {
        let mut result = HashMap::new();
        for (key, value) in props.iter() {
            let key_str = self.interned_to_string(*key);
            result.insert(key_str, self.property_value_to_json(value, include_vectors));
        }
        result
    }

    fn property_value_to_json(
        &self,
        value: &PropertyValue,
        include_vectors: bool,
    ) -> serde_json::Value {
        match value {
            PropertyValue::Null => serde_json::Value::Null,
            PropertyValue::Bool(b) => serde_json::Value::Bool(*b),
            PropertyValue::Int(i) => json!(*i),
            PropertyValue::Float(f) => json!(*f),
            PropertyValue::String(s) => serde_json::Value::String(s.to_string()),
            PropertyValue::Bytes(b) => {
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(b))
            }
            PropertyValue::Array(arr) => serde_json::Value::Array(
                arr.iter()
                    .map(|v| self.property_value_to_json(v, include_vectors))
                    .collect(),
            ),
            PropertyValue::Vector(v) => {
                if include_vectors {
                    serde_json::Value::Array(
                        v.iter()
                            .map(|&f| serde_json::Value::from(f as f64))
                            .collect(),
                    )
                } else {
                    json!({"type": "vector", "dim": v.len(), "elided": true})
                }
            }
            PropertyValue::SparseVector(sv) => {
                if include_vectors {
                    json!({
                        "indices": sv.indices(),
                        "values": sv.values()
                    })
                } else {
                    json!({
                        "type": "sparse_vector",
                        "dim": sv.dimension(),
                        "nnz": sv.nnz(),
                        "elided": true
                    })
                }
            }
        }
    }

    pub(crate) fn json_to_property_map(
        &self,
        json: &HashMap<String, serde_json::Value>,
    ) -> Result<PropertyMap, String> {
        let mut builder = PropertyMapBuilder::new();
        for (key, value) in json {
            if let Some(pv) = self.json_to_property_value(value) {
                builder = builder
                    .try_insert(key.as_str(), pv)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(builder.build())
    }

    fn json_to_property_value(&self, value: &serde_json::Value) -> Option<PropertyValue> {
        match value {
            serde_json::Value::Null => Some(PropertyValue::Null),
            serde_json::Value::Bool(b) => Some(PropertyValue::Bool(*b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Some(PropertyValue::Int(i))
                } else {
                    n.as_f64().map(PropertyValue::Float)
                }
            }
            serde_json::Value::String(s) => Some(PropertyValue::String(Arc::from(s.as_str()))),
            serde_json::Value::Array(arr) => {
                if arr.iter().all(|v| v.is_number()) && !arr.is_empty() {
                    let floats: Vec<f32> = arr
                        .iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect();
                    if floats.len() == arr.len() {
                        return Some(PropertyValue::Vector(Arc::from(floats)));
                    }
                }
                let values: Vec<PropertyValue> = arr
                    .iter()
                    .filter_map(|v| self.json_to_property_value(v))
                    .collect();
                Some(PropertyValue::Array(Arc::new(values)))
            }
            serde_json::Value::Object(_) => None,
        }
    }

    pub(crate) fn parse_timestamp(&self, s: &str) -> Result<Timestamp, String> {
        // Try parsing as ISO 8601 timestamp first
        if let Ok(dt) = s.parse::<DateTime<Utc>>() {
            let micros = dt.timestamp_micros();
            return Ok(Timestamp::from(micros));
        }

        // Also try parsing ISO 8601 without timezone (assume UTC)
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
            let micros = dt.and_utc().timestamp_micros();
            return Ok(Timestamp::from(micros));
        }

        // Fall back to microseconds since epoch
        if let Ok(micros) = s.parse::<i64>() {
            return Ok(Timestamp::from(micros));
        }

        Err(format!(
            "Invalid timestamp format: '{}'. Expected ISO 8601 (e.g., '2024-01-15T10:00:00Z') or microseconds since epoch.",
            s
        ))
    }

    /// Parse an optional timestamp argument, returning a structured INVALID_ARGUMENT error result on a parse failure.
    ///
    /// Collapses the otherwise-duplicated "if present, parse, else None" handling for the
    /// changefeed's optional time bounds.
    fn parse_opt_timestamp(
        &self,
        label: &str,
        value: &Option<String>,
    ) -> std::result::Result<Option<Timestamp>, CallToolResult> {
        value
            .as_deref()
            .map(|s| self.parse_timestamp(s))
            .transpose()
            .map_err(|e| self.invalid_argument(&format!("Invalid {label}: {e}")))
    }

    /// Resolve a pair of independently-optional `as_of_valid_time` /
    /// `as_of_transaction_time` request fields into a single bi-temporal
    /// coordinate, shared by every MCP tool that supports point-in-time
    /// queries (`get_schema`, `traverse`, ...).
    ///
    /// Returns `None` when both fields are absent (current-state / no
    /// temporal filtering). When either field is supplied, the other
    /// defaults to the current time -- e.g. `as_of_valid_time` alone answers
    /// "using everything recorded so far, what was valid at this instant"
    /// (transaction_time defaults to now), matching the Rust API's
    /// `get_node_at_valid_time`/`get_node_at_transaction_time` convenience
    /// methods, which default the unspecified dimension the same way.
    fn resolve_bitemporal_as_of(
        &self,
        as_of_valid_time: &Option<String>,
        as_of_transaction_time: &Option<String>,
    ) -> std::result::Result<Option<(Timestamp, Timestamp)>, CallToolResult> {
        let valid_time = self.parse_opt_timestamp("as_of_valid_time", as_of_valid_time)?;
        let transaction_time =
            self.parse_opt_timestamp("as_of_transaction_time", as_of_transaction_time)?;
        Ok(match (valid_time, transaction_time) {
            (None, None) => None,
            (vt, tt) => Some((vt.unwrap_or_else(time::now), tt.unwrap_or_else(time::now))),
        })
    }

    /// Validate and convert an optional MCP [`ProvenanceRequest`] into a
    /// core [`Provenance`](crate::core::provenance::Provenance), stamping
    /// the authenticated session principal (Issue #3350).
    ///
    /// Mirrors [`parse_opt_timestamp`](Self::parse_opt_timestamp): returns
    /// `Err(invalid_argument(...))` with a clear message when `confidence` is out
    /// of `[0.0, 1.0]` (Issue #3224), rather than a generic deserialization
    /// error. An entirely empty bundle (all fields omitted) is normalized to
    /// `None` -- never persisted as a fabricated empty object.
    ///
    /// **Principal stamping**: when the session has a verified principal
    /// (see [`session_principal`](Self::session_principal)), its *name* is
    /// recorded as the bundle's `principal` field -- composing with (never
    /// replacing) whatever `source`/`confidence`/`note`/`correlation_id`
    /// the caller supplied. A write with no caller-supplied provenance
    /// still records a principal-only bundle. The principal is
    /// server-stamped from the verified credential; [`ProvenanceRequest`]
    /// deliberately has no `principal` field, so callers cannot forge it.
    /// Anonymous-mode sessions (and the embedded `new()` constructor)
    /// record no principal -- the field is absent, not an empty string.
    pub(crate) fn parse_opt_provenance(
        &self,
        value: Option<crate::mcp::tools::ProvenanceRequest>,
    ) -> std::result::Result<Option<Provenance>, CallToolResult> {
        let principal = self.session_principal().map(|p| p.name);
        let supplied = match value {
            Some(req) => {
                let provenance = Provenance::from_parts(
                    req.source,
                    req.confidence,
                    req.note,
                    req.correlation_id,
                    None,
                )
                .map_err(|e| {
                    self.invalid_argument(&format!(
                        "Invalid provenance: confidence must be between 0.0 and 1.0 ({e})"
                    ))
                })?;
                // Normalize an all-empty caller bundle away *before*
                // stamping, so "caller sent {}" and "caller sent nothing"
                // behave identically.
                if provenance.is_empty() {
                    None
                } else {
                    Some(provenance)
                }
            }
            None => None,
        };
        Ok(match (supplied, principal) {
            (Some(p), Some(name)) => Some(p.with_principal(name)),
            (Some(p), None) => Some(p),
            // A principal-only bundle cannot fail validation (only
            // `confidence` is validated, and it is unset here); `.ok()`
            // keeps this non-panicking regardless.
            (None, Some(name)) => Provenance::builder().principal(name).build().ok(),
            (None, None) => None,
        })
    }

    /// Parse a single MCP [`LineageRefRequest`] into a core
    /// [`LineageRef`](crate::core::lineage::LineageRef) (Issue #3371).
    ///
    /// Validates `entity_kind` (`"node"`/`"edge"`, case-insensitive) and that
    /// the id and version are in range. Structural validity only — whether the
    /// version actually *exists* is checked by the write path
    /// (`validate_sources`) so a dangling reference becomes a `NOT_FOUND`
    /// rather than an `INVALID_ARGUMENT`.
    fn parse_lineage_ref(
        &self,
        req: &crate::mcp::tools::LineageRefRequest,
    ) -> std::result::Result<crate::core::lineage::LineageRef, CallToolResult> {
        let version = VersionId::new(req.version)
            .map_err(|e| self.invalid_argument(&format!("Invalid derived_from version: {e}")))?;
        let entity = match req.entity_kind.trim().to_ascii_lowercase().as_str() {
            "node" => crate::core::id::EntityId::Node(NodeId::new(req.id).map_err(|e| {
                self.invalid_argument(&format!("Invalid derived_from node id: {e}"))
            })?),
            "edge" => crate::core::id::EntityId::Edge(EdgeId::new(req.id).map_err(|e| {
                self.invalid_argument(&format!("Invalid derived_from edge id: {e}"))
            })?),
            other => {
                return Err(self.invalid_argument(&format!(
                    "Invalid derived_from entity_kind '{other}': expected 'node' or 'edge'"
                )));
            }
        };
        Ok(crate::core::lineage::LineageRef { entity, version })
    }

    /// Parse the optional `derived_from` list on a write request into core
    /// [`LineageRef`](crate::core::lineage::LineageRef)s (Issue #3371). `None`
    /// or an empty list yields an empty vec (no lineage recorded).
    fn parse_derived_from(
        &self,
        value: &Option<Vec<crate::mcp::tools::LineageRefRequest>>,
    ) -> std::result::Result<Vec<crate::core::lineage::LineageRef>, CallToolResult> {
        match value {
            None => Ok(Vec::new()),
            Some(list) => list.iter().map(|r| self.parse_lineage_ref(r)).collect(),
        }
    }

    /// Parse an optional transaction time, returning the current time if not specified.
    fn parse_optional_tx_time(&self, tx_time: Option<&str>) -> Result<Timestamp, String> {
        match tx_time {
            Some(tx) => self.parse_timestamp(tx),
            None => Ok(time::now()),
        }
    }

    /// Format transaction time for response, using the constant for current time.
    fn format_tx_time_response(tx_time: Option<String>) -> String {
        tx_time.unwrap_or_else(|| TRANSACTION_TIME_NOW.to_string())
    }

    /// Serialize a timestamp as an RFC 3339 `Z`-suffixed microsecond-precision
    /// JSON string — the interval-bound convention used by retraction
    /// responses (half-open `[start, end)`; `null` would denote an
    /// open-ended bound). Delegates to
    /// [`format_timestamp_rfc3339`](Self::format_timestamp_rfc3339) so all
    /// MCP tools share one formatter.
    fn timestamp_to_rfc3339_micros(ts: Timestamp) -> serde_json::Value {
        json!(Self::format_timestamp_rfc3339(ts))
    }

    /// Format a resolved bi-temporal coordinate as an RFC 3339 UTC string
    /// with microsecond precision and a `Z` suffix (e.g.
    /// `2024-01-15T10:00:00.000000Z`).
    ///
    /// Total: a wallclock value outside chrono's representable range degrades
    /// to the raw microsecond count rather than silently rendering the 1970
    /// epoch.
    fn format_timestamp_rfc3339(ts: Timestamp) -> String {
        match DateTime::<Utc>::from_timestamp_micros(ts.wallclock()) {
            Some(dt) => dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            None => ts.wallclock().to_string(),
        }
    }

    fn matches_label(&self, interned: crate::core::InternedString, label: &str) -> bool {
        GLOBAL_INTERNER
            .resolve_with(interned, |s| s == label)
            .unwrap_or(false)
    }

    /// Get the expected dimensions for a vector index property.
    /// Returns None if the index doesn't exist.
    fn get_vector_index_dimensions(&self, property_name: &str) -> Option<usize> {
        self.db
            .list_vector_indexes()
            .into_iter()
            .find(|info| info.property_name == property_name)
            .map(|info| info.dimensions)
    }

    /// Validate embedding dimensions against the expected dimensions for an index.
    fn validate_embedding_dimensions(
        &self,
        embedding: &[f32],
        property_name: &str,
    ) -> Result<(), String> {
        if let Some(expected_dims) = self.get_vector_index_dimensions(property_name)
            && embedding.len() != expected_dims
        {
            return Err(format!(
                "Embedding dimension mismatch: expected {} dimensions for property '{}', got {}",
                expected_dims,
                property_name,
                embedding.len()
            ));
        }
        Ok(())
    }

    pub(crate) fn success_json(&self, value: serde_json::Value) -> CallToolResult {
        CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        )])
    }

    /// Serialize a structured [`McpError`] into an error `CallToolResult`
    /// with the Issue #3234 shape:
    /// `{"error": {"code", "message", "retriable", "details"?}}`.
    pub(crate) fn error_result(&self, err: McpError) -> CallToolResult {
        Self::error_result_with_top_level(err, serde_json::Map::new())
    }

    /// Like [`error_result`](Self::error_result), but preserving additional
    /// legacy top-level fields alongside the structured `error` object (e.g.
    /// the #3209 DETACH refusal's `connected_edges`), so pre-#3234 consumers
    /// lose nothing.
    pub(crate) fn error_result_with_top_level(
        err: McpError,
        mut top_level: serde_json::Map<String, serde_json::Value>,
    ) -> CallToolResult {
        top_level.insert("error".to_string(), err.to_json());
        let value = serde_json::Value::Object(top_level);
        // Compact serialization, matching both the pre-#3234 error payloads
        // and the query tool's error path (success payloads stay pretty).
        CallToolResult::error(vec![Content::text(
            serde_json::to_string(&value).unwrap_or_else(|_| value.to_string()),
        )])
    }

    /// Caller-fault error: malformed arguments, bad IDs, unparseable
    /// timestamps, inconsistent parameter combinations. Never retriable.
    pub(crate) fn invalid_argument(&self, msg: &str) -> CallToolResult {
        self.error_result(McpError::new(McpErrorCode::InvalidArgument, msg))
    }

    /// The request is well-formed but the system is not in the required
    /// state (e.g. a vector index is not enabled). Never retriable as-is.
    fn failed_precondition(&self, msg: &str) -> CallToolResult {
        self.error_result(McpError::new(McpErrorCode::FailedPrecondition, msg))
    }

    /// Classify an internal database error into the structured MCP shape.
    ///
    /// A `UniqueViolation` additionally carries its constraint metadata under
    /// `error.details` and (for backward compatibility) as legacy top-level
    /// fields, exactly as before #3234.
    fn db_error(&self, e: impl Into<crate::core::error::Error>) -> CallToolResult {
        let e = e.into();
        if let Some(crate::core::error::ConstraintError::UniqueViolation {
            label,
            property,
            value,
            existing_node_id,
        }) = e.as_constraint()
        {
            let details = json!({
                "label": label,
                "property": property,
                "value": value,
                "existing_node_id": existing_node_id.as_u64()
            });
            let mut top_level = serde_json::Map::new();
            top_level.insert("success".to_string(), json!(false));
            top_level.insert("constraint_violation".to_string(), json!(true));
            top_level.insert("label".to_string(), json!(label));
            top_level.insert("property".to_string(), json!(property));
            top_level.insert("value".to_string(), json!(value));
            top_level.insert(
                "existing_node_id".to_string(),
                json!(existing_node_id.as_u64()),
            );
            return Self::error_result_with_top_level(
                McpError::from_db_error(&e).details(details),
                top_level,
            );
        }
        self.error_result(McpError::from_db_error(&e))
    }

    /// Attach result-completeness signals (Issue #3226) to a bounded read
    /// tool's response object so a single MCP call reveals whether the returned
    /// page is the whole truth.
    ///
    /// - `has_more` is always set: `true` when at least one matching result
    ///   exists beyond the returned page, `false` otherwise.
    /// - `next_offset` (the value to pass as `offset` for the next page) is set
    ///   only when `has_more` is `true`; it is omitted otherwise.
    /// - `total_matching` is set only when a matching total is cheaply known;
    ///   it is omitted (never faked) when computing it would require an
    ///   expensive full scan.
    ///
    /// `consumed` is the number of underlying candidates the page advanced
    /// past -- i.e. the requested page window (e.g. `limit` or `k`), NOT the
    /// number of items actually returned in the body. These differ when an
    /// id resolved from an index (property lookup, vector search) turns out
    /// to be a since-deleted node and is dropped: the page still consumed a
    /// full window of candidates, so `next_offset` must advance by the window
    /// size, not the smaller returned count, or the next call would re-skip
    /// into already-consumed candidates and duplicate a row across pages.
    fn attach_completeness(
        value: &mut serde_json::Value,
        offset: usize,
        consumed: usize,
        has_more: bool,
        total_matching: Option<usize>,
    ) {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("has_more".to_string(), json!(has_more));
            if has_more {
                obj.insert(
                    "next_offset".to_string(),
                    json!(offset.saturating_add(consumed)),
                );
            }
            if let Some(total) = total_matching {
                obj.insert("total_matching".to_string(), json!(total));
            }
        }
    }

    // ========================================================================
    // Snapshot-anchored cursor continuation (Issue #3360)
    // ========================================================================

    /// Whether the raw arguments opt into cursor paging -- either a `cursor`
    /// continuation token or `use_cursor: true` on the first page. The cursor
    /// parameters are additive and read directly off the arguments so the
    /// per-tool request structs (and their many struct-literal call sites)
    /// stay unchanged.
    fn cursor_requested(args: &serde_json::Value) -> bool {
        args.get("cursor").and_then(|v| v.as_str()).is_some()
            || args.get("use_cursor").and_then(|v| v.as_bool()) == Some(true)
    }

    /// Read an optional non-empty string argument.
    fn arg_str(args: &serde_json::Value, key: &str) -> Option<String> {
        args.get(key).and_then(|v| v.as_str()).map(str::to_string)
    }

    /// Read an optional non-null JSON argument (e.g. a `property_value` that
    /// may be a string, number, or bool).
    fn arg_value(args: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
        match args.get(key) {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(v.clone()),
        }
    }

    /// Read and clamp the per-page `limit`, matching the bounded read tools'
    /// convention (default 100, at least 1, capped at `MAX_RESULT_LIMIT`).
    fn arg_limit(args: &serde_json::Value) -> usize {
        args.get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT)
    }

    /// Emit one snapshot-anchored keyset page over a set of node candidates
    /// (shared by `list_nodes` and `find_nodes_at_time` in cursor mode).
    ///
    /// `candidates` MUST be sorted ascending by node id (both underlying `db`
    /// finders guarantee this) and reconstructed at `snapshot`. The page
    /// returns the first `limit` candidates whose id is strictly greater than
    /// `after` -- a keyset filter that avoids re-emitting prior result pages (no
    /// dup/gap). Candidate enumeration is still O(total) per page in v1 (the
    /// full candidate scan re-runs each page); a true depth-independent keyset
    /// seek is a follow-up. When more candidates remain, a continuation `cursor` is
    /// issued carrying the same pinned `snapshot`, so the union of all pages
    /// equals exactly the unbounded result at that one bi-temporal moment.
    ///
    /// `parent_cid` is the cursor id of the page being resumed (empty for the
    /// first page): passing it through keeps the whole scan on one registry
    /// slot instead of consuming the live-cursor cap once per page.
    #[allow(clippy::too_many_arguments)]
    fn emit_node_cursor_page(
        &self,
        tool: &str,
        snapshot: (Timestamp, Timestamp),
        after: Option<u64>,
        limit: usize,
        include_vectors: bool,
        filters: serde_json::Value,
        candidates: crate::db::ops::NodesAtTime,
        parent_cid: String,
        now: Timestamp,
    ) -> CallToolResult {
        let sampled = candidates.sampled;
        let nodes = candidates.nodes;
        let total_matching = nodes.len();

        // Keyset seek: everything strictly past the last id returned so far
        // (`after == None` on the first page returns from the beginning).
        let past = |id: u64| after.is_none_or(|a| id > a);
        let remaining = nodes.iter().filter(|n| past(n.id.as_u64())).count();
        let page: Vec<&crate::core::Node> = nodes
            .iter()
            .filter(|n| past(n.id.as_u64()))
            .take(limit)
            .collect();
        let has_more = remaining > page.len();
        let last_id = page.last().map(|n| n.id.as_u64());

        let responses: Vec<NodeResponse> = page
            .iter()
            .map(|n| self.node_to_response(n, include_vectors, now))
            .collect();

        let mut obj = serde_json::Map::new();
        obj.insert(
            "nodes".to_string(),
            serde_json::to_value(&responses).unwrap_or_else(|_| json!([])),
        );
        obj.insert("count".to_string(), json!(responses.len()));
        obj.insert("total_matching".to_string(), json!(total_matching));
        obj.insert("sampled".to_string(), json!(sampled));
        obj.insert("has_more".to_string(), json!(has_more));
        // Disclose the pinned snapshot so the caller can see exactly which
        // bi-temporal moment the whole scan is answering at.
        obj.insert(
            "snapshot_valid_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.0)),
        );
        obj.insert(
            "snapshot_transaction_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.1)),
        );
        obj.insert("paging".to_string(), json!("cursor"));

        if has_more {
            // `last_id` is Some whenever the page is non-empty; a full page
            // with more remaining always has a last id.
            if let Some(last_id) = last_id {
                let mut payload = CursorPayload::seed(
                    tool,
                    (snapshot.0.wallclock(), snapshot.1.wallclock()),
                    limit,
                    filters,
                );
                // Continuation key = last id ACTUALLY EMITTED on this page.
                // When #3353 token budgets land and can trim a page short of
                // `limit`, `after` MUST still be derived from the last row that
                // survived the trim (not the limit-th candidate), or the next
                // page would skip the trimmed-off rows.
                payload.after = Some(last_id);
                payload.cid = parent_cid;
                match self.cursors.issue(payload) {
                    Ok(token) => {
                        obj.insert("cursor".to_string(), json!(token));
                        obj.insert(
                            "cursor_ttl_seconds".to_string(),
                            json!(self.cursors.ttl().as_secs()),
                        );
                    }
                    Err(e) => return self.error_result(e),
                }
            }
        }

        self.success_json(serde_json::Value::Object(obj))
    }

    /// Fetch the node candidate set for a cursored scan at `snapshot`,
    /// applying the same optional exact-property filter `list_nodes` /
    /// `find_nodes_at_time` support. Returns a structured error result on a
    /// bad property value.
    fn fetch_node_candidates(
        &self,
        label: &str,
        property_key: &Option<String>,
        property_value: &Option<serde_json::Value>,
        snapshot: (Timestamp, Timestamp),
    ) -> Result<crate::db::ops::NodesAtTime, CallToolResult> {
        let (vt, tt) = snapshot;
        match (property_key, property_value) {
            (Some(key), Some(val)) => {
                let pv = match self.json_to_property_value(val) {
                    Some(v) => v,
                    None => {
                        return Err(self.invalid_argument(
                            "Unsupported property_value type. Use strings, numbers, booleans, or null.",
                        ));
                    }
                };
                self.db
                    .find_nodes_by_property_at(label, key, &pv, vt, tt)
                    .map_err(|e| self.db_error(e))
            }
            _ => self
                .db
                .find_nodes_at_time(label, vt, tt)
                .map_err(|e| self.db_error(e)),
        }
    }

    /// Build the opaque `filters` blob embedded in a node-scan cursor so a
    /// continuation reconstructs the exact same query with no extra params.
    fn node_scan_filters(
        label: &str,
        property_key: &Option<String>,
        property_value: &Option<serde_json::Value>,
        include_vectors: bool,
    ) -> serde_json::Value {
        json!({
            "label": label,
            "property_key": property_key,
            "property_value": property_value,
            "include_vectors": include_vectors,
        })
    }

    /// Emit one snapshot-anchored keyset page over a node's adjacency (shared
    /// by `get_outgoing_edges` / `get_incoming_edges` in cursor mode), ordered
    /// by edge id. The adjacency is read as of the pinned snapshot via the
    /// bi-temporal `get_*_edges_at_time` path, so paging is consistent under
    /// concurrent writes exactly like the node scans.
    #[allow(clippy::too_many_arguments)]
    fn emit_adjacency_cursor_page(
        &self,
        tool: &str,
        node_id: NodeId,
        incoming: bool,
        label: &Option<String>,
        snapshot: (Timestamp, Timestamp),
        after: Option<u64>,
        limit: usize,
        include_vectors: bool,
        parent_cid: String,
        now: Timestamp,
    ) -> CallToolResult {
        let (vt, tt) = snapshot;
        let edge_ids = if incoming {
            self.db.get_incoming_edges_at_time(node_id, vt, tt)
        } else {
            self.db.get_outgoing_edges_at_time(node_id, vt, tt)
        };

        // Resolve each candidate edge once as of the snapshot, applying the
        // optional label filter, then order by edge id for a stable keyset.
        let mut resolved: Vec<crate::core::Edge> = edge_ids
            .into_iter()
            .filter_map(|eid| self.get_edge_maybe_at(eid, Some((vt, tt))).ok())
            .filter(|e| {
                label
                    .as_ref()
                    .map(|l| self.matches_label(e.label, l))
                    .unwrap_or(true)
            })
            .collect();
        resolved.sort_by_key(|e| e.id.as_u64());

        let past = |id: u64| after.is_none_or(|a| id > a);
        let total_matching = resolved.len();
        let remaining = resolved.iter().filter(|e| past(e.id.as_u64())).count();
        let page: Vec<&crate::core::Edge> = resolved
            .iter()
            .filter(|e| past(e.id.as_u64()))
            .take(limit)
            .collect();
        let has_more = remaining > page.len();
        let last_id = page.last().map(|e| e.id.as_u64());

        let responses: Vec<EdgeResponse> = page
            .iter()
            .map(|e| self.edge_to_response(e, include_vectors, now))
            .collect();

        let mut obj = serde_json::Map::new();
        obj.insert(
            "edges".to_string(),
            serde_json::to_value(&responses).unwrap_or_else(|_| json!([])),
        );
        obj.insert("count".to_string(), json!(responses.len()));
        obj.insert("total_matching".to_string(), json!(total_matching));
        obj.insert("has_more".to_string(), json!(has_more));
        obj.insert(
            "snapshot_valid_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.0)),
        );
        obj.insert(
            "snapshot_transaction_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.1)),
        );
        obj.insert("paging".to_string(), json!("cursor"));

        if has_more && let Some(last_id) = last_id {
            let filters = json!({
                "node_id": node_id.as_u64(),
                "label": label,
                "incoming": incoming,
                "include_vectors": include_vectors,
            });
            let mut payload = CursorPayload::seed(
                tool,
                (snapshot.0.wallclock(), snapshot.1.wallclock()),
                limit,
                filters,
            );
            // Continuation key = last edge id ACTUALLY EMITTED on this page.
            // When #3353 token budgets land and can trim a page short of
            // `limit`, `after` MUST still be derived from the last row that
            // survived the trim (not the limit-th candidate), or the next page
            // would skip the trimmed-off edges.
            payload.after = Some(last_id);
            payload.cid = parent_cid;
            match self.cursors.issue(payload) {
                Ok(token) => {
                    obj.insert("cursor".to_string(), json!(token));
                    obj.insert(
                        "cursor_ttl_seconds".to_string(),
                        json!(self.cursors.ttl().as_secs()),
                    );
                }
                Err(e) => return self.error_result(e),
            }
        }

        self.success_json(serde_json::Value::Object(obj))
    }

    /// Cursor-mode dispatch shared by `get_outgoing_edges` /
    /// `get_incoming_edges` (Issue #3360). `incoming` selects the direction and
    /// the tool name the cursor is bound to.
    fn handle_adjacency_cursor(
        &self,
        tool: &str,
        incoming: bool,
        args: &serde_json::Value,
    ) -> CallToolResult {
        let now = time::now();

        if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, tool) {
                Ok(p) => p,
                Err(e) => return self.error_result(e),
            };
            let node_id = match NodeId::new(payload.filters["node_id"].as_u64().unwrap_or(0)) {
                Ok(id) => id,
                Err(e) => return self.invalid_argument(&e.to_string()),
            };
            let label = payload.filters["label"].as_str().map(str::to_string);
            let include_vectors = payload.filters["include_vectors"]
                .as_bool()
                .unwrap_or(false);
            let snapshot = (Timestamp::from(payload.svt), Timestamp::from(payload.stt));
            return self.emit_adjacency_cursor_page(
                tool,
                node_id,
                incoming,
                &label,
                snapshot,
                payload.after,
                payload.limit,
                include_vectors,
                payload.cid,
                now,
            );
        }

        let node_id = match NodeId::new(args.get("node_id").and_then(|v| v.as_u64()).unwrap_or(0)) {
            Ok(id) => id,
            Err(e) => return self.invalid_argument(&e.to_string()),
        };
        let label = Self::arg_str(args, "label");
        let include_vectors = args
            .get("include_vectors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let limit = Self::arg_limit(args);
        let snapshot = (now, now);
        self.emit_adjacency_cursor_page(
            tool,
            node_id,
            incoming,
            &label,
            snapshot,
            None,
            limit,
            include_vectors,
            String::new(),
            now,
        )
    }

    // ========================================================================
    // Tool Implementations
    // ========================================================================

    fn handle_get_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self.db.get_node(node_id) {
            Ok(node) => {
                let now = time::now();
                let response =
                    self.node_to_response(&node, req.include_vectors.unwrap_or(false), now);
                self.success_json(
                    serde_json::to_value(&response)
                        .expect("response serialization should not fail"),
                )
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_create_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: CreateNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let properties = match req.properties {
            Some(p) => match self.json_to_property_map(&p) {
                Ok(map) => map,
                Err(e) => return self.invalid_argument(&format!("Invalid properties: {}", e)),
            },
            None => PropertyMap::default(),
        };

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };
        let provenance = match self.parse_opt_provenance(req.provenance) {
            Ok(p) => p,
            Err(result) => return result,
        };

        let derived_from = match self.parse_derived_from(&req.derived_from) {
            Ok(refs) => refs,
            Err(result) => return result,
        };

        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        let created = if derived_from.is_empty() {
            self.db
                .create_node_with_options(&req.label, properties, options)
        } else {
            self.db
                .create_node_with_options_and_lineage(
                    &req.label,
                    properties,
                    options,
                    &derived_from,
                )
                .map(|(node_id, _version)| node_id)
        };

        match created {
            Ok(node_id) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let now = time::now();
                    let response = self.node_to_response(&node, true, now);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.db_error(e),
            },
            Err(e) => self.db_error(e),
        }
    }

    fn handle_update_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: UpdateNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let properties = match self.json_to_property_map(&req.properties) {
            Ok(map) => map,
            Err(e) => return self.invalid_argument(&format!("Invalid properties: {}", e)),
        };

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };
        let provenance = match self.parse_opt_provenance(req.provenance) {
            Ok(p) => p,
            Err(result) => return result,
        };

        let derived_from = match self.parse_derived_from(&req.derived_from) {
            Ok(refs) => refs,
            Err(result) => return result,
        };

        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        let updated = if derived_from.is_empty() {
            self.db
                .update_node_with_options(node_id, properties, options)
        } else {
            self.db
                .update_node_with_options_and_lineage(node_id, properties, options, &derived_from)
                .map(|_version| ())
        };

        match updated {
            Ok(()) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let now = time::now();
                    let response = self.node_to_response(&node, true, now);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.db_error(e),
            },
            Err(e) => self.db_error(e),
        }
    }

    fn handle_delete_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let detach = req.detach.unwrap_or(false);

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };

        if detach && valid_from.is_some() {
            return self.invalid_argument(
                "valid_time is not supported together with detach:true; cascade delete does \
                 not support backdating. Delete the connected edges individually with \
                 valid_time, or omit valid_time to cascade-delete at now.",
            );
        }

        // Issue #3427: stamp the authenticated session principal onto the
        // tombstone version's provenance. These destructive tools take NO
        // caller-supplied provenance (pass `None`), so the bundle is
        // principal-only and unforgeable from request fields — the principal
        // is server-derived by `parse_opt_provenance`, never read from the
        // request JSON. Anonymous sessions yield `None` (no principal field).
        let provenance = match self.parse_opt_provenance(None) {
            Ok(p) => p,
            Err(result) => return result,
        };
        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        // Perform the connected-edge check and the deletion inside a single write
        // transaction so they observe the same storage state. Splitting the count
        // into a separate transaction (or doing it before opening one) leaves a
        // check-then-act gap in which a concurrent writer could add an edge after
        // the count but before the delete, silently orphaning it. Keeping both in
        // one closure removes that cross-transaction gap (Issue #3209).
        enum Outcome {
            Refused { connected_edges: usize },
            Deleted { edges_removed: usize },
        }

        let outcome = self.db.write(|tx| -> crate::core::error::Result<Outcome> {
            // `count_connected_edges` reads the same `current` storage the
            // transaction's edge traversal uses, and also verifies the node
            // exists (errors propagate via `?`).
            let connected_edges = self.db.count_connected_edges(node_id)?;

            // Refuse-by-default: never report a bare success while silently
            // orphaning edges. The caller must opt into destruction via `detach`.
            if connected_edges > 0 && !detach {
                return Ok(Outcome::Refused { connected_edges });
            }

            if detach {
                // Cascade-equivalent delete: remove the node and all connected
                // edges, reporting exactly how many edges were removed. The
                // options (principal provenance) stamp every co-deleted edge
                // tombstone too, not just the node (Issue #3427).
                tx.delete_node_cascade_with_options(node_id, options.clone())?;
                Ok(Outcome::Deleted {
                    edges_removed: connected_edges,
                })
            } else {
                // No connected edges: a plain delete cannot orphan anything.
                // `options` carries the backdated valid_from (if any) and the
                // acting principal's provenance (Issue #3427).
                tx.delete_node_with_options(node_id, options.clone())?;
                Ok(Outcome::Deleted { edges_removed: 0 })
            }
        });

        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => return self.db_error(e),
        };

        match outcome {
            // The #3209 refusal in the #3234 structured shape:
            // `FAILED_PRECONDITION` with `details.connected_edges`, while the
            // legacy top-level fields are preserved additively (no loss).
            Outcome::Refused { connected_edges } => {
                let message = format!(
                    "Node {} has {} connected edge(s); refusing to delete. \
                     Pass `detach: true` to delete the node and its connected edges, \
                     or remove the edges first.",
                    req.node_id, connected_edges
                );
                let mut top_level = serde_json::Map::new();
                top_level.insert("success".to_string(), json!(false));
                top_level.insert("node_id".to_string(), json!(req.node_id));
                top_level.insert("connected_edges".to_string(), json!(connected_edges));
                top_level.insert("detach_required".to_string(), json!(true));
                Self::error_result_with_top_level(
                    McpError::new(McpErrorCode::FailedPrecondition, message).details(json!({
                        "node_id": req.node_id,
                        "connected_edges": connected_edges,
                        "detach_required": true
                    })),
                    top_level,
                )
            }
            Outcome::Deleted { edges_removed } => self.success_json(json!({
                "success": true,
                "deleted_node_id": req.node_id,
                "detached": detach,
                "edges_removed": edges_removed
            })),
        }
    }

    fn handle_retract_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: RetractNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (see handle_delete_node).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let detach = req.detach.unwrap_or(false);

        // Matching the #3221 convention: valid_time defaults to now.
        let valid_to = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v.unwrap_or_else(time::now),
            Err(result) => return result,
        };

        // Issue #3427: principal-only, unforgeable provenance (see
        // handle_delete_node) stamped onto the retraction version — and onto
        // every co-retracted edge in the detach branch below.
        let provenance = match self.parse_opt_provenance(None) {
            Ok(p) => p,
            Err(result) => return result,
        };

        // Perform the connected-edge check and the retraction inside a single
        // write transaction so they observe the same storage state — no
        // check-then-act gap for a concurrent writer to slip an edge into
        // (same rationale as handle_delete_node, Issue #3209).
        enum Outcome {
            Refused { connected_edges: usize },
            Retracted(crate::api::transaction::RetractionResult),
        }

        let outcome = self.db.write(|tx| -> crate::core::error::Result<Outcome> {
            use crate::api::transaction::ReadOps;

            // The connected-edge contract only applies to a currently-present
            // node; an already-retracted node short-circuits below to the
            // idempotent result (and a nonexistent one to NOT_FOUND). All
            // reads go through the transaction itself (buffer-aware,
            // snapshot-isolated) rather than back through `self.db`.
            if tx.get_node(node_id).is_ok() {
                // Enumerate DISTINCT connected edges once — the refusal
                // count and the detach co-retraction share the same
                // sort/dedup list, so `connected_edges` always equals what
                // `detach: true` would retract (a self-loop appears in both
                // adjacency directions but is one edge).
                let mut edge_ids = tx.get_outgoing_edges(node_id)?;
                edge_ids.extend(tx.get_incoming_edges(node_id)?);
                edge_ids.sort_unstable();
                edge_ids.dedup();
                let connected_edges = edge_ids.len();

                // Refuse-by-default: never report a bare success that leaves
                // edges pointing at a retracted node. The caller must opt
                // into co-retraction via `detach`.
                if connected_edges > 0 && !detach {
                    return Ok(Outcome::Refused { connected_edges });
                }

                if detach && connected_edges > 0 {
                    // Co-retract every connected edge at the same valid time,
                    // stamping the SAME acting principal onto each edge's
                    // retraction version as the node's (Issue #3427).
                    let mut edges_retracted = 0;
                    for edge_id in edge_ids {
                        let edge_result =
                            tx.retract_edge_with_provenance(edge_id, valid_to, provenance.clone())?;
                        if !edge_result.already_retracted {
                            edges_retracted += 1;
                        }
                    }

                    let mut result =
                        tx.retract_node_with_provenance(node_id, valid_to, provenance.clone())?;
                    result.edges_retracted = edges_retracted;
                    return Ok(Outcome::Retracted(result));
                }
            }

            Ok(Outcome::Retracted(tx.retract_node_with_provenance(
                node_id,
                valid_to,
                provenance.clone(),
            )?))
        });

        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => return self.db_error(e),
        };

        match outcome {
            // The refusal in the #3234 structured shape: FAILED_PRECONDITION
            // with `details.connected_edges`, legacy top-level fields
            // preserved additively (byte-for-byte parallel to the
            // handle_delete_node refusal).
            Outcome::Refused { connected_edges } => {
                let message = format!(
                    "Node {} has {} connected edge(s); refusing to retract. \
                     Pass `detach: true` to retract the node and its connected edges \
                     at the same valid time, or retract the edges first.",
                    req.node_id, connected_edges
                );
                let mut top_level = serde_json::Map::new();
                top_level.insert("success".to_string(), json!(false));
                top_level.insert("node_id".to_string(), json!(req.node_id));
                top_level.insert("connected_edges".to_string(), json!(connected_edges));
                top_level.insert("detach_required".to_string(), json!(true));
                Self::error_result_with_top_level(
                    McpError::new(McpErrorCode::FailedPrecondition, message).details(json!({
                        "node_id": req.node_id,
                        "connected_edges": connected_edges,
                        "detach_required": true
                    })),
                    top_level,
                )
            }
            Outcome::Retracted(result) => self.success_json(json!({
                "success": true,
                "node_id": req.node_id,
                "retracted": true,
                "already_retracted": result.already_retracted,
                "valid_from": Self::timestamp_to_rfc3339_micros(result.valid_from),
                "valid_to": Self::timestamp_to_rfc3339_micros(result.valid_to),
                "edges_retracted": result.edges_retracted
            })),
        }
    }

    fn handle_retract_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: RetractEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (see handle_delete_node).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        // Matching the #3221 convention: valid_time defaults to now.
        let valid_to = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v.unwrap_or_else(time::now),
            Err(result) => return result,
        };

        // Issue #3427: principal-only, unforgeable provenance (see
        // handle_delete_node) stamped onto the retraction version.
        let provenance = match self.parse_opt_provenance(None) {
            Ok(p) => p,
            Err(result) => return result,
        };

        match self
            .db
            .retract_edge_with_provenance(edge_id, valid_to, provenance)
        {
            Ok(result) => self.success_json(json!({
                "success": true,
                "edge_id": req.edge_id,
                "retracted": true,
                "already_retracted": result.already_retracted,
                "valid_from": Self::timestamp_to_rfc3339_micros(result.valid_from),
                "valid_to": Self::timestamp_to_rfc3339_micros(result.valid_to)
            })),
            Err(e) => self.db_error(e),
        }
    }

    fn handle_delete_node_cascade(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteNodeCascadeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        // Issue #3427: principal-only, unforgeable provenance (see
        // handle_delete_node) stamped onto the node's tombstone AND every
        // co-deleted edge's tombstone.
        let provenance = match self.parse_opt_provenance(None) {
            Ok(p) => p,
            Err(result) => return result,
        };
        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        match self
            .db
            .write(|tx| tx.delete_node_cascade_with_options(node_id, options.clone()))
        {
            Ok(()) => self.success_json(json!({
                "success": true,
                "deleted_node_id": req.node_id,
                "cascade": true
            })),
            Err(e) => self.db_error(e),
        }
    }

    /// Cursor-mode `list_nodes` (Issue #3360): snapshot-anchored keyset paging.
    ///
    /// The scan is pinned to the transaction time captured on the first page
    /// and every page is reconstructed as of that coordinate via the same
    /// point-in-time machinery `find_nodes_at_time` uses, so concurrent writes
    /// after the anchor are invisible: the union of all pages equals exactly a
    /// single unbounded `list_nodes` at the anchor moment. Requires `label`
    /// (an unlabeled list has no enumerable, ordered candidate set).
    fn handle_list_nodes_cursor(&self, args: &serde_json::Value) -> CallToolResult {
        let now = time::now();

        // Resume path: everything discriminating is baked into the token.
        if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, "list_nodes") {
                Ok(p) => p,
                Err(e) => return self.error_result(e),
            };
            let label = payload.filters["label"].as_str().unwrap_or("").to_string();
            let property_key = payload.filters["property_key"].as_str().map(str::to_string);
            let property_value = match &payload.filters["property_value"] {
                serde_json::Value::Null => None,
                v => Some(v.clone()),
            };
            let include_vectors = payload.filters["include_vectors"]
                .as_bool()
                .unwrap_or(false);
            let snapshot = (Timestamp::from(payload.svt), Timestamp::from(payload.stt));
            let candidates = match self.fetch_node_candidates(
                &label,
                &property_key,
                &property_value,
                snapshot,
            ) {
                Ok(c) => c,
                Err(result) => return result,
            };
            return self.emit_node_cursor_page(
                "list_nodes",
                snapshot,
                payload.after,
                payload.limit,
                include_vectors,
                payload.filters.clone(),
                candidates,
                payload.cid,
                now,
            );
        }

        // First page: validate and pin the snapshot at "now".
        let property_key = Self::arg_str(args, "property_key");
        let property_value = Self::arg_value(args, "property_value");
        if property_key.is_some() != property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }
        let label = match Self::arg_str(args, "label") {
            Some(l) => l,
            None => {
                return self.invalid_argument(
                    "Cursor paging requires 'label' (an unlabeled node list has no ordered, \
                     enumerable candidate set to page over).",
                );
            }
        };
        let limit = Self::arg_limit(args);
        let include_vectors = args
            .get("include_vectors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let snapshot = (now, now);
        let candidates =
            match self.fetch_node_candidates(&label, &property_key, &property_value, snapshot) {
                Ok(c) => c,
                Err(result) => return result,
            };
        let filters =
            Self::node_scan_filters(&label, &property_key, &property_value, include_vectors);
        self.emit_node_cursor_page(
            "list_nodes",
            snapshot,
            None,
            limit,
            include_vectors,
            filters,
            candidates,
            String::new(),
            now,
        )
    }

    fn handle_list_nodes(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360) is a distinct path
        // from the legacy offset paging below, which stays unchanged for
        // backward compatibility. The cursor parameters are additive and read
        // straight off the raw arguments (the request structs stay unchanged).
        if Self::cursor_requested(&args) {
            return self.handle_list_nodes_cursor(&args);
        }

        let req: ListNodesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits. A page must be able to carry a continuation
        // cursor, so the limit is at least 1 (limit:0 would otherwise report
        // has_more:true with next_offset==offset, a non-progressing page that
        // traps a paginating caller in an infinite loop).
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0).min(MAX_PAGINATION_OFFSET);

        // Validate property filter: both key and value are required together with label
        if req.property_key.is_some() != req.property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }
        if req.property_key.is_some() && req.label.is_none() {
            return self.invalid_argument("Property filtering requires 'label' to be specified");
        }

        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();

        // Property-based lookup: label + property_key + property_value
        if let (Some(label), Some(prop_key), Some(prop_val)) =
            (&req.label, &req.property_key, &req.property_value)
        {
            let property_value =
                match self.json_to_property_value(prop_val) {
                    Some(v) => v,
                    None => return self.invalid_argument(
                        "Unsupported property_value type. Use strings, numbers, booleans, or null.",
                    ),
                };

            let node_ids = self
                .db
                .find_nodes_by_property(label, prop_key, &property_value);

            // The full matching id list is already materialized, so the total
            // is cheap to report and `has_more` is exact.
            let total_matching = node_ids.len();
            let include_vectors = req.include_vectors.unwrap_or(false);
            let mut nodes = Vec::with_capacity(limit);
            for node_id in node_ids.into_iter().skip(offset).take(limit) {
                if let Ok(node) = self.db.get_node(node_id) {
                    nodes.push(self.node_to_response(&node, include_vectors, now));
                }
            }

            // `has_more`/`next_offset` are derived from the requested window
            // (`limit`) against `total_matching`, not from `nodes.len()`: a
            // stale property-index entry pointing at a since-deleted node is
            // still one of the `limit` ids this page consumed, so basing
            // `next_offset` on the (possibly smaller) resolved count would
            // re-skip into already-consumed ids and duplicate a row on the
            // next page.
            let has_more = offset.saturating_add(limit) < total_matching;
            let mut response = json!({
                "nodes": nodes,
                "count": nodes.len(),
                "offset": offset,
                "limit": limit
            });
            Self::attach_completeness(&mut response, offset, limit, has_more, Some(total_matching));
            return self.success_json(response);
        }

        // Label-only scan
        if let Some(label) = &req.label {
            let builder = crate::query::QueryBuilder::new().scan_label(label);

            // Note: We fetch offset+limit rows then skip offset. We fetch one
            // extra row (offset+limit+1) purely to detect whether more matching
            // nodes exist beyond this page (`has_more`) without paying for a
            // full-scan count. Offset is capped to prevent excessive memory use.
            match builder.limit(limit + offset + 1).execute(&self.db) {
                Ok(results) => {
                    // Use iterator-based approach to avoid allocating full Vec
                    let include_vectors = req.include_vectors.unwrap_or(false);
                    let mut nodes = Vec::with_capacity(limit);
                    let mut skipped = 0;
                    let mut has_more = false;

                    for row_result in results {
                        match row_result {
                            Ok(row) => {
                                if skipped < offset {
                                    skipped += 1;
                                    continue;
                                }
                                if let EntityResult::Node(node) = row.entity {
                                    if nodes.len() >= limit {
                                        // The extra (offset+limit+1)th matching
                                        // row proves more results remain.
                                        has_more = true;
                                        break;
                                    }
                                    nodes.push(self.node_to_response(&node, include_vectors, now));
                                }
                            }
                            Err(e) => return self.db_error(e),
                        }
                    }

                    // A label scan cannot cheaply know the matching total
                    // (that needs a full scan), so `total_matching` is omitted;
                    // `has_more` alone carries the completeness signal.
                    let mut response = json!({
                        "nodes": nodes,
                        "count": nodes.len(),
                        "offset": offset,
                        "limit": limit
                    });
                    Self::attach_completeness(&mut response, offset, nodes.len(), has_more, None);
                    self.success_json(response)
                }
                Err(e) => self.db_error(e),
            }
        } else {
            // Without a label filter, we cannot efficiently list all nodes
            // Return a helpful message
            let mut response = json!({
                "message": "Use 'label' filter to list nodes by type, or use 'count_nodes' for total count",
                "total_count": self.db.node_count(),
                "nodes": [],
                "count": 0,
                "offset": offset,
                "limit": limit
            });
            Self::attach_completeness(&mut response, offset, 0, false, None);
            self.success_json(response)
        }
    }

    fn handle_count_nodes(&self, args: serde_json::Value) -> CallToolResult {
        let req: CountNodesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        if let Some(label) = &req.label {
            // Use QueryBuilder to count by label efficiently without collecting all rows
            let builder = crate::query::QueryBuilder::new().scan_label(label);
            match builder.execute(&self.db) {
                Ok(mut results) => {
                    // Efficiently count without allocating a Vec
                    match results.try_fold(0usize, |acc, row| row.map(|_| acc + 1)) {
                        Ok(count) => self.success_json(json!({"count": count, "label": label})),
                        Err(e) => self.error_result(McpError::from_db_error(&e).with_message(
                            format!("Error counting nodes with label '{}': {}", label, e),
                        )),
                    }
                }
                Err(e) => self.error_result(McpError::from_db_error(&e).with_message(format!(
                    "Error executing count query for label '{}': {}",
                    label, e
                ))),
            }
        } else {
            self.success_json(json!({"count": self.db.node_count()}))
        }
    }

    fn handle_get_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self.db.get_edge(edge_id) {
            Ok(edge) => {
                let now = time::now();
                let response =
                    self.edge_to_response(&edge, req.include_vectors.unwrap_or(false), now);
                self.success_json(
                    serde_json::to_value(&response)
                        .expect("response serialization should not fail"),
                )
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_create_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: CreateEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let source_id = match NodeId::new(req.source_id) {
            Ok(id) => id,
            Err(e) => return self.invalid_argument(&format!("Invalid source_id: {}", e)),
        };

        let target_id = match NodeId::new(req.target_id) {
            Ok(id) => id,
            Err(e) => return self.invalid_argument(&format!("Invalid target_id: {}", e)),
        };

        let properties = match req.properties {
            Some(p) => match self.json_to_property_map(&p) {
                Ok(map) => map,
                Err(e) => return self.invalid_argument(&format!("Invalid properties: {}", e)),
            },
            None => PropertyMap::default(),
        };

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };
        let provenance = match self.parse_opt_provenance(req.provenance) {
            Ok(p) => p,
            Err(result) => return result,
        };

        let derived_from = match self.parse_derived_from(&req.derived_from) {
            Ok(refs) => refs,
            Err(result) => return result,
        };

        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        let created = if derived_from.is_empty() {
            self.db
                .create_edge_with_options(source_id, target_id, &req.label, properties, options)
        } else {
            self.db
                .create_edge_with_options_and_lineage(
                    source_id,
                    target_id,
                    &req.label,
                    properties,
                    options,
                    &derived_from,
                )
                .map(|(edge_id, _version)| edge_id)
        };

        match created {
            Ok(edge_id) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let now = time::now();
                    let response = self.edge_to_response(&edge, true, now);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.db_error(e),
            },
            Err(e) => self.db_error(e),
        }
    }

    fn handle_update_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: UpdateEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let properties = match self.json_to_property_map(&req.properties) {
            Ok(map) => map,
            Err(e) => return self.invalid_argument(&format!("Invalid properties: {}", e)),
        };

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };
        let provenance = match self.parse_opt_provenance(req.provenance) {
            Ok(p) => p,
            Err(result) => return result,
        };

        let derived_from = match self.parse_derived_from(&req.derived_from) {
            Ok(refs) => refs,
            Err(result) => return result,
        };

        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        let updated = if derived_from.is_empty() {
            self.db
                .update_edge_with_options(edge_id, properties, options)
        } else {
            self.db
                .update_edge_with_options_and_lineage(edge_id, properties, options, &derived_from)
                .map(|_version| ())
        };

        match updated {
            Ok(()) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let now = time::now();
                    let response = self.edge_to_response(&edge, true, now);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.db_error(e),
            },
            Err(e) => self.db_error(e),
        }
    }

    fn handle_delete_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };

        // Issue #3427: principal-only, unforgeable provenance (see
        // handle_delete_node) stamped onto the tombstone version.
        let provenance = match self.parse_opt_provenance(None) {
            Ok(p) => p,
            Err(result) => return result,
        };
        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        match self
            .db
            .write(|tx| tx.delete_edge_with_options(edge_id, options.clone()))
        {
            Ok(()) => self.success_json(json!({
                "success": true,
                "deleted_edge_id": req.edge_id
            })),
            Err(e) => self.db_error(e),
        }
    }

    fn handle_list_edges(&self, args: serde_json::Value) -> CallToolResult {
        // Cursor paging (Issue #3360) is not supported here: `list_edges` does
        // not enumerate edges (there is no global edge scan). Rather than
        // silently ignore the flag, direct the caller to the cursor-paged
        // adjacency tools. (No-silent-fallback culture.)
        if Self::cursor_requested(&args) {
            return self.error_result(
                McpError::new(
                    McpErrorCode::InvalidArgument,
                    "list_edges does not enumerate edges and is not cursorable. Use \
                     get_outgoing_edges or get_incoming_edges from a known node -- both support \
                     snapshot-anchored cursor paging (use_cursor / cursor).",
                )
                .details(json!({ "cursorable_alternatives": ["get_outgoing_edges", "get_incoming_edges"] })),
            );
        }

        let req: ListEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0);

        // Edges cannot be efficiently listed without knowing source/target nodes.
        // Provide helpful guidance to use get_outgoing_edges or get_incoming_edges.
        let mut response = json!({
            "message": "Use 'get_outgoing_edges' or 'get_incoming_edges' from a known node to list edges",
            "total_count": self.db.edge_count(),
            "edges": [],
            "count": 0,
            "offset": offset,
            "limit": limit,
            "label_filter": req.label
        });
        Self::attach_completeness(&mut response, offset, 0, false, None);
        self.success_json(response)
    }

    fn handle_count_edges(&self, args: serde_json::Value) -> CallToolResult {
        let req: CountEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Note: Counting by label is not efficiently supported without iterating all edges.
        // For now, we only support total count.
        if req.label.is_some() {
            self.success_json(json!({
                "message": "Counting edges by label is not supported. Use total_count instead.",
                "total_count": self.db.edge_count(),
                "count": null
            }))
        } else {
            self.success_json(json!({"count": self.db.edge_count()}))
        }
    }

    fn handle_get_outgoing_edges(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360); the full-adjacency
        // path below is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_adjacency_cursor("get_outgoing_edges", false, &args);
        }

        let req: GetOutgoingEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let edge_ids = if let Some(label) = &req.label {
            self.db.get_outgoing_edges_with_label(node_id, label)
        } else {
            self.db.get_outgoing_edges(node_id)
        };

        let include_vectors = req.include_vectors.unwrap_or(false);
        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();
        let edges: Vec<EdgeResponse> = edge_ids
            .into_iter()
            .filter_map(|eid| self.db.get_edge(eid).ok())
            .map(|e| self.edge_to_response(&e, include_vectors, now))
            .collect();

        // This handler returns the complete adjacency (no limit/offset), so the
        // result is never truncated: `has_more` is always false and
        // `total_matching` equals the returned count.
        let count = edges.len();
        let mut response = json!({
            "edges": edges,
            "count": count
        });
        Self::attach_completeness(&mut response, 0, 0, false, Some(count));
        self.success_json(response)
    }

    fn handle_get_incoming_edges(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360); the full-adjacency
        // path below is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_adjacency_cursor("get_incoming_edges", true, &args);
        }

        let req: GetIncomingEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let edge_ids = self.db.get_incoming_edges(node_id);

        // Filter by label if provided
        let include_vectors = req.include_vectors.unwrap_or(false);
        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();
        let edges: Vec<EdgeResponse> = edge_ids
            .into_iter()
            .filter_map(|eid| self.db.get_edge(eid).ok())
            .filter(|e| {
                req.label
                    .as_ref()
                    .map(|l| self.matches_label(e.label, l))
                    .unwrap_or(true)
            })
            .map(|e| self.edge_to_response(&e, include_vectors, now))
            .collect();

        // Complete adjacency (no limit/offset): never truncated, so
        // `has_more` is always false and `total_matching` equals the count.
        let count = edges.len();
        let mut response = json!({
            "edges": edges,
            "count": count
        });
        Self::attach_completeness(&mut response, 0, 0, false, Some(count));
        self.success_json(response)
    }

    /// Fetch a node, optionally as of a bi-temporal coordinate.
    fn get_node_maybe_at(
        &self,
        node_id: NodeId,
        temporal: Option<(Timestamp, Timestamp)>,
    ) -> Result<crate::core::Node, crate::core::error::Error> {
        match temporal {
            Some((vt, tt)) => self.db.get_node_at_time(node_id, vt, tt),
            None => self.db.get_node(node_id),
        }
    }

    /// Fetch an edge, optionally as of a bi-temporal coordinate.
    fn get_edge_maybe_at(
        &self,
        edge_id: EdgeId,
        temporal: Option<(Timestamp, Timestamp)>,
    ) -> Result<crate::core::Edge, crate::core::error::Error> {
        match temporal {
            Some((vt, tt)) => self.db.get_edge_at_time(edge_id, vt, tt),
            None => self.db.get_edge(edge_id),
        }
    }

    /// Fetch one candidate edge exactly once, check its label (unless the id
    /// list is already known to be label-filtered), and resolve the "next"
    /// node for this hop via `next_of` -- e.g. `|e| e.target` for an edge
    /// reached through the outgoing side, `|e| e.source` for the incoming
    /// side. Folding the label check and the source/target extraction into
    /// a single fetch avoids reconstructing the same edge twice per hop on
    /// the temporal path (each `get_edge_at_time` call is an index lookup
    /// plus a property reconstruction), and resolving `next_of` per-edge
    /// (rather than from the caller's top-level `direction` string) is what
    /// keeps a `"both"`-direction traversal correct: an edge discovered via
    /// the incoming side must resolve to `edge.source`, never `edge.target`.
    fn resolve_edge_hop(
        &self,
        edge_id: EdgeId,
        edge_label: &str,
        already_label_filtered: bool,
        next_of: impl Fn(&crate::core::Edge) -> NodeId,
        temporal: Option<(Timestamp, Timestamp)>,
    ) -> Option<NodeId> {
        let edge = self.get_edge_maybe_at(edge_id, temporal).ok()?;
        if !already_label_filtered && !self.matches_label(edge.label, edge_label) {
            return None;
        }
        Some(next_of(&edge))
    }

    /// Enumerate the next-hop node ids for a traversal step, optionally as
    /// of a bi-temporal coordinate. Current-state outgoing lookups are
    /// pre-filtered by label at the storage layer; every other combination
    /// (incoming, or the temporal index -- which has no label-aware
    /// incoming variant and, for outgoing, an existing
    /// `TemporalAdjacencyIndex::get_outgoing_with_label_at_time` that isn't
    /// yet plumbed through `HistoricalStorage`/`AletheiaDB`'s public API) is
    /// filtered by fetching each candidate edge once via `resolve_edge_hop`.
    fn traversal_next_hops(
        &self,
        current_id: NodeId,
        edge_label: &str,
        direction: &str,
        temporal: Option<(Timestamp, Timestamp)>,
    ) -> Vec<NodeId> {
        let outgoing = || -> Vec<NodeId> {
            match temporal {
                Some((vt, tt)) => self
                    .db
                    .get_outgoing_edges_at_time(current_id, vt, tt)
                    .into_iter()
                    .filter_map(|eid| {
                        self.resolve_edge_hop(eid, edge_label, false, |e| e.target, temporal)
                    })
                    .collect(),
                None => self
                    .db
                    .get_outgoing_edges_with_label(current_id, edge_label)
                    .into_iter()
                    .filter_map(|eid| {
                        self.resolve_edge_hop(eid, edge_label, true, |e| e.target, temporal)
                    })
                    .collect(),
            }
        };
        let incoming = || -> Vec<NodeId> {
            match temporal {
                Some((vt, tt)) => self.db.get_incoming_edges_at_time(current_id, vt, tt),
                None => self.db.get_incoming_edges(current_id),
            }
            .into_iter()
            .filter_map(|eid| self.resolve_edge_hop(eid, edge_label, false, |e| e.source, temporal))
            .collect()
        };

        match direction {
            "incoming" => incoming(),
            "both" => {
                let mut hops = outgoing();
                hops.extend(incoming());
                hops
            }
            _ => outgoing(),
        }
    }

    fn handle_traverse(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360). Unlike the id-keyset
        // node/adjacency scans, a DFS result order is not a simple id keyset,
        // so traverse's cursor pins the bi-temporal snapshot (making every
        // continuation page consistent -- AC2) and continues by an internal
        // offset over the deterministic DFS order (v1; a depth-independent
        // keyset traversal is a documented follow-up). The offset path below
        // is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_traverse_cursor(&args);
        }

        let req: TraverseRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let start_id = match NodeId::new(req.start_node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let temporal = match self
            .resolve_bitemporal_as_of(&req.as_of_valid_time, &req.as_of_transaction_time)
        {
            Ok(t) => t,
            Err(result) => return result,
        };

        // Apply resource limits to prevent DoS. A page must be able to carry a
        // continuation cursor, so the limit is at least 1 (limit:0 would
        // otherwise report has_more:true with next_offset==offset, a
        // non-progressing page that traps a paginating caller in a loop).
        let depth = req.depth.unwrap_or(1).min(MAX_TRAVERSAL_DEPTH);
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0).min(MAX_PAGINATION_OFFSET);
        let direction = req.direction.as_deref().unwrap_or("outgoing");
        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();

        let (results, has_more) = self.run_traversal(
            start_id,
            &req.edge_label,
            direction,
            depth,
            limit,
            offset,
            temporal,
            req.include_vectors.unwrap_or(false),
            now,
        );

        let count = results.len();
        let mut response = match temporal {
            Some((vt, tt)) => json!({
                "results": results,
                "count": count,
                "as_of_valid_time": time::to_iso8601(vt),
                "as_of_transaction_time": time::to_iso8601(tt),
            }),
            None => json!({
                "results": results,
                "count": count
            }),
        };
        // The matching total would require exhausting the traversal, so
        // `total_matching` is omitted; `has_more`/`next_offset` carry the
        // completeness signal.
        Self::attach_completeness(&mut response, offset, count, has_more, None);
        self.success_json(response)
    }

    /// The DFS core of `traverse`, shared by the offset path and the
    /// snapshot-anchored cursor path (Issue #3360). Returns the page of
    /// results (after skipping `offset`, taking `limit`) and whether more
    /// remain. Behavior is identical to the pre-refactor inline loop.
    #[allow(clippy::too_many_arguments)]
    fn run_traversal(
        &self,
        start_id: NodeId,
        edge_label: &str,
        direction: &str,
        depth: usize,
        limit: usize,
        offset: usize,
        temporal: Option<(Timestamp, Timestamp)>,
        include_vectors: bool,
        now: Timestamp,
    ) -> (Vec<TraversalResult>, bool) {
        // Use depth-first search (DFS) traversal.
        // DFS is chosen for memory efficiency: it processes nodes immediately rather than
        // queuing all nodes at each level. For large graphs with high branching factors,
        // this significantly reduces peak memory usage compared to BFS.
        let mut results: Vec<TraversalResult> = Vec::new();
        let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut frontier: Vec<(NodeId, Vec<u64>, usize)> =
            vec![(start_id, vec![start_id.as_u64()], 0)];
        // Caches whether a node exists as of `temporal`, so a node already
        // resolved (via the results-push lookup below, or a prior visit) is
        // never re-fetched just to decide whether to keep expanding past it.
        // Only consulted on the temporal path: the current-state path never
        // gated edge-following on node existence (an edge left dangling by a
        // non-cascade delete_node is documented, pre-existing behavior --
        // see CLAUDE.md's "Orphaned Edges" section -- and stays unchanged).
        let mut node_exists_cache: std::collections::HashMap<u64, bool> =
            std::collections::HashMap::new();
        // Offset pagination + completeness: `produced` counts every resolved
        // result node in traversal order; the first `offset` are skipped (a
        // prior page), the next `limit` are collected. Once the page is full
        // we expand the last collected node's own out-edges as usual (so
        // graph structure discovery for this hop isn't cut short), then stop:
        // `has_more` is read off whatever remains on `frontier` rather than by
        // resolving one further node. That peek-by-resolution would otherwise
        // force expansion through dangling/orphaned edges (current-state mode
        // keeps expanding past unresolvable nodes -- see the Err(_) arm below)
        // in search of a single confirmable result, which can walk the entire
        // remaining reachable subgraph just to answer `has_more`. Reading the
        // frontier instead is O(1) and conservative: it may say `true` when
        // the remaining candidates are all dangling and the next page would
        // in fact be empty, but it never requires unbounded work and never
        // under-reports.
        let mut produced: usize = 0;
        let mut has_more = false;

        while let Some((current_id, path, current_depth)) = frontier.pop() {
            let mut current_exists = true;
            if current_depth > 0 && !visited.contains(&current_id.as_u64()) {
                visited.insert(current_id.as_u64());
                match self.get_node_maybe_at(current_id, temporal) {
                    Ok(node) => {
                        produced += 1;
                        if produced > offset && results.len() < limit {
                            results.push(TraversalResult {
                                node: self.node_to_response(&node, include_vectors, now),
                                path: path.clone(),
                                depth: current_depth,
                            });
                        }
                    }
                    // Non-temporal: preserve the historical "keep expanding
                    // regardless" behavior. Temporal: a node absent at the
                    // coordinate must not have its edges followed further --
                    // otherwise nodes only reachable through it would
                    // surface despite being excluded from `results`.
                    Err(_) => current_exists = temporal.is_none(),
                }
                if temporal.is_some() {
                    node_exists_cache.insert(current_id.as_u64(), current_exists);
                }
            } else if temporal.is_some() {
                current_exists = *node_exists_cache
                    .entry(current_id.as_u64())
                    .or_insert_with(|| self.get_node_maybe_at(current_id, temporal).is_ok());
            }

            if current_depth < depth && current_exists {
                let next_ids =
                    self.traversal_next_hops(current_id, edge_label, direction, temporal);

                for next_id in next_ids {
                    if !visited.contains(&next_id.as_u64()) {
                        let mut new_path = path.clone();
                        new_path.push(next_id.as_u64());
                        frontier.push((next_id, new_path, current_depth + 1));
                    }
                }
            }

            if results.len() >= limit {
                has_more = !frontier.is_empty();
                break;
            }
        }

        (results, has_more)
    }

    /// Cursor-mode `traverse` (Issue #3360): snapshot-pinned offset
    /// continuation. On the first page the bi-temporal snapshot is pinned
    /// (to the request's `as_of_*` coordinate, or to "now" if none was given,
    /// so a current-state traversal still becomes a consistent point-in-time
    /// scan for the duration of the cursor). Every continuation re-walks the
    /// deterministic DFS as of that pinned snapshot and skips the already-seen
    /// prefix, so all pages reflect one consistent moment.
    fn handle_traverse_cursor(&self, args: &serde_json::Value) -> CallToolResult {
        let now = time::now();

        // Resolve page parameters, start node, and pinned snapshot, from the
        // token when resuming or from the request on the first page.
        let (
            start_id,
            edge_label,
            direction,
            depth,
            limit,
            offset,
            include_vectors,
            snapshot,
            parent_cid,
        ) = if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, "traverse") {
                Ok(p) => p,
                Err(e) => return self.error_result(e),
            };
            let f = &payload.filters;
            let start_id = match NodeId::new(f["start_node_id"].as_u64().unwrap_or(0)) {
                Ok(id) => id,
                Err(e) => return self.invalid_argument(&e.to_string()),
            };
            (
                start_id,
                f["edge_label"].as_str().unwrap_or("").to_string(),
                f["direction"].as_str().unwrap_or("outgoing").to_string(),
                f["depth"].as_u64().unwrap_or(1) as usize,
                payload.limit,
                payload.off as usize,
                f["include_vectors"].as_bool().unwrap_or(false),
                (Timestamp::from(payload.svt), Timestamp::from(payload.stt)),
                payload.cid,
            )
        } else {
            // First page: parse the request and pin the snapshot. If no as_of
            // was supplied, anchor at "now" so the whole scan is consistent.
            let start_id = match NodeId::new(
                args.get("start_node_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            ) {
                Ok(id) => id,
                Err(e) => return self.invalid_argument(&e.to_string()),
            };
            let temporal = match self.resolve_bitemporal_as_of(
                &Self::arg_str(args, "as_of_valid_time"),
                &Self::arg_str(args, "as_of_transaction_time"),
            ) {
                Ok(t) => t,
                Err(result) => return result,
            };
            let snapshot = temporal.unwrap_or((now, now));
            (
                start_id,
                Self::arg_str(args, "edge_label").unwrap_or_default(),
                Self::arg_str(args, "direction").unwrap_or_else(|| "outgoing".into()),
                args.get("depth")
                    .and_then(|v| v.as_u64())
                    .map(|d| d as usize)
                    .unwrap_or(1)
                    .min(MAX_TRAVERSAL_DEPTH),
                Self::arg_limit(args),
                0usize,
                args.get("include_vectors")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                snapshot,
                String::new(),
            )
        };

        let (results, has_more) = self.run_traversal(
            start_id,
            &edge_label,
            &direction,
            depth,
            limit,
            offset,
            Some(snapshot),
            include_vectors,
            now,
        );
        let count = results.len();

        let mut obj = serde_json::Map::new();
        obj.insert(
            "results".to_string(),
            serde_json::to_value(&results).unwrap_or_else(|_| json!([])),
        );
        obj.insert("count".to_string(), json!(count));
        obj.insert(
            "snapshot_valid_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.0)),
        );
        obj.insert(
            "snapshot_transaction_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.1)),
        );
        obj.insert("has_more".to_string(), json!(has_more));
        obj.insert("paging".to_string(), json!("cursor"));

        if has_more {
            let filters = json!({
                "start_node_id": start_id.as_u64(),
                "edge_label": edge_label,
                "direction": direction,
                "depth": depth,
                "include_vectors": include_vectors,
            });
            let mut payload = CursorPayload::seed(
                "traverse",
                (snapshot.0.wallclock(), snapshot.1.wallclock()),
                limit,
                filters,
            );
            // Continuation offset advances by the number of rows ACTUALLY
            // EMITTED (`count`). When #3353 token budgets land and can trim a
            // page short of `limit`, `off` MUST advance by the post-trim row
            // count (not `limit`), or the next page would skip trimmed rows.
            payload.off = (offset + count) as u64;
            payload.cid = parent_cid;
            match self.cursors.issue(payload) {
                Ok(token) => {
                    obj.insert("cursor".to_string(), json!(token));
                    obj.insert(
                        "cursor_ttl_seconds".to_string(),
                        json!(self.cursors.ttl().as_secs()),
                    );
                }
                Err(e) => return self.error_result(e),
            }
        }

        self.success_json(serde_json::Value::Object(obj))
    }

    fn handle_find_similar(&self, args: serde_json::Value) -> CallToolResult {
        let req: FindSimilarRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits. A page must be able to carry a continuation
        // cursor, so k is at least 1 (k:0 would otherwise report
        // has_more:true with next_offset==offset, a non-progressing page).
        let k = req.k.unwrap_or(DEFAULT_VECTOR_K).clamp(1, MAX_VECTOR_K);
        // Bound the total pagination window (offset+k) by MAX_VECTOR_K so the
        // over-fetch below never asks the vector index for more than
        // MAX_VECTOR_K+1 candidates regardless of offset -- otherwise a large
        // offset could force a search far past the MAX_VECTOR_K resource
        // budget the cap is meant to enforce. Vector-similarity pagination is
        // therefore bounded to the top MAX_VECTOR_K matches; an offset beyond
        // that horizon returns an empty, complete (`has_more: false`) page.
        let offset = req
            .offset
            .unwrap_or(0)
            .min(MAX_PAGINATION_OFFSET)
            .min(MAX_VECTOR_K.saturating_sub(k));

        if !self.db.is_vector_index_enabled_for(&req.property_name) {
            return self.failed_precondition(&format!(
                "Vector index not enabled for property '{}'. Use enable_vector_index first.",
                req.property_name
            ));
        }

        // Validate embedding dimensions
        if let Err(e) = self.validate_embedding_dimensions(&req.embedding, &req.property_name) {
            return self.invalid_argument(&e);
        }

        // Over-fetch one past the requested page (offset + k + 1, capped at
        // MAX_VECTOR_K + 1 by the offset bound above) so we can tell whether
        // more similar nodes exist beyond this page (`has_more`) without a
        // second query. The matching total would need a full index scan, so
        // `total_matching` is omitted.
        let fetch_k = k.saturating_add(offset).saturating_add(1);
        match self
            .db
            .similarity_search(crate::SimilarityQuery::from_embedding(req.embedding).k(fetch_k))
        {
            Ok(results) => {
                let has_more = results.len() > offset.saturating_add(k);
                let include_vectors = req.include_vectors.unwrap_or(false);
                // One request-scoped wallclock for every entity in the
                // response (Issue #3391).
                let now = time::now();
                let similarity_results: Vec<SimilarityResult> = results
                    .into_iter()
                    .skip(offset)
                    .take(k)
                    .filter_map(|(node_id, score)| {
                        self.db.get_node(node_id).ok().map(|node| SimilarityResult {
                            node: self.node_to_response(&node, include_vectors, now),
                            score,
                        })
                    })
                    .collect();

                let count = similarity_results.len();
                let mut response = json!({
                    "results": similarity_results,
                    "count": count
                });
                // `next_offset` advances by the requested window `k`, not the
                // (possibly smaller) resolved `count`: a since-deleted node
                // behind a stale vector-index entry is still one of the `k`
                // candidates this page consumed, so basing next_offset on
                // `count` would re-skip into already-consumed candidates and
                // duplicate a row on the next page.
                Self::attach_completeness(&mut response, offset, k, has_more, None);
                self.success_json(response)
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_enable_vector_index(&self, args: serde_json::Value) -> CallToolResult {
        let req: EnableVectorIndexRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let distance_metric = match req.distance_metric.as_deref().unwrap_or("cosine") {
            "euclidean" => DistanceMetric::Euclidean,
            "dot" | "dot_product" => DistanceMetric::DotProduct,
            _ => DistanceMetric::Cosine,
        };

        let config = HnswConfig::new(req.dimensions, distance_metric);

        match self.db.enable_vector_index(&req.property_name, config) {
            Ok(()) => self.success_json(json!({
                "success": true,
                "property_name": req.property_name,
                "dimensions": req.dimensions,
                "distance_metric": req.distance_metric.unwrap_or_else(|| "cosine".to_string())
            })),
            Err(e) => self.db_error(e),
        }
    }

    fn handle_list_vector_indexes(&self, _args: serde_json::Value) -> CallToolResult {
        let indexes = self.db.list_vector_indexes();
        let index_list: Vec<serde_json::Value> = indexes
            .into_iter()
            .map(|info| {
                json!({
                    "property_name": info.property_name,
                    "dimensions": info.dimensions,
                    "distance_metric": format!("{:?}", info.distance_metric)
                })
            })
            .collect();
        self.success_json(json!({
            "indexes": index_list,
            "count": index_list.len()
        }))
    }

    fn handle_enable_unique_constraint(&self, args: serde_json::Value) -> CallToolResult {
        let req: EnableUniqueConstraintRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        match self
            .db
            .unique_constraint(&req.label, &req.property)
            .enable()
        {
            Ok(()) => self.success_json(json!({
                "success": true,
                "label": req.label,
                "property": req.property
            })),
            Err(e) => self.db_error(e),
        }
    }

    fn handle_list_unique_constraints(&self, _args: serde_json::Value) -> CallToolResult {
        let constraints = self.db.list_unique_constraints();
        let list: Vec<serde_json::Value> = constraints
            .into_iter()
            .map(|(label, property)| json!({ "label": label, "property": property }))
            .collect();
        self.success_json(json!({
            "constraints": list,
            "count": list.len()
        }))
    }

    fn handle_get_node_at_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeAtTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_node_at_time(node_id, valid_time, tx_time) {
            Ok(node) => {
                let now = time::now();
                let response = self.node_to_response(&node, true, now);
                self.success_json(json!({
                    "node": response,
                    "valid_time": req.valid_time,
                    "transaction_time": Self::format_tx_time_response(req.transaction_time)
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_get_edge_at_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_edge_at_time(edge_id, valid_time, tx_time) {
            Ok(edge) => {
                let now = time::now();
                let response = self.edge_to_response(&edge, true, now);
                self.success_json(json!({
                    "edge": response,
                    "valid_time": req.valid_time,
                    "transaction_time": Self::format_tx_time_response(req.transaction_time)
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    /// Cursor-mode `find_nodes_at_time` (Issue #3360): snapshot-anchored keyset
    /// paging. The snapshot is the caller's requested `(valid_time,
    /// transaction_time)` -- already a point-in-time read, so consistency is
    /// native; continuation just seeks by node id past the last returned.
    fn handle_find_nodes_at_time_cursor(&self, args: &serde_json::Value) -> CallToolResult {
        let now = time::now();

        if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, "find_nodes_at_time") {
                Ok(p) => p,
                Err(e) => return self.error_result(e),
            };
            let label = payload.filters["label"].as_str().unwrap_or("").to_string();
            let property_key = payload.filters["property_key"].as_str().map(str::to_string);
            let property_value = match &payload.filters["property_value"] {
                serde_json::Value::Null => None,
                v => Some(v.clone()),
            };
            let include_vectors = payload.filters["include_vectors"]
                .as_bool()
                .unwrap_or(false);
            let snapshot = (Timestamp::from(payload.svt), Timestamp::from(payload.stt));
            let candidates = match self.fetch_node_candidates(
                &label,
                &property_key,
                &property_value,
                snapshot,
            ) {
                Ok(c) => c,
                Err(result) => return result,
            };
            return self.emit_node_cursor_page(
                "find_nodes_at_time",
                snapshot,
                payload.after,
                payload.limit,
                include_vectors,
                payload.filters.clone(),
                candidates,
                payload.cid,
                now,
            );
        }

        // First page: validate filter combo and pin the requested coordinate.
        let label = Self::arg_str(args, "label").unwrap_or_default();
        let property_key = Self::arg_str(args, "property_key");
        let property_value = Self::arg_value(args, "property_value");
        if property_key.is_some() != property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }
        let valid_time_str = Self::arg_str(args, "valid_time").unwrap_or_default();
        let valid_time = match self.parse_timestamp(&valid_time_str) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid valid_time: {}", e)),
        };
        let tx_time = match self
            .parse_optional_tx_time(Self::arg_str(args, "transaction_time").as_deref())
        {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid transaction_time: {}", e)),
        };
        let limit = Self::arg_limit(args);
        let include_vectors = args
            .get("include_vectors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let snapshot = (valid_time, tx_time);
        let candidates =
            match self.fetch_node_candidates(&label, &property_key, &property_value, snapshot) {
                Ok(c) => c,
                Err(result) => return result,
            };
        let filters =
            Self::node_scan_filters(&label, &property_key, &property_value, include_vectors);
        self.emit_node_cursor_page(
            "find_nodes_at_time",
            snapshot,
            None,
            limit,
            include_vectors,
            filters,
            candidates,
            String::new(),
            now,
        )
    }

    /// Find nodes by label (and optional exact property match) as of a
    /// bi-temporal point (Issue #3236).
    ///
    /// Validation mirrors `handle_list_nodes` (both-or-neither property
    /// filter, same limit/offset clamps); the temporal reconstruction is
    /// delegated to `AletheiaDB::find_nodes_at_time` /
    /// `find_nodes_by_property_at`, which reconstruct each candidate from
    /// the historical version visible at the queried coordinate -- so nodes
    /// deleted from current state are still found when both dimensions
    /// anchor before the deletion. The candidate set is capped at the same
    /// `max_schema_as_of_entities` limit bi-temporal `get_schema` uses; when
    /// truncated, the response discloses it via `sampled: true` and
    /// `total_matching`/`has_more` count matches within the sampled
    /// candidate set only.
    fn handle_find_nodes_at_time(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360); offset paging below
        // is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_find_nodes_at_time_cursor(&args);
        }

        let req: FindNodesAtTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Validate property filter: both key and value are required together
        // (mirroring list_nodes).
        if req.property_key.is_some() != req.property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid valid_time: {}", e)),
        };
        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid transaction_time: {}", e)),
        };

        // Apply resource limits exactly like list_nodes: a page must be able
        // to carry a continuation cursor, so the limit is at least 1.
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0).min(MAX_PAGINATION_OFFSET);

        let matches =
            if let (Some(prop_key), Some(prop_val)) = (&req.property_key, &req.property_value) {
                let property_value = match self.json_to_property_value(prop_val) {
                    Some(v) => v,
                    None => return self.invalid_argument(
                        "Unsupported property_value type. Use strings, numbers, booleans, or null.",
                    ),
                };
                self.db.find_nodes_by_property_at(
                    &req.label,
                    prop_key,
                    &property_value,
                    valid_time,
                    tx_time,
                )
            } else {
                self.db.find_nodes_at_time(&req.label, valid_time, tx_time)
            };

        match matches {
            Ok(matches) => {
                // The matching set is already materialized (sorted by node
                // id for stable pagination), so the total is cheap to report
                // and `has_more` is exact *within the candidate set*. When
                // `sampled` is true the candidate enumeration was truncated
                // at the configured cap, so `total_matching` is honest only
                // about the sampled candidates -- the flag discloses that.
                let sampled = matches.sampled;
                let matches = matches.nodes;
                let total_matching = matches.len();
                let include_vectors = req.include_vectors.unwrap_or(false);
                // One request-scoped wallclock for every entity in the
                // response (Issue #3391).
                let now = time::now();
                let nodes: Vec<NodeResponse> = matches
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|node| self.node_to_response(node, include_vectors, now))
                    .collect();

                let has_more = offset.saturating_add(limit) < total_matching;
                let mut response = json!({
                    "nodes": nodes,
                    "count": nodes.len(),
                    "offset": offset,
                    "limit": limit,
                    // Candidate-set truncation disclosure, mirroring
                    // get_schema's `sampled` (same underlying cap).
                    "sampled": sampled,
                    // The resolved coordinate this answer holds at -- the
                    // omitted transaction_time resolves to a concrete "now".
                    "valid_time": Self::format_timestamp_rfc3339(valid_time),
                    "transaction_time": Self::format_timestamp_rfc3339(tx_time),
                });
                Self::attach_completeness(
                    &mut response,
                    offset,
                    limit,
                    has_more,
                    Some(total_matching),
                );
                self.success_json(response)
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_list_changes(&self, args: serde_json::Value) -> CallToolResult {
        let req: ListChangesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let tx_from = match self.parse_timestamp(&req.tx_from) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid tx_from: {}", e)),
        };
        let tx_to = match self.parse_timestamp(&req.tx_to) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid tx_to: {}", e)),
        };

        let valid_from = match self.parse_opt_timestamp("valid_from", &req.valid_from) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let valid_to = match self.parse_opt_timestamp("valid_to", &req.valid_to) {
            Ok(v) => v,
            Err(resp) => return resp,
        };

        // A page must be able to carry a continuation cursor, so the limit is at least 1.
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);

        let query = ChangeFeedQuery {
            tx_from,
            tx_to,
            valid_from,
            valid_to,
            label: req.label.clone(),
            limit,
            cursor: req.cursor.clone(),
        };

        match self.db.list_changes(&query) {
            Ok(page) => {
                let changes: Vec<serde_json::Value> = page
                    .changes
                    .iter()
                    .map(|record| {
                        json!({
                            "entity_id": record.entity_id,
                            "version_id": record.version_id,
                            "kind": record.kind.as_str(),
                            "change_type": record.change_type.as_str(),
                            "label": record.label,
                            "transaction_time": time::to_iso8601(record.transaction_time()),
                            "transaction_time_range": {
                                "start": time::to_iso8601(record.transaction_time_range.start()),
                                "end": time::to_iso8601(record.transaction_time_range.end()),
                            },
                            "valid_time_range": {
                                "start": time::to_iso8601(record.valid_time_range.start()),
                                "end": time::to_iso8601(record.valid_time_range.end()),
                            },
                        })
                    })
                    .collect();

                self.success_json(json!({
                    "changes": changes,
                    "count": page.changes.len(),
                    "next_cursor": page.next_cursor,
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    // ============================================================================
    // Phase 11: Independent Dimension Temporal Queries & Version History
    // ============================================================================

    fn handle_get_node_at_valid_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeAtValidTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_node_at_valid_time(node_id, valid_time) {
            Ok(node) => {
                let now = time::now();
                let response = self.node_to_response(&node, true, now);
                self.success_json(json!({
                    "node": response,
                    "valid_time": req.valid_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_get_node_at_transaction_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeAtTransactionTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let tx_time = match self.parse_timestamp(&req.transaction_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_node_at_transaction_time(node_id, tx_time) {
            Ok(node) => {
                let now = time::now();
                let response = self.node_to_response(&node, true, now);
                self.success_json(json!({
                    "node": response,
                    "transaction_time": req.transaction_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_get_node_history(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeHistoryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self.db.get_node_history(node_id) {
            Ok(history) => {
                let versions: Vec<_> = history
                    .versions
                    .iter()
                    .map(|v| self.version_info_to_response(v))
                    .collect();

                self.success_json(json!({
                    "node_id": req.node_id,
                    "versions": versions,
                    "version_count": versions.len()
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_diff_node_versions(&self, args: serde_json::Value) -> CallToolResult {
        let req: DiffNodeVersionsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let from_version = match crate::core::id::VersionId::new(req.from_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let to_version = match crate::core::id::VersionId::new(req.to_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self
            .db
            .diff_node_versions(node_id, from_version, to_version)
        {
            Ok(diff) => {
                let response = self.version_diff_to_response(&diff);
                self.success_json(json!(response))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_get_edge_at_valid_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtValidTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_edge_at_valid_time(edge_id, valid_time) {
            Ok(edge) => {
                let now = time::now();
                let response = self.edge_to_response(&edge, true, now);
                self.success_json(json!({
                    "edge": response,
                    "valid_time": req.valid_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_get_edge_at_transaction_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtTransactionTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let tx_time = match self.parse_timestamp(&req.transaction_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_edge_at_transaction_time(edge_id, tx_time) {
            Ok(edge) => {
                let now = time::now();
                let response = self.edge_to_response(&edge, true, now);
                self.success_json(json!({
                    "edge": response,
                    "transaction_time": req.transaction_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_get_edge_history(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeHistoryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self.db.get_edge_history(edge_id) {
            Ok(history) => {
                let versions: Vec<_> = history
                    .versions
                    .iter()
                    .map(|v| self.version_info_to_response(v))
                    .collect();

                self.success_json(json!({
                    "edge_id": req.edge_id,
                    "versions": versions,
                    "version_count": versions.len()
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    fn handle_diff_edge_versions(&self, args: serde_json::Value) -> CallToolResult {
        let req: DiffEdgeVersionsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let from_version = match crate::core::id::VersionId::new(req.from_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let to_version = match crate::core::id::VersionId::new(req.to_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self
            .db
            .diff_edge_versions(edge_id, from_version, to_version)
        {
            Ok(diff) => {
                let response = self.version_diff_to_response(&diff);
                self.success_json(json!(response))
            }
            Err(e) => self.db_error(e),
        }
    }

    // Helper methods for converting internal types to response types

    fn version_info_to_response(&self, info: &crate::query::VersionInfo) -> serde_json::Value {
        use crate::core::temporal::TIMESTAMP_MAX;

        let properties = self.property_map_to_json(&info.properties, true);

        let valid_to = {
            let end = info.temporal.valid_time().end();
            if end == TIMESTAMP_MAX {
                None
            } else {
                Some(end.wallclock().to_string())
            }
        };

        let transaction_to = {
            let end = info.temporal.transaction_time().end();
            if end == TIMESTAMP_MAX {
                None
            } else {
                Some(end.wallclock().to_string())
            }
        };

        let mut value = json!({
            "version_number": info.version_number,
            "version_id": info.version_id.as_u64(),
            "valid_from": info.temporal.valid_time().start().wallclock().to_string(),
            "valid_to": valid_to,
            "transaction_from": info.temporal.transaction_time().start().wallclock().to_string(),
            "transaction_to": transaction_to,
            "properties": properties,
            "label": info.label
        });

        // Provenance is omitted entirely when absent -- never a fabricated
        // default (Issue #3224).
        if let Some(provenance) = &info.provenance
            && let Some(obj) = value.as_object_mut()
        {
            obj.insert(
                "provenance".to_string(),
                serde_json::to_value(provenance).unwrap_or(serde_json::Value::Null),
            );
        }

        value
    }

    fn version_diff_to_response(&self, diff: &crate::query::VersionDiff) -> serde_json::Value {
        let added = self.property_map_to_json(&diff.added, true);
        let removed = self.property_map_to_json(&diff.removed, true);

        let modified: Vec<_> = diff
            .modified
            .iter()
            .map(|(key, old_val, new_val)| {
                let key_str = GLOBAL_INTERNER
                    .resolve_with(*key, |s| s.to_string())
                    .unwrap_or_else(|| format!("{:?}", key));

                json!({
                    "key": key_str,
                    "old_value": self.property_value_to_json(old_val, true),
                    "new_value": self.property_value_to_json(new_val, true)
                })
            })
            .collect();

        json!({
            "from_version": diff.from_version.as_u64(),
            "to_version": diff.to_version.as_u64(),
            "added": added,
            "removed": removed,
            "modified": modified,
            "has_changes": diff.has_changes(),
            "change_count": diff.change_count()
        })
    }

    /// Convert a [`crate::db::GraphSchema`] into its JSON wire representation.
    fn schema_to_json(&self, schema: &crate::db::GraphSchema) -> serde_json::Value {
        let node_labels: Vec<serde_json::Value> = schema
            .node_labels
            .iter()
            .map(|l| {
                json!({
                    "label": l.label,
                    "count": l.count,
                    "property_keys": l.property_keys,
                })
            })
            .collect();

        let edge_types: Vec<serde_json::Value> = schema
            .edge_types
            .iter()
            .map(|e| {
                json!({
                    "edge_type": e.edge_type,
                    "count": e.count,
                    "property_keys": e.property_keys,
                })
            })
            .collect();

        json!({
            "node_labels": node_labels,
            "edge_types": edge_types,
            "total_nodes": schema.total_nodes,
            "total_edges": schema.total_edges,
            "sampled": schema.sampled,
            "as_of": schema.as_of.map(|instant| json!({
                "valid_time": time::to_iso8601(instant.valid_time),
                "transaction_time": time::to_iso8601(instant.transaction_time),
            })),
        })
    }

    /// Discover the graph's schema (labels, edge types, property keys),
    /// optionally as of a bi-temporal instant. Never errors on an empty
    /// database — returns a well-formed, empty summary instead.
    fn handle_get_schema(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetSchemaRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let temporal = match self
            .resolve_bitemporal_as_of(&req.as_of_valid_time, &req.as_of_transaction_time)
        {
            Ok(t) => t,
            Err(result) => return result,
        };

        let result = match temporal {
            None => self.db.schema(),
            Some((vt, tt)) => self.db.schema_as_of(vt, tt),
        };

        match result {
            Ok(schema) => self.success_json(self.schema_to_json(&schema)),
            Err(e) => self.db_error(e),
        }
    }

    /// Render a finite timestamp as an RFC3339 string (UTC, microsecond
    /// precision). Falls back to a raw microsecond form for coordinates
    /// outside chrono's representable range rather than panicking.
    fn timestamp_to_rfc3339(ts: Timestamp) -> String {
        let micros = ts.wallclock();
        DateTime::<Utc>::from_timestamp_micros(micros)
            .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
            .unwrap_or_else(|| format!("{micros}us"))
    }

    /// Serialize one dimension's bounds; `None` bounds become explicit JSON
    /// `null`s (never `0`/epoch), so an empty database is unambiguous.
    fn time_bounds_to_json(bounds: &crate::db::TimeBounds) -> serde_json::Value {
        json!({
            "earliest": bounds.earliest.map(Self::timestamp_to_rfc3339),
            "latest": bounds.latest.map(Self::timestamp_to_rfc3339),
        })
    }

    /// Convert a [`crate::db::TemporalExtent`] into its JSON wire
    /// representation. Serialization only — all bounds are computed by
    /// [`AletheiaDB::temporal_extent`]/[`AletheiaDB::temporal_extent_by_label`].
    fn temporal_extent_to_json(extent: &crate::db::TemporalExtent) -> serde_json::Value {
        let mut value = json!({
            "valid_time": Self::time_bounds_to_json(&extent.valid_time),
            "transaction_time": Self::time_bounds_to_json(&extent.transaction_time),
        });

        let label_extents_to_json = |extents: &[crate::db::LabelExtent], key: &str| {
            extents
                .iter()
                .map(|e| {
                    json!({
                        key: e.label,
                        "valid_time": Self::time_bounds_to_json(&e.valid_time),
                        "transaction_time": Self::time_bounds_to_json(&e.transaction_time),
                    })
                })
                .collect::<Vec<_>>()
        };

        if let Some(obj) = value.as_object_mut() {
            if let Some(node_labels) = &extent.node_labels {
                obj.insert(
                    "node_labels".to_string(),
                    serde_json::Value::Array(label_extents_to_json(node_labels, "label")),
                );
            }
            if let Some(edge_types) = &extent.edge_types {
                obj.insert(
                    "edge_types".to_string(),
                    serde_json::Value::Array(label_extents_to_json(edge_types, "edge_type")),
                );
            }
        }

        value
    }

    /// Report the dataset's queryable bi-temporal extent (Issue #3238).
    ///
    /// The handler only serializes: bounds come from the public
    /// `AletheiaDB::temporal_extent` / `temporal_extent_by_label` API.
    fn handle_temporal_extent(&self, args: serde_json::Value) -> CallToolResult {
        // The tool has no required arguments: a call with no `arguments`
        // object at all must behave like `{}`.
        let args = if args.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            args
        };

        let req: TemporalExtentRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let result = if req.by_label.unwrap_or(false) {
            self.db.temporal_extent_by_label()
        } else {
            self.db.temporal_extent()
        };

        match result {
            Ok(extent) => self.success_json(Self::temporal_extent_to_json(&extent)),
            Err(e) => self.db_error(e),
        }
    }

    /// Convert a core [`EntityId`](crate::core::id::EntityId) into the
    /// `(entity_kind, id)` pair used in lineage JSON responses.
    fn lineage_entity_parts(entity: crate::core::id::EntityId) -> (&'static str, u64) {
        match entity {
            crate::core::id::EntityId::Node(id) => ("node", id.as_u64()),
            crate::core::id::EntityId::Edge(id) => ("edge", id.as_u64()),
        }
    }

    /// Serialize one resolved lineage entry (version-pinned ref + depth +
    /// current-state status) for a lineage query response (Issue #3371).
    fn lineage_entry_to_json(entry: &crate::db::LineageViewEntry) -> serde_json::Value {
        let (entity_kind, id) = Self::lineage_entity_parts(entry.reference.entity);
        json!({
            "entity_kind": entity_kind,
            "id": id,
            "version": entry.reference.version.as_u64(),
            "depth": entry.depth,
            "status": entry.status.as_str(),
        })
    }

    /// Shared implementation of the `lineage_upstream` / `lineage_downstream`
    /// tools (Issue #3371): resolve the root, run the closure in `direction`,
    /// paginate, and shape the response with `has_more`/`next_offset` (#3226).
    fn handle_lineage_query(&self, args: serde_json::Value, upstream: bool) -> CallToolResult {
        let req: crate::mcp::tools::LineageQueryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let root_req = crate::mcp::tools::LineageRefRequest {
            entity_kind: req.entity_kind.clone(),
            id: req.id,
            version: req.version,
        };
        let root = match self.parse_lineage_ref(&root_req) {
            Ok(r) => r,
            Err(result) => return result,
        };

        let as_of =
            match self.parse_opt_timestamp("as_of_transaction_time", &req.as_of_transaction_time) {
                Ok(v) => v,
                Err(result) => return result,
            };

        let page_limit = req.limit.unwrap_or(100);
        let offset = req.offset.unwrap_or(0);
        // Fetch enough to cover the requested page window; slicing happens
        // below so `offset` paginates a stable breadth-first ordering.
        let fetch_limit = offset.saturating_add(page_limit);

        let mut options = crate::core::lineage::LineageQueryOptions::new().with_limit(fetch_limit);
        if let Some(max_depth) = req.max_depth {
            options = options.with_max_depth(max_depth);
        }
        if let Some(as_of) = as_of {
            options = options.with_as_of(as_of);
        }

        let view = if upstream {
            self.db.upstream_lineage(root, options)
        } else {
            self.db.downstream_lineage(root, options)
        };

        let page: Vec<serde_json::Value> = view
            .entries
            .iter()
            .skip(offset)
            .take(page_limit)
            .map(Self::lineage_entry_to_json)
            .collect();
        // The store already bounded the fetch to `fetch_limit`; anything it
        // dropped (limit or depth cap) is reported via `has_more`.
        let has_more = view.has_more;
        let returned = page.len();

        let (root_kind, root_id) = Self::lineage_entity_parts(root.entity);
        let mut response = json!({
            "direction": if upstream { "upstream" } else { "downstream" },
            "root": {
                "entity_kind": root_kind,
                "id": root_id,
                "version": root.version.as_u64(),
            },
            "entries": page,
            "count": returned,
        });
        if let Some(obj) = response.as_object_mut() {
            obj.insert("has_more".to_string(), json!(has_more));
            if has_more {
                obj.insert(
                    "next_offset".to_string(),
                    json!(offset.saturating_add(returned)),
                );
            }
        }
        self.success_json(response)
    }

    /// Handle the `lineage_upstream` tool (Issue #3371): "what was this fact
    /// derived from?" — the transitive evidence chain.
    fn handle_lineage_upstream(&self, args: serde_json::Value) -> CallToolResult {
        self.handle_lineage_query(args, true)
    }

    /// Handle the `lineage_downstream` tool (Issue #3371): "what has been
    /// derived from this fact?" — the retraction blast-radius report.
    fn handle_lineage_downstream(&self, args: serde_json::Value) -> CallToolResult {
        self.handle_lineage_query(args, false)
    }

    /// Handle the `audit_export` tool (Issue #3358).
    ///
    /// Produces a signed, offline-verifiable evidence artifact of an entity's
    /// complete bi-temporal history. The Ed25519 signing key is operator-
    /// provided out of band via the `ALETHEIADB_AUDIT_SIGNING_KEY` environment
    /// variable (a 32-byte hex seed); the secret is never returned or logged —
    /// only the public key travels in the artifact.
    fn handle_audit_export(&self, args: serde_json::Value) -> CallToolResult {
        use crate::audit::{AuditScope, AuditSigningKey, ExportOptions, SIGNING_KEY_ENV};

        let req: AuditExportRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let scope = match req.entity_type.as_str() {
            "node" => match NodeId::new(req.entity_id) {
                Ok(id) => AuditScope::node(id),
                Err(e) => return self.invalid_argument(&e.to_string()),
            },
            "edge" => match crate::core::id::EdgeId::new(req.entity_id) {
                Ok(id) => AuditScope::edge(id),
                Err(e) => return self.invalid_argument(&e.to_string()),
            },
            other => {
                return self.invalid_argument(&format!(
                    "entity_type must be 'node' or 'edge', got '{other}'"
                ));
            }
        };

        // The signing key is a precondition supplied by the operator, not a
        // caller argument — a missing key is a FAILED_PRECONDITION, never a
        // silent unsigned export.
        let signing_key = match AuditSigningKey::from_env(SIGNING_KEY_ENV) {
            Ok(k) => k,
            Err(_) => {
                return self.failed_precondition(&format!(
                    "audit export requires an operator-provided Ed25519 signing key in the \
                     {SIGNING_KEY_ENV} environment variable (32-byte hex seed)"
                ));
            }
        };

        let mut options =
            ExportOptions::new(req.database_id.unwrap_or_else(|| "aletheiadb".to_string()));
        if !req.redact_keys.is_empty() {
            options = options.redact(req.redact_keys);
        }

        match self.db.audit_export(scope, &signing_key, &options) {
            Ok(export) => match serde_json::to_value(&export) {
                Ok(artifact) => self.success_json(json!({
                    "artifact": artifact,
                    "public_key": signing_key.public_key().to_hex(),
                    "entity_count": export.entity_count(),
                    "version_count": export.version_count(),
                    "chain_root": export.chain.root,
                })),
                Err(e) => self.error_result(McpError::new(
                    McpErrorCode::Internal,
                    format!("failed to serialize artifact: {e}"),
                )),
            },
            Err(crate::audit::AuditError::NoHistory(msg)) => self.error_result(McpError::new(
                McpErrorCode::NotFound,
                format!("no exportable history: {msg}"),
            )),
            Err(e) => self.error_result(McpError::new(
                McpErrorCode::Internal,
                format!("audit export failed: {e}"),
            )),
        }
    }

    /// Handle the `database_stats` tool (Issue #3222).
    ///
    /// Thin aggregator: delegates entirely to the public
    /// [`AletheiaDB::stats`] snapshot and serializes it — no storage logic
    /// lives here. The underlying getters are all O(1)/cached (see
    /// `src/db/stats.rs`), so this never triggers a version scan.
    fn handle_database_stats(&self, args: serde_json::Value) -> CallToolResult {
        // The tool takes no required arguments; clients may send no
        // `arguments` at all (surfaced here as JSON null) or an empty
        // object. Normalize null so both forms are accepted.
        let args = if args.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            args
        };
        let _req: DatabaseStatsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        match serde_json::to_value(self.db.stats()) {
            Ok(value) => self.success_json(value),
            Err(e) => self.error_result(McpError::new(
                McpErrorCode::Internal,
                format!("Failed to serialize database stats: {}", e),
            )),
        }
    }

    /// Handle the `verify_chain` tool (Issue #3351): verify the tamper-evident
    /// provenance hash chain — full, entity-scoped, or against an exported
    /// anchor. Read-only; when the chain is not enabled the request is a
    /// structured `FAILED_PRECONDITION` (never a silent empty pass).
    fn handle_verify_chain(&self, args: serde_json::Value) -> CallToolResult {
        let args = if args.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            args
        };
        let req: VerifyChainRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Precedence: anchor extension > entity-scoped > full.
        let (verification, scope) = if let Some(anchor_value) = req.against {
            let anchor: crate::provenance_chain::ChainHead =
                match serde_json::from_value(anchor_value) {
                    Ok(a) => a,
                    Err(e) => {
                        return self.invalid_argument(&format!(
                            "`against` is not a valid exported chain head (as returned by \
                             export_chain_head): {e}"
                        ));
                    }
                };
            match self.db.verify_chain_against(&anchor) {
                Ok(v) => (v, "anchor"),
                Err(e) => return self.chain_not_enabled(e),
            }
        } else if req.entity_kind.is_some() || req.id.is_some() {
            let kind = match req.entity_kind.as_deref() {
                Some(k) => match k.trim().to_ascii_lowercase().as_str() {
                    "node" => crate::provenance_chain::EntityKind::Node,
                    "edge" => crate::provenance_chain::EntityKind::Edge,
                    other => {
                        return self.invalid_argument(&format!(
                            "entity_kind must be 'node' or 'edge', got '{other}'"
                        ));
                    }
                },
                None => {
                    return self.invalid_argument("entity_kind is required when `id` is supplied");
                }
            };
            let id = match req.id {
                Some(id) => id,
                None => {
                    return self
                        .invalid_argument("`id` is required when `entity_kind` is supplied");
                }
            };
            match self.db.verify_entity_chain(kind, id) {
                Ok(v) => (v, "entity"),
                Err(e) => return self.chain_not_enabled(e),
            }
        } else {
            match self.db.verify_chain() {
                Ok(v) => (v, "full"),
                Err(e) => return self.chain_not_enabled(e),
            }
        };

        self.success_json(json!({
            "scope": scope,
            "passed": verification.passed,
            "head_seq": verification.head_seq,
            "head_digest": verification.head_digest_hex,
            "earliest_broken_seq": verification.earliest_broken_seq,
            "reason": verification.reason,
            "transactions_checked": verification.transactions_checked,
        }))
    }

    /// Handle the `export_chain_head` tool (Issue #3351): export the current
    /// chain head as an external anchor for offline storage and later
    /// fork/rollback detection via `verify_chain`'s `against` argument.
    fn handle_export_chain_head(&self, args: serde_json::Value) -> CallToolResult {
        let args = if args.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            args
        };
        let _req: ExportChainHeadRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        match self.db.export_chain_head() {
            Ok(head) => match serde_json::to_value(&head) {
                Ok(value) => self.success_json(value),
                Err(e) => self.error_result(McpError::new(
                    McpErrorCode::Internal,
                    format!("Failed to serialize chain head: {}", e),
                )),
            },
            Err(e) => self.chain_not_enabled(e),
        }
    }

    /// Map the "chain not enabled" database error (Issue #3351) to a structured
    /// `FAILED_PRECONDITION` so a caller learns the chain must be enabled for
    /// this data dir rather than misreading a bare failure.
    fn chain_not_enabled(&self, e: crate::core::error::Error) -> CallToolResult {
        self.failed_precondition(&e.to_string())
    }

    fn handle_hybrid_query(&self, args: serde_json::Value) -> CallToolResult {
        let req: HybridQueryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let depth = req.traverse_depth.unwrap_or(1).min(MAX_TRAVERSAL_DEPTH);
        let k = req.top_k.unwrap_or(DEFAULT_VECTOR_K).min(MAX_VECTOR_K);

        // Parse temporal parameters if provided
        let valid_time = if let Some(ref vt) = req.valid_time {
            match self.parse_timestamp(vt) {
                Ok(t) => Some(t),
                Err(e) => return self.invalid_argument(&format!("Invalid valid_time: {}", e)),
            }
        } else {
            None
        };

        let tx_time = if let Some(ref tt) = req.transaction_time {
            match self.parse_timestamp(tt) {
                Ok(t) => Some(t),
                Err(e) => {
                    return self.invalid_argument(&format!("Invalid transaction_time: {}", e));
                }
            }
        } else {
            None
        };

        let include_vectors = req.include_vectors.unwrap_or(false);

        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();

        // Helper to convert rows to hybrid results with temporal info
        let rows_to_results =
            |rows: Vec<crate::query::executor::QueryRow>| -> Vec<HybridQueryResult> {
                rows.into_iter()
                    .filter_map(|row| {
                        if let EntityResult::Node(node) = row.entity {
                            Some(HybridQueryResult {
                                node: self.node_to_response(&node, include_vectors, now),
                                similarity_score: row.score,
                                traversal_path: row.path.map(|p| {
                                    p.iter()
                                        .map(|e| match e {
                                            ResultEntityId::Node(id) => id.as_u64(),
                                            ResultEntityId::Edge(id) => id.as_u64(),
                                        })
                                        .collect()
                                }),
                                timestamp: row.timestamp.map(|t| t.wallclock().to_string()),
                            })
                        } else {
                            None
                        }
                    })
                    .collect()
            };

        // Use QueryBuilder for hybrid queries
        if let Some(start_id) = req.start_node_id {
            let node_id = match NodeId::new(start_id) {
                Ok(id) => id,
                // Bare StorageError text verbatim — see the note on the other
                // ID-validation sites.
                Err(e) => return self.invalid_argument(&e.to_string()),
            };

            // If temporal filtering requested, use temporal query
            if let (Some(vt), Some(tt)) = (valid_time, tx_time) {
                // Temporal query for a single node
                return match self.db.get_node_at_time(node_id, vt, tt) {
                    Ok(node) => {
                        let response = self.node_to_response(&node, include_vectors, now);
                        self.success_json(json!({
                            "results": [HybridQueryResult {
                                node: response,
                                similarity_score: None,
                                traversal_path: Some(vec![node_id.as_u64()]),
                                timestamp: Some(vt.wallclock().to_string()),
                            }],
                            "count": 1,
                            "temporal_query": {
                                "valid_time": req.valid_time,
                                "transaction_time": req.transaction_time
                            }
                        }))
                    }
                    Err(e) => self.db_error(e),
                };
            }

            // Graph-first query with optional vector ranking
            let builder = crate::query::QueryBuilder::new().start(node_id);

            let builder = if let Some(ref edge_label) = req.traverse_edge {
                if depth > 1 {
                    builder.traverse_n(edge_label, depth)
                } else {
                    builder.traverse(edge_label)
                }
            } else {
                // Just return the start node
                return match self.db.get_node(node_id) {
                    Ok(node) => {
                        let response = self.node_to_response(&node, include_vectors, now);
                        self.success_json(json!({
                            "results": [HybridQueryResult {
                                node: response,
                                similarity_score: None,
                                traversal_path: Some(vec![node_id.as_u64()]),
                                timestamp: None,
                            }],
                            "count": 1
                        }))
                    }
                    Err(e) => self.db_error(e),
                };
            };

            // Execute and collect results
            match builder.limit(limit).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let hybrid_results = rows_to_results(rows);
                        self.success_json(json!({
                            "results": hybrid_results,
                            "count": hybrid_results.len()
                        }))
                    }
                    Err(e) => self.db_error(e),
                },
                Err(e) => self.db_error(e),
            }
        } else if let Some(ref embedding) = req.query_embedding {
            // Vector-first query
            // Use vector_property if specified
            let property_name = req.vector_property.as_deref().unwrap_or("embedding");

            // Check if vector index is enabled for the property
            if !self.db.is_vector_index_enabled_for(property_name) {
                return self.failed_precondition(&format!(
                    "Vector index not enabled for property '{}'. Use enable_vector_index first.",
                    property_name
                ));
            }

            // Validate embedding dimensions
            if let Err(e) = self.validate_embedding_dimensions(embedding, property_name) {
                return self.invalid_argument(&e);
            }

            let builder = crate::query::QueryBuilder::new().find_similar(embedding, k);

            match builder.limit(limit).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let hybrid_results = rows_to_results(rows);
                        self.success_json(json!({
                            "results": hybrid_results,
                            "count": hybrid_results.len(),
                            "vector_property": property_name
                        }))
                    }
                    Err(e) => self.db_error(e),
                },
                Err(e) => self.db_error(e),
            }
        } else if let Some(ref label) = req.filter_label {
            // Label scan query
            let builder = crate::query::QueryBuilder::new().scan_label(label);

            match builder.limit(limit).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let hybrid_results = rows_to_results(rows);
                        self.success_json(json!({
                            "results": hybrid_results,
                            "count": hybrid_results.len()
                        }))
                    }
                    Err(e) => self.db_error(e),
                },
                Err(e) => self.db_error(e),
            }
        } else {
            self.invalid_argument(
                "Must specify either start_node_id, query_embedding, or filter_label",
            )
        }
    }

    // ========================================================================
    // Declarative query tool (read-only Cypher / AQL) -- Issue #3213
    // ========================================================================

    /// Build a structured query-tool error payload.
    ///
    /// Distinct `kind`s let an LLM self-correct on retry: `invalid_request`,
    /// `read_only_violation`, `language_unavailable`, `parse_error`,
    /// `unsupported_construct`, `invalid_params`, `runtime_error`.
    ///
    /// The `kind` field is the query tool's own published contract (Issue
    /// #3213) and is preserved verbatim; the uniform `code`/`retriable`
    /// fields (Issue #3234) are added additively, derived from `kind`.
    fn query_error(
        &self,
        kind: &str,
        message: &str,
        clause: Option<&str>,
        language: Option<&str>,
    ) -> CallToolResult {
        let (code, retriable) = query_kind_classification(kind);
        self.query_error_classified(kind, code, retriable, message, clause, language)
    }

    /// [`query_error`](Self::query_error) with an explicit `code`/`retriable`
    /// classification, for callers that can classify more precisely than the
    /// kind-derived default (e.g. a `runtime_error` caused by a timeout is
    /// `UNAVAILABLE`/retriable rather than `INTERNAL`).
    fn query_error_classified(
        &self,
        kind: &str,
        code: McpErrorCode,
        retriable: bool,
        message: &str,
        clause: Option<&str>,
        language: Option<&str>,
    ) -> CallToolResult {
        let mut obj = serde_json::Map::new();
        obj.insert("kind".to_string(), json!(kind));
        obj.insert("code".to_string(), json!(code.as_str()));
        obj.insert("retriable".to_string(), json!(retriable));
        obj.insert("message".to_string(), json!(message));
        if let Some(clause) = clause {
            obj.insert("clause".to_string(), json!(clause));
        }
        if let Some(language) = language {
            obj.insert("language".to_string(), json!(language));
        }
        CallToolResult::error(vec![Content::text(
            json!({ "error": serde_json::Value::Object(obj) }).to_string(),
        )])
    }

    /// Map an engine error from query execution into a structured query-tool error.
    fn map_query_error(&self, error: crate::core::error::Error, language: &str) -> CallToolResult {
        use crate::core::error::{Error, QueryError};
        match error {
            Error::Query(QueryError::SyntaxError { message }) => {
                self.query_error("parse_error", &message, None, Some(language))
            }
            Error::Query(QueryError::UnsupportedFeature { feature }) => self.query_error(
                "unsupported_construct",
                &format!("Unsupported query construct: {feature}"),
                None,
                Some(language),
            ),
            Error::Query(QueryError::InvalidParameter { parameter, reason }) => self.query_error(
                "invalid_params",
                &format!("Invalid parameter '{parameter}': {reason}"),
                None,
                Some(language),
            ),
            Error::Query(QueryError::ExecutionError { message }) => {
                self.query_error("runtime_error", &message, None, Some(language))
            }
            // Anything else keeps kind "runtime_error" (the tool's own
            // contract) but classifies code/retriable from the actual error,
            // so e.g. a timeout is UNAVAILABLE/retriable, not INTERNAL.
            other => {
                let classified = McpError::from_db_error(&other);
                self.query_error_classified(
                    "runtime_error",
                    classified.code(),
                    classified.is_retriable(),
                    &other.to_string(),
                    None,
                    Some(language),
                )
            }
        }
    }

    /// Serialize a single query row (entity + score/path/timestamp) to JSON.
    ///
    /// Query rows carry only id/label/properties -- no provenance or
    /// temporal block -- so entities are serialized directly instead of
    /// through `node_to_response`/`edge_to_response`, which would pay a
    /// per-entity version-metadata lookup just to discard the result
    /// (Issue #3391).
    /// Serialize a single query-result entity (node/edge/id/null) to JSON.
    ///
    /// Shared by the single-entity row path and the multi-variable binding path
    /// (#549) so both render nodes/edges identically. `include_vectors: true` --
    /// query rows carry stored properties; vector elision is not applied here.
    fn query_entity_to_json(&self, entity: &EntityResult) -> serde_json::Value {
        match entity {
            EntityResult::Node(node) => json!({
                "type": "node",
                "id": node.id.as_u64(),
                "label": self.interned_to_string(node.label),
                "properties": self.property_map_to_json(&node.properties, true),
            }),
            EntityResult::Edge(edge) => json!({
                "type": "edge",
                "id": edge.id.as_u64(),
                "source_id": edge.source.as_u64(),
                "target_id": edge.target.as_u64(),
                "label": self.interned_to_string(edge.label),
                "properties": self.property_map_to_json(&edge.properties, true),
            }),
            EntityResult::NodeId(id) => json!({"type": "node", "id": id.as_u64()}),
            EntityResult::EdgeId(id) => json!({"type": "edge", "id": id.as_u64()}),
            // Null binding from an unmatched OPTIONAL MATCH pattern: surface
            // as JSON null so an LLM/caller sees the preserved row explicitly.
            EntityResult::Null => serde_json::Value::Null,
        }
    }

    fn query_row_to_json(&self, row: crate::query::executor::QueryRow) -> serde_json::Value {
        // Multi-variable binding row (#549): `MATCH (a),(b) RETURN a,b` binds
        // several variables, which the single `entity` field cannot represent
        // (its `entity` is `EntityResult::Null`). Serialize each bound variable
        // under its name, MERGED with any scalar `columns` (property/alias
        // projections). Checked BEFORE the columns-only branch because a binding
        // row can also carry columns. Without this, the row would render as a
        // lossy `{"entity": null}` and drop every bound entity.
        if let Some(bindings) = row.bindings {
            let mut obj = serde_json::Map::with_capacity(
                bindings.len() + row.columns.as_ref().map_or(0, Vec::len),
            );
            for (name, entity) in bindings {
                obj.insert(name, self.query_entity_to_json(&entity));
            }
            if let Some(columns) = row.columns {
                for (name, value) in columns {
                    obj.insert(name, self.property_value_to_json(&value, true));
                }
            }
            return serde_json::Value::Object(obj);
        }
        // Computed/aggregate row (e.g. `RETURN count(*)`, `RETURN n.dept,
        // count(*)`): the meaningful payload lives in `row.columns`, not
        // `row.entity` (which is `EntityResult::Null`). Render each named column
        // via `property_value_to_json` so an LLM sees the aggregate/group value
        // instead of a lossy `entity: null`. Closes the #558 MCP-surface
        // follow-up. `include_vectors: true` -- computed aggregate values are
        // not stored embeddings, so nothing should be elided.
        if let Some(columns) = row.columns {
            let mut obj = serde_json::Map::with_capacity(columns.len());
            for (name, value) in columns {
                obj.insert(name, self.property_value_to_json(&value, true));
            }
            return serde_json::Value::Object(obj);
        }
        let entity = self.query_entity_to_json(&row.entity);
        json!({
            "entity": entity,
            "score": row.score,
            "path": row.path.map(|p| {
                p.iter()
                    .map(|e| match e {
                        ResultEntityId::Node(id) => json!({"type": "node", "id": id.as_u64()}),
                        ResultEntityId::Edge(id) => json!({"type": "edge", "id": id.as_u64()}),
                    })
                    .collect::<Vec<_>>()
            }),
            "timestamp": row.timestamp.map(|t| t.wallclock().to_string()),
        })
    }

    /// Convert JSON parameter bindings into Cypher parameter values.
    #[cfg(feature = "cypher")]
    fn json_to_cypher_params(
        &self,
        params: Option<&HashMap<String, serde_json::Value>>,
    ) -> std::result::Result<HashMap<String, crate::cypher::CypherParameterValue>, (String, String)>
    {
        use crate::cypher::CypherParameterValue;
        let mut out = HashMap::new();
        let Some(map) = params else {
            return Ok(out);
        };
        for (key, value) in map {
            let pv = match value {
                serde_json::Value::Null => CypherParameterValue::Null,
                serde_json::Value::Bool(b) => CypherParameterValue::Bool(*b),
                serde_json::Value::Number(n) => {
                    // Check is_f64() first so that JSON floats (e.g. 1.0) are not
                    // silently coerced to Int by as_i64(), which would succeed for
                    // whole-number floats in some representations.
                    if n.is_f64() {
                        CypherParameterValue::Float(n.as_f64().unwrap())
                    } else if let Some(i) = n.as_i64() {
                        CypherParameterValue::Int(i)
                    } else if let Some(f) = n.as_f64() {
                        CypherParameterValue::Float(f)
                    } else {
                        return Err((key.clone(), "unsupported numeric value".to_string()));
                    }
                }
                serde_json::Value::String(s) => CypherParameterValue::String(s.clone()),
                serde_json::Value::Array(arr) => {
                    if arr.is_empty() {
                        return Err((
                            key.clone(),
                            "array parameters must not be empty; embeddings require at least \
                             one dimension"
                                .to_string(),
                        ));
                    }
                    let mut floats = Vec::with_capacity(arr.len());
                    for element in arr {
                        match element.as_f64() {
                            Some(f) => floats.push(f as f32),
                            None => {
                                return Err((
                                    key.clone(),
                                    "array parameters must contain only numbers (numeric arrays \
                                     are treated as embeddings)"
                                        .to_string(),
                                ));
                            }
                        }
                    }
                    CypherParameterValue::Embedding(Arc::from(floats))
                }
                serde_json::Value::Object(_) => {
                    return Err((
                        key.clone(),
                        "object parameters are not supported".to_string(),
                    ));
                }
            };
            out.insert(key.clone(), pv);
        }
        Ok(out)
    }

    fn handle_query(&self, args: serde_json::Value) -> CallToolResult {
        // Extract language early so it can appear in error payloads even when
        // full deserialization fails (language is not yet known at that point).
        let raw_language = args
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase());

        // Cursor paging is not supported for the declarative query tool in v1
        // (Issue #3360); captured before `args` is consumed by deserialization.
        let cursor_requested = Self::cursor_requested(&args);

        let req: QueryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => {
                return self.query_error(
                    "invalid_request",
                    &format!("Invalid arguments: {e}"),
                    None,
                    raw_language.as_deref(),
                );
            }
        };

        let language = req.language.to_ascii_lowercase();
        if language != "cypher" && language != "aql" {
            return self.query_error(
                "invalid_request",
                &format!(
                    "Unsupported query language '{}'. Use \"cypher\" or \"aql\".",
                    req.language
                ),
                None,
                Some(&language),
            );
        }

        // Cursor paging (Issue #3360) is not supported for the declarative
        // query tool in v1: arbitrary result shapes (projections, aggregates,
        // ordering) have no snapshot-anchored keyset to page over. Return a
        // structured `unsupported_construct` error rather than silently
        // serving a single truncated page (AC7: no silent fallback). Callers
        // needing consistent, resumable scans use `list_nodes` /
        // `find_nodes_at_time`, which are cursor-paged.
        if cursor_requested {
            return self.query_error(
                "unsupported_construct",
                "Cursor paging is not supported for the `query` tool in v1. Use `list_nodes` or \
                 `find_nodes_at_time` for snapshot-anchored, resumable cursor scans; the `query` \
                 tool returns a single (optionally `limit`-bounded) result set.",
                None,
                Some(&language),
            );
        }

        // Read-only guard: reject mutating statements BEFORE any execution.
        // Runs for every language so the tool can never write, even if the
        // grammars later gain write support.
        if let Some(clause) = detect_mutating_clause(&req.query) {
            return self.query_error(
                "read_only_violation",
                &format!(
                    "The `query` tool is read-only; the `{clause}` clause would mutate state and \
                     is rejected before execution."
                ),
                Some(clause),
                Some(&language),
            );
        }

        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let has_params = req.params.as_ref().is_some_and(|p| !p.is_empty());

        let execution = match language.as_str() {
            "aql" => {
                if has_params {
                    return self.query_error(
                        "invalid_request",
                        "AQL does not support parameter bindings; inline literal values or use \
                         language \"cypher\" with $params.",
                        None,
                        Some("aql"),
                    );
                }
                self.db.execute_aql(&req.query)
            }
            "cypher" => {
                #[cfg(feature = "cypher")]
                {
                    match self.json_to_cypher_params(req.params.as_ref()) {
                        Ok(params) if params.is_empty() => self.db.execute_cypher(&req.query),
                        Ok(params) => self.db.execute_cypher_with_params(&req.query, params),
                        Err((parameter, reason)) => {
                            return self.query_error(
                                "invalid_params",
                                &format!("Invalid parameter '{parameter}': {reason}"),
                                None,
                                Some("cypher"),
                            );
                        }
                    }
                }
                #[cfg(not(feature = "cypher"))]
                {
                    return self.query_error(
                        "language_unavailable",
                        "Cypher support is not compiled in (enable the `cypher` feature). Use \
                         language \"aql\" instead.",
                        None,
                        Some("cypher"),
                    );
                }
            }
            _ => unreachable!("language already validated above"),
        };

        let results = match execution {
            Ok(results) => results,
            Err(e) => return self.map_query_error(e, &language),
        };

        // Collect one extra row to detect (and report) truncation at the cap.
        let collected = match results.take_n(limit.saturating_add(1)) {
            Ok(rows) => rows,
            Err(e) => return self.map_query_error(e, &language),
        };
        let truncated = collected.len() > limit;
        // Detect computed/aggregate rows (#558): a row carrying named `columns`
        // (e.g. `RETURN count(*)`, `RETURN n.dept, count(*)`) reports its own
        // column schema in projection order. Ordinary entity rows leave this
        // `None`, so the static entity/score/path/timestamp schema is retained
        // byte-for-byte.
        // A multi-variable binding row (#549) advertises its columns
        // dynamically too: the bound variable names (in binding order) followed
        // by any scalar projection columns -- mirroring how aggregate rows
        // derive their dynamic schema -- so a caller can map row keys to
        // columns instead of seeing the static entity/score/path schema.
        let mut computed_columns: Option<Vec<String>> = None;
        let rows: Vec<serde_json::Value> = collected
            .into_iter()
            .take(limit)
            .map(|row| {
                if computed_columns.is_none() {
                    if let Some(bindings) = &row.bindings {
                        let mut names: Vec<String> =
                            bindings.iter().map(|(name, _)| name.clone()).collect();
                        if let Some(cols) = &row.columns {
                            names.extend(cols.iter().map(|(name, _)| name.clone()));
                        }
                        computed_columns = Some(names);
                    } else if let Some(cols) = &row.columns {
                        computed_columns =
                            Some(cols.iter().map(|(name, _)| name.clone()).collect());
                    }
                }
                self.query_row_to_json(row)
            })
            .collect();
        let row_count = rows.len();

        let columns = match &computed_columns {
            Some(names) => computed_query_columns(names),
            None => query_columns(),
        };

        self.success_json(json!({
            "language": language,
            "columns": columns,
            "rows": rows,
            "row_count": row_count,
            "truncated": truncated,
        }))
    }

    /// Test-only accessor returning the names of all advertised tools.
    #[cfg(test)]
    pub(crate) fn list_tools_for_test(&self) -> Vec<String> {
        tool_definitions()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// Test-only accessor returning the advertised input schema for a tool,
    /// so tests can pin schema contents (e.g. the apply_batch op variants)
    /// against silent drift.
    #[cfg(test)]
    pub(crate) fn tool_input_schema_for_test(&self, name: &str) -> Option<serde_json::Value> {
        tool_definitions()
            .iter()
            .find(|tool| tool.name == name)
            .map(|tool| serde_json::Value::Object((*tool.input_schema).clone()))
    }

    /// Dispatch a tool call by name to its handler.
    ///
    /// Shared between [`ServerHandler::call_tool`] and tests so that every
    /// advertised tool (including ones added later) can be driven through the
    /// exact same dispatch table the MCP transport uses — e.g. the
    /// registry-driven error-shape test iterates [`tool_definitions`] and
    /// calls this for each tool, guaranteeing new tools are automatically
    /// covered by the structured-error contract (Issue #3234).
    ///
    /// # Authentication & authorization (Issue #3350)
    ///
    /// This is the single enforcement point for the MCP surface: the session
    /// credential is (re-)verified and the tool's access class checked
    /// against the principal's role **before** any handler runs — including
    /// before tool-name resolution, so an unknown tool name cannot bypass
    /// authentication or probe the tool inventory. The per-tool public Rust
    /// methods (e.g. [`get_node`](Self::get_node)) are the embedded API and
    /// are not gated — a Rust caller already holds the `Arc<AletheiaDB>`.
    pub(crate) fn dispatch_tool(&self, name: &str, args: serde_json::Value) -> CallToolResult {
        if let Err(err) = self.auth.authorize_tool(name) {
            return self.error_result(err);
        }
        // Token-budget-aware response shaping (Issue #3353). For the read tools
        // listed in `BUDGETABLE_READ_TOOLS`, an optional `max_response_tokens` /
        // `max_response_bytes` shapes the successful response to fit the stated
        // budget with a disclosed truncation contract. Omitting the budget
        // parameters leaves behavior completely unchanged.
        if is_budgetable_read_tool(name) {
            match budget::parse_budget(&args) {
                Ok(Some(budget_req)) => {
                    // Retain the original arguments so the rung-4 truncation
                    // handle can emit a concrete offset-based resume call
                    // (Issue #3353 F1/F5).
                    let orig_args = args.clone();
                    let result = self.dispatch_read_tool(name, args);
                    return self.apply_budget(name, result, &budget_req, &orig_args);
                }
                Ok(None) => {}
                Err(err) => return self.error_result(err),
            }
        }
        self.dispatch_read_tool(name, args)
    }

    /// Apply the parsed token budget to a handler's result. Errors pass through
    /// untouched; a successful object response is shaped to fit and
    /// re-serialized, or replaced with the structured `INVALID_ARGUMENT`
    /// too-small-budget error (Issue #3353 AC6). Non-object / non-JSON success
    /// payloads cannot degrade along the entity ladder but are still held to the
    /// byte cap with a disclosed truncation marker — the "guaranteed to fit"
    /// contract is unconditional (Issue #3353 F6).
    fn apply_budget(
        &self,
        name: &str,
        result: CallToolResult,
        budget_req: &budget::BudgetRequest,
        args: &serde_json::Value,
    ) -> CallToolResult {
        if result.is_error.unwrap_or(false) {
            return result;
        }
        let text = Self::extract_text(result);
        let value: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            // Non-JSON payload: still enforce the byte cap on the raw text.
            Err(_) => {
                return match budget::enforce_raw_cap(text, budget_req) {
                    Ok(capped) => self.success_json(serde_json::Value::String(capped)),
                    Err(err) => self.error_result(err),
                };
            }
        };
        if !value.is_object() {
            // JSON scalar/array: not shapeable along the entity ladder, but the
            // serialized form is still capped so an unbounded array cannot
            // bypass the budget.
            let serialized =
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            if serialized.len() as u64 <= budget_req.effective_bytes() {
                return self.success_json(value);
            }
            return match budget::enforce_raw_cap(serialized, budget_req) {
                Ok(capped) => self.success_json(serde_json::Value::String(capped)),
                Err(err) => self.error_result(err),
            };
        }
        match budget::shape_response(value, budget_req, name, args) {
            Ok(shaped) => self.success_json(shaped),
            Err(err) => self.error_result(err),
        }
    }

    /// The tool-name dispatch table shared between the budgeted and unbudgeted
    /// paths.
    fn dispatch_read_tool(&self, name: &str, args: serde_json::Value) -> CallToolResult {
        match name {
            "get_node" => self.handle_get_node(args),
            "create_node" => self.handle_create_node(args),
            "update_node" => self.handle_update_node(args),
            "delete_node" => self.handle_delete_node(args),
            "retract_node" => self.handle_retract_node(args),
            "retract_edge" => self.handle_retract_edge(args),
            "delete_node_cascade" => self.handle_delete_node_cascade(args),
            "list_nodes" => self.handle_list_nodes(args),
            "count_nodes" => self.handle_count_nodes(args),
            "get_edge" => self.handle_get_edge(args),
            "create_edge" => self.handle_create_edge(args),
            "update_edge" => self.handle_update_edge(args),
            "delete_edge" => self.handle_delete_edge(args),
            "apply_batch" => self.handle_apply_batch(args),
            "list_edges" => self.handle_list_edges(args),
            "count_edges" => self.handle_count_edges(args),
            "get_outgoing_edges" => self.handle_get_outgoing_edges(args),
            "get_incoming_edges" => self.handle_get_incoming_edges(args),
            "traverse" => self.handle_traverse(args),
            "find_similar" => self.handle_find_similar(args),
            "enable_vector_index" => self.handle_enable_vector_index(args),
            "list_vector_indexes" => self.handle_list_vector_indexes(args),
            "enable_unique_constraint" => self.handle_enable_unique_constraint(args),
            "list_unique_constraints" => self.handle_list_unique_constraints(args),
            "get_node_at_time" => self.handle_get_node_at_time(args),
            "get_edge_at_time" => self.handle_get_edge_at_time(args),
            "find_nodes_at_time" => self.handle_find_nodes_at_time(args),
            "list_changes" => self.handle_list_changes(args),
            "get_node_at_valid_time" => self.handle_get_node_at_valid_time(args),
            "get_node_at_transaction_time" => self.handle_get_node_at_transaction_time(args),
            "get_node_history" => self.handle_get_node_history(args),
            "diff_node_versions" => self.handle_diff_node_versions(args),
            "get_edge_at_valid_time" => self.handle_get_edge_at_valid_time(args),
            "get_edge_at_transaction_time" => self.handle_get_edge_at_transaction_time(args),
            "get_edge_history" => self.handle_get_edge_history(args),
            "diff_edge_versions" => self.handle_diff_edge_versions(args),
            "hybrid_query" => self.handle_hybrid_query(args),
            "query" => self.handle_query(args),
            "get_schema" => self.handle_get_schema(args),
            "temporal_extent" => self.handle_temporal_extent(args),
            "lineage_upstream" => self.handle_lineage_upstream(args),
            "lineage_downstream" => self.handle_lineage_downstream(args),
            "audit_export" => self.handle_audit_export(args),
            "database_stats" => self.handle_database_stats(args),
            "verify_chain" => self.handle_verify_chain(args),
            "export_chain_head" => self.handle_export_chain_head(args),
            _ => self.error_result(
                McpError::new(McpErrorCode::NotFound, format!("Unknown tool: {}", name))
                    .details(json!({ "tool": name })),
            ),
        }
    }
}

/// The read tools that honor the Issue #3353 token budget (`max_response_tokens`
/// / `max_response_bytes`). Kept as a single source of truth so the dispatch
/// path, the schema-documentation path, and the CI conformance sweep cannot
/// drift apart.
pub(crate) const BUDGETABLE_READ_TOOLS: &[&str] = &[
    "get_node",
    "list_nodes",
    "get_edge",
    "list_edges",
    "get_outgoing_edges",
    "get_incoming_edges",
    "traverse",
    "find_similar",
    "hybrid_query",
    "query",
    "find_nodes_at_time",
    "get_node_history",
    "get_schema",
];

/// Does this tool honor the token budget parameters (Issue #3353)?
pub(crate) fn is_budgetable_read_tool(name: &str) -> bool {
    BUDGETABLE_READ_TOOLS.contains(&name)
}

fn make_input_schema<T: rmcp::schemars::JsonSchema>()
-> Arc<serde_json::Map<String, serde_json::Value>> {
    let schema = rmcp::schemars::schema_for!(T);
    let value = serde_json::to_value(schema).expect("JSON schema serialization should not fail");
    match value {
        serde_json::Value::Object(map) => Arc::new(map),
        _ => Arc::new(serde_json::Map::new()),
    }
}

/// Inject the snapshot-anchored cursor parameters (`use_cursor`, `cursor`) into
/// a generated tool `inputSchema` (Issue #3360).
///
/// The five cursorable read tools (`list_nodes`, `find_nodes_at_time`,
/// `get_outgoing_edges`, `get_incoming_edges`, `traverse`) read `use_cursor` /
/// `cursor` directly off the raw JSON arguments rather than through their typed
/// request structs, so those two parameters are absent from the derived schema
/// and would otherwise be undiscoverable to a client/LLM. This programmatically
/// adds them (both optional) to `inputSchema.properties` with clear
/// descriptions -- mirroring the sibling budget-param injection (#3353) rather
/// than threading the fields through every request-struct construction site.
fn inject_cursor_schema_params(
    schema: Arc<serde_json::Map<String, serde_json::Value>>,
) -> Arc<serde_json::Map<String, serde_json::Value>> {
    let mut map = (*schema).clone();
    let props = map
        .entry("properties")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(props) = props.as_object_mut() {
        props.insert(
            "use_cursor".to_string(),
            json!({
                "type": "boolean",
                "description": "Optional. Set true on the FIRST call to open a \
                    snapshot-anchored cursor scan; the response then carries an opaque \
                    `cursor` token to resume paging. Consistent, duplicate-free and \
                    gap-free under concurrent writes. Omit (or false) for the default \
                    offset pagination."
            }),
        );
        props.insert(
            "cursor".to_string(),
            json!({
                "type": "string",
                "description": "Optional. The opaque continuation token returned by a \
                    prior cursor page. Echo it back VERBATIM with no other arguments to \
                    fetch the next page. When the response omits `cursor` (has_more is \
                    false) the scan is complete."
            }),
        );
    }
    Arc::new(map)
}

/// [`make_input_schema`] for a cursorable read tool, with the `use_cursor` /
/// `cursor` parameters injected (see [`inject_cursor_schema_params`]).
fn make_cursor_input_schema<T: rmcp::schemars::JsonSchema>()
-> Arc<serde_json::Map<String, serde_json::Value>> {
    inject_cursor_schema_params(make_input_schema::<T>())
}

// The read-only statement guard (`detect_mutating_clause`) moved to
// `crate::query::read_only` so the HTTP surface can reuse it for RBAC
// classification (Issue #3350). Re-imported here to keep call sites stable.
use crate::query::read_only::detect_mutating_clause;

/// Column metadata describing the structured shape of each `query` result row.
///
/// `QueryRow` does not carry the `RETURN` aliases, so the contract is the fixed
/// row schema rather than per-query projection names. Constructed once and
/// cloned cheaply on each call via `OnceLock`.
fn query_columns() -> serde_json::Value {
    use std::sync::OnceLock;
    static COLUMNS: OnceLock<serde_json::Value> = OnceLock::new();
    COLUMNS
        .get_or_init(|| {
            json!([
                {
                    "name": "entity",
                    "type": "node|edge",
                    "description": "The node or edge bound by the RETURN clause (with label and properties)."
                },
                {
                    "name": "score",
                    "type": "number|null",
                    "description": "Vector similarity score, present for ranked queries."
                },
                {
                    "name": "path",
                    "type": "array<{type:string,id:number}>|null",
                    "description": "Typed entity references along the traversal path (each element has `type` and `id`), when applicable."
                },
                {
                    "name": "timestamp",
                    "type": "string|null",
                    "description": "Bi-temporal point this row represents, when applicable."
                }
            ])
        })
        .clone()
}

/// Build the column schema for a computed/aggregate `query` result (#558).
///
/// When a result carries named `QueryRow::columns` (e.g. `RETURN count(*)` or
/// `RETURN n.dept, count(*)`), the response advertises those column names in
/// projection order instead of the static entity/score/path/timestamp schema,
/// so a caller can map each row's values to their columns.
fn computed_query_columns(names: &[String]) -> serde_json::Value {
    serde_json::Value::Array(
        names
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "type": "value",
                    "description": "Computed column from a RETURN projection or aggregation."
                })
            })
            .collect(),
    )
}

/// Build the list of tool definitions advertised by this MCP server.
///
/// Shared between [`ServerHandler::list_tools`] and test helpers so the advertised tool
/// set and the `call_tool` dispatch table cannot silently drift apart.
fn tool_definitions() -> Vec<Tool> {
    let mut tools = vec![
        Tool::new(
            "get_node",
            "Get a node by its ID. Returns the node's label and properties. \
                     Vector/embedding properties are elided by default (replaced with a \
                     `{type, dim, elided:true}` descriptor) to protect LLM context; pass \
                     `include_vectors: true` to receive the full float array.",
            make_input_schema::<GetNodeRequest>(),
        ),
        Tool::new(
            "create_node",
            "Create a new node with a label and optional properties. Optionally pass \
                     `valid_time` (ISO 8601 or microseconds since epoch) to record when this fact \
                     became true in the real world (backdating/future-dating); omit it to default \
                     to the transaction time. Transaction time is always system-assigned.",
            make_input_schema::<CreateNodeRequest>(),
        ),
        Tool::new(
            "update_node",
            "Update an existing node's properties. Optionally pass `valid_time` (ISO 8601 or \
                     microseconds since epoch) to record when this update became true in the real \
                     world; omit it to default to the transaction time. Transaction time is always \
                     system-assigned.",
            make_input_schema::<UpdateNodeRequest>(),
        ),
        Tool::new(
            "delete_node",
            "Delete a node by its ID (safe-by-default). If the node has connected \
                     edges and `detach` is not true, the deletion is refused and the response \
                     reports `connected_edges`. Pass `detach: true` to delete the node together \
                     with all connected edges; the response then reports `edges_removed`. \
                     Optionally pass `valid_time` to record when this fact stopped being true in \
                     the real world; omit it to default to the transaction time. Not supported \
                     together with `detach: true` (cascade delete does not support backdating).",
            make_input_schema::<DeleteNodeRequest>(),
        ),
        Tool::new(
            "retract_node",
            "Retract a node as of a valid time (bi-temporal, safe-by-default): close its \
                     valid-time interval at `valid_time` (ISO 8601 or microseconds since epoch; \
                     defaults to now) WITHOUT deleting its history. Unlike delete_node, queries \
                     AS OF a valid time strictly before `valid_time` still return the node, and \
                     AS OF SYSTEM_TIME queries positioned before the retraction still show it \
                     open-ended. If the node has connected edges and `detach` is not true, the \
                     retraction is refused and the response reports `connected_edges`; pass \
                     `detach: true` to co-retract every connected edge at the same valid time \
                     (`edges_retracted` reports how many). Re-retracting is an idempotent no-op \
                     returning the existing interval with `already_retracted: true`. The response \
                     carries the closed half-open interval as `valid_from`/`valid_to` RFC 3339 \
                     strings. Transaction time is always system-assigned.",
            make_input_schema::<RetractNodeRequest>(),
        ),
        Tool::new(
            "retract_edge",
            "Retract an edge as of a valid time (bi-temporal): close its valid-time interval \
                     at `valid_time` (ISO 8601 or microseconds since epoch; defaults to now) \
                     WITHOUT deleting its history. Queries AS OF a valid time strictly before \
                     `valid_time` still return the edge; AS OF SYSTEM_TIME queries positioned \
                     before the retraction still show it open-ended. Re-retracting is an \
                     idempotent no-op returning the existing interval with \
                     `already_retracted: true`. The response carries the closed half-open \
                     interval as `valid_from`/`valid_to` RFC 3339 strings. Transaction time is \
                     always system-assigned.",
            make_input_schema::<RetractEdgeRequest>(),
        ),
        Tool::new(
            "delete_node_cascade",
            "Delete a node and all its connected edges (cascade delete). \
                     This maintains referential integrity by removing orphaned edges.",
            make_input_schema::<DeleteNodeCascadeRequest>(),
        ),
        Tool::new(
            "list_nodes",
            "List nodes with optional label filter and pagination. The response carries \
                     `has_more` (true when more matching nodes exist beyond this page — always \
                     check it before trusting a count); when true, `next_offset` gives the \
                     `offset` to pass for the next page. `total_matching` is included when cheap \
                     (property-filtered queries) and omitted for plain label scans. \
                     Vector/embedding properties are elided by default (replaced with a \
                     `{type, dim, elided:true}` descriptor) to protect LLM context; pass \
                     `include_vectors: true` to receive the full float arrays. \
                     Pass `use_cursor: true` for snapshot-anchored cursor paging; echo the \
                     returned `cursor` back verbatim (no other args) for the next page; when \
                     the response omits `cursor` / `has_more` is false, the scan is complete.",
            make_cursor_input_schema::<ListNodesRequest>(),
        ),
        Tool::new(
            "count_nodes",
            "Count the total number of nodes.",
            make_input_schema::<CountNodesRequest>(),
        ),
        Tool::new(
            "get_edge",
            "Get an edge by its ID. Vector/embedding properties are elided by default \
                     (replaced with a `{type, dim, elided:true}` descriptor) to protect LLM \
                     context; pass `include_vectors: true` to receive the full float array.",
            make_input_schema::<GetEdgeRequest>(),
        ),
        Tool::new(
            "create_edge",
            "Create a new edge between two nodes. Optionally pass `valid_time` (ISO 8601 or \
                     microseconds since epoch) to record when this relationship became true in the \
                     real world (backdating/future-dating); omit it to default to the transaction \
                     time. Transaction time is always system-assigned.",
            make_input_schema::<CreateEdgeRequest>(),
        ),
        Tool::new(
            "update_edge",
            "Update an existing edge's properties. Optionally pass `valid_time` (ISO 8601 or \
                     microseconds since epoch) to record when this update became true in the real \
                     world; omit it to default to the transaction time. Transaction time is always \
                     system-assigned.",
            make_input_schema::<UpdateEdgeRequest>(),
        ),
        Tool::new(
            "delete_edge",
            "Delete an edge by its ID. Optionally pass `valid_time` to record when this \
                     relationship stopped being true in the real world; omit it to default to the \
                     transaction time.",
            make_input_schema::<DeleteEdgeRequest>(),
        ),
        Tool::new(
            "apply_batch",
            "Apply an ordered list of write operations ATOMICALLY (all-or-nothing) in one \
                     call. Each operation is a tagged object (`op`: create_node, create_edge, \
                     update_node, update_edge, delete_node, delete_edge) mirroring the single-op \
                     tools, including optional per-op `valid_time`. A `create_node` may carry a \
                     `ref` alias; later edge operations may reference batch-created nodes as \
                     '$alias' (or positionally as '$<index>') wherever a node id is accepted as \
                     an endpoint — forward references are rejected. If ANY operation fails \
                     (validation, unknown ref, constraint violation, detach refusal), NONE of \
                     the batch's writes take effect: atomicity holds for every acknowledged \
                     outcome and all non-crash failures (narrow caveat: a process crash during \
                     the commit flush can persist a prefix of a batch until WAL transaction \
                     framing lands, issue #3413), and the error reports \
                     `details.failed_op_index`. `delete_node` honors the safe-by-default DETACH \
                     contract against committed AND batch-created edges. On success the response \
                     returns per-operation results in input order (ids and version ids for \
                     creates/updates) plus `ref_map` mapping every alias to its committed real \
                     id. Batch size is capped (default 1000; the limit is echoed on rejection). \
                     v1 limits: an op may not update/delete a node created in the same batch, \
                     and each committed entity accepts at most one write per batch.",
            make_input_schema::<ApplyBatchRequest>(),
        ),
        Tool::new(
            "list_edges",
            "List edges with optional label filter and pagination. Edges cannot be listed \
                     without a start node, so this returns guidance (use get_outgoing_edges / \
                     get_incoming_edges) with `has_more: false` for response-shape consistency. \
                     Vector/embedding properties are elided by default (replaced with a \
                     `{type, dim, elided:true}` descriptor) to protect LLM context; pass \
                     `include_vectors: true` to receive the full float arrays.",
            make_input_schema::<ListEdgesRequest>(),
        ),
        Tool::new(
            "count_edges",
            "Count the total number of edges.",
            make_input_schema::<CountEdgesRequest>(),
        ),
        Tool::new(
            "get_outgoing_edges",
            "Get all outgoing edges from a node. Returns the complete set (never \
                     truncated) in the default full-adjacency mode, so the response carries \
                     `has_more: false` and `total_matching` equal to `count`; with \
                     `use_cursor: true` the adjacency is paged and `has_more` may be true with a \
                     continuation `cursor`. Vector/embedding properties are elided \
                     by default (replaced with a `{type, dim, elided:true}` descriptor) to \
                     protect LLM context; pass `include_vectors: true` to receive the full float \
                     arrays. Pass `use_cursor: true` for snapshot-anchored cursor paging; echo \
                     the returned `cursor` back verbatim (no other args) for the next page; when \
                     the response omits `cursor` / `has_more` is false, the scan is complete.",
            make_cursor_input_schema::<GetOutgoingEdgesRequest>(),
        ),
        Tool::new(
            "get_incoming_edges",
            "Get all incoming edges to a node. Returns the complete set (never truncated) \
                     in the default full-adjacency mode, so the response carries \
                     `has_more: false` and `total_matching` equal to `count`; with \
                     `use_cursor: true` the adjacency is paged and `has_more` may be true with a \
                     continuation `cursor`. Vector/embedding properties are elided by \
                     default (replaced with a `{type, dim, elided:true}` descriptor) to protect \
                     LLM context; pass `include_vectors: true` to receive the full float arrays. \
                     Pass `use_cursor: true` for snapshot-anchored cursor paging; echo the \
                     returned `cursor` back verbatim (no other args) for the next page; when the \
                     response omits `cursor` / `has_more` is false, the scan is complete.",
            make_cursor_input_schema::<GetIncomingEdgesRequest>(),
        ),
        Tool::new(
            "traverse",
            "Traverse the graph starting from a node. The response carries `has_more` \
                     (true when the traversal was truncated by `limit` — check it before trusting \
                     `count`); when true, `next_offset` gives the `offset` to pass for the next \
                     page. Vector/embedding properties are \
                     elided by default (replaced with a `{type, dim, elided:true}` descriptor) to \
                     protect LLM context; pass `include_vectors: true` to receive the full float \
                     arrays. Accepts optional as_of_valid_time / as_of_transaction_time (ISO 8601 \
                     or microseconds since epoch) to walk the graph as it existed at that \
                     bi-temporal instant instead of the current state -- e.g. \"Alice's KNOWS \
                     network as of last year\". Edges/nodes not valid at that instant are \
                     excluded; omitting both parameters reproduces today's current-state behavior \
                     exactly. Pass `use_cursor: true` for snapshot-anchored cursor paging; echo \
                     the returned `cursor` back verbatim (no other args) for the next page; when \
                     the response omits `cursor` / `has_more` is false, the scan is complete.",
            make_cursor_input_schema::<TraverseRequest>(),
        ),
        Tool::new(
            "find_similar",
            "Find nodes similar to a query embedding. The response carries `has_more` \
                     (true when more similar nodes exist beyond the returned `k`); when true, \
                     `next_offset` gives the `offset` to pass for the next page. Pagination via \
                     `offset` only reaches the top-ranked matches up to the server's k cap \
                     (`offset + k` bounded); querying past that horizon returns an empty page with \
                     `has_more: false` rather than an unbounded search. The similarity \
                     `score` is always returned in full. Vector/embedding properties on the \
                     returned nodes are elided by default (replaced with a \
                     `{type, dim, elided:true}` descriptor) to protect LLM context; pass \
                     `include_vectors: true` to receive the full float arrays.",
            make_input_schema::<FindSimilarRequest>(),
        ),
        Tool::new(
            "enable_vector_index",
            "Enable vector indexing on a property.",
            make_input_schema::<EnableVectorIndexRequest>(),
        ),
        Tool::new(
            "list_vector_indexes",
            "List all enabled vector indexes.",
            make_input_schema::<ListVectorIndexesRequest>(),
        ),
        Tool::new(
            "enable_unique_constraint",
            "Enable a uniqueness constraint on a label+property pair. Fails fast if existing duplicates are found.",
            make_input_schema::<EnableUniqueConstraintRequest>(),
        ),
        Tool::new(
            "list_unique_constraints",
            "List all active uniqueness constraints.",
            make_input_schema::<ListUniqueConstraintsRequest>(),
        ),
        Tool::new(
            "get_node_at_time",
            "Get node state at a specific time.",
            make_input_schema::<GetNodeAtTimeRequest>(),
        ),
        Tool::new(
            "get_edge_at_time",
            "Get edge state at a specific time.",
            make_input_schema::<GetEdgeAtTimeRequest>(),
        ),
        Tool::new(
            "find_nodes_at_time",
            "Find nodes by label (and optional exact property_key/property_value match) as of \
                     a bi-temporal point -- resolve e.g. \"the Person named Alice as of \
                     2024-01-01\" in one call, without knowing any node ID. `valid_time` is \
                     required (ISO 8601 or microseconds since epoch); `transaction_time` is \
                     optional and defaults to now. Each returned node is reconstructed AS IT \
                     EXISTED at that coordinate (not its current state); nodes that did not \
                     exist, or whose property value did not hold, at that point are excluded. \
                     Recalling a since-deleted node requires anchoring BOTH dimensions before \
                     the deletion. Results are sorted by node id; the response echoes the \
                     resolved valid_time/transaction_time and carries `has_more`/`next_offset`/\
                     `total_matching` pagination metadata. On databases with very large \
                     bi-temporal history the candidate scan is capped; the response then sets \
                     `sampled: true` and `total_matching` counts matches within the sampled \
                     candidate set only. Vector/embedding properties are \
                     elided by default; pass `include_vectors: true` for full arrays. \
                     Pass `use_cursor: true` for snapshot-anchored cursor paging; echo the \
                     returned `cursor` back verbatim (no other args) for the next page; when the \
                     response omits `cursor` / `has_more` is false, the scan is complete.",
            make_cursor_input_schema::<FindNodesAtTimeRequest>(),
        ),
        Tool::new(
            "list_changes",
            "List graph-wide changes (node & edge versions) committed in a transaction-time window, with optional valid-time and label filters and stable cursor pagination. Discover what changed without knowing entity IDs.",
            make_input_schema::<ListChangesRequest>(),
        ),
        Tool::new(
            "get_node_at_valid_time",
            "Get node state at a specific valid time (independent dimension query).",
            make_input_schema::<GetNodeAtValidTimeRequest>(),
        ),
        Tool::new(
            "get_node_at_transaction_time",
            "Get node state at a specific transaction time (independent dimension query).",
            make_input_schema::<GetNodeAtTransactionTimeRequest>(),
        ),
        Tool::new(
            "get_node_history",
            "Get complete version history of a node.",
            make_input_schema::<GetNodeHistoryRequest>(),
        ),
        Tool::new(
            "diff_node_versions",
            "Compute the difference between two versions of a node.",
            make_input_schema::<DiffNodeVersionsRequest>(),
        ),
        Tool::new(
            "get_edge_at_valid_time",
            "Get edge state at a specific valid time (independent dimension query).",
            make_input_schema::<GetEdgeAtValidTimeRequest>(),
        ),
        Tool::new(
            "get_edge_at_transaction_time",
            "Get edge state at a specific transaction time (independent dimension query).",
            make_input_schema::<GetEdgeAtTransactionTimeRequest>(),
        ),
        Tool::new(
            "get_edge_history",
            "Get complete version history of an edge.",
            make_input_schema::<GetEdgeHistoryRequest>(),
        ),
        Tool::new(
            "diff_edge_versions",
            "Compute the difference between two versions of an edge.",
            make_input_schema::<DiffEdgeVersionsRequest>(),
        ),
        Tool::new(
            "hybrid_query",
            "Execute a hybrid query combining graph traversal, vector similarity, and \
                     temporal filtering. The `similarity_score` is always returned in full. \
                     Vector/embedding properties on returned nodes are elided by default \
                     (replaced with a `{type, dim, elided:true}` descriptor) to protect LLM \
                     context; pass `include_vectors: true` to receive the full float arrays.",
            make_input_schema::<HybridQueryRequest>(),
        ),
        Tool::new(
            "query",
            "Execute a single READ-ONLY declarative query and get structured rows back, \
             replacing a chain of get_node/traverse/filter calls. \
             `language` is \"cypher\" or \"aql\"; pass the statement in `query` and optional \
             `$param` bindings in `params` (Cypher only; numeric arrays are treated as \
             embeddings). \
             Supported read-only subset: MATCH patterns with node/edge labels and inline \
             property filters, variable-depth traversal (-[:REL*1..3]->), directions \
             (->, <-, -), WHERE, RETURN [DISTINCT] / AS aliases, ORDER BY, SKIP/LIMIT, WITH \
             chaining, vector similarity ranking, and bi-temporal scoping \
             (AS OF TIMESTAMP/VALID_TIME/SYSTEM_TIME, FOR SYSTEM_TIME AS OF, BETWEEN ... AND ...). \
             Mutating statements (CREATE/MERGE/SET/DELETE/REMOVE/DETACH/DROP/CALL/FOREACH/LOAD) \
             are rejected before execution and never write. Results are capped (default 100, \
             max 10000 rows; `truncated` indicates a cap hit). Errors are returned as a \
             structured {error:{kind,code,retriable,message,clause?,language}} payload \
             (kinds: invalid_request, read_only_violation, language_unavailable, parse_error, \
             unsupported_construct, invalid_params, runtime_error; code/retriable follow the \
             uniform MCP error contract) so callers can self-correct.",
            make_input_schema::<QueryRequest>(),
        ),
        Tool::new(
            "get_schema",
            "Discover the graph's schema: distinct node labels and edge/relationship types, \
             each with an entity count and the union of property keys observed. Call this \
             first to learn what labels/edge-types/properties exist before guessing names in \
             list_nodes, traverse, or query. Accepts optional as_of_valid_time / \
             as_of_transaction_time (ISO 8601 or microseconds since epoch) to return the \
             schema as it existed at that bi-temporal instant instead of the current state. \
             Returns a well-formed empty summary on an empty database, never an error.",
            make_input_schema::<GetSchemaRequest>(),
        ),
        Tool::new(
            "temporal_extent",
            "Report the dataset's queryable bi-temporal extent: valid_time {earliest, latest} \
             and transaction_time {earliest, latest} as RFC3339 strings, in one call. Call \
             this BEFORE issuing AS OF queries so the target instant lands inside recorded \
             data — an AS OF before valid_time.earliest returns empty results that are \
             otherwise indistinguishable from 'the fact never existed'. Bounds cover ALL \
             recorded history, including expired/superseded versions and deletions (a 2019 \
             fact later corrected still counts toward earliest); this is a calendar RANGE, \
             not a current-state count. Convention: earliest = the minimum interval start in \
             that dimension; latest = the maximum of interval starts and CLOSED interval \
             ends. Open-ended intervals (still-valid facts / still-current records) \
             contribute only their start, so latest is the newest finite recorded event \
             coordinate, never +infinity. An empty database returns explicit nulls for every \
             bound, never 0/epoch-1970. Bounds only ever widen while the server runs, so \
             callers can cache the result for a session. Coverage caveat: bounds span all \
             history recorded during the current process lifetime plus hot-tier history \
             restored at startup; on databases with cold-storage migration, versions migrated \
             to cold storage before the last restart are not reflected. Pass by_label: true \
             to also receive the same bounds per node label (node_labels) and per edge type \
             (edge_types); per-label bounds are computed from hot-tier history only and may \
             be narrower than the overall bounds (or a label absent) after cold migration.",
            make_input_schema::<TemporalExtentRequest>(),
        ),
        Tool::new(
            "lineage_upstream",
            "Query the UPSTREAM derivation lineage of a fact (Issue #3371): 'what was this fact \
             derived from?', transitively — the evidence chain for citation-grade answers. \
             Arguments: entity_kind ('node'|'edge'), id, version (lineage is version-pinned; use \
             the exact version whose evidence you want), optional max_depth (transitive hop cap; \
             1 = direct parents), optional limit (default 100) and offset for pagination, and \
             optional as_of_transaction_time (ISO 8601 / RFC 3339 or integer microseconds) to see \
             lineage as it was recorded by that transaction time. Returns {direction:'upstream', \
             root, entries[], count, has_more, next_offset?}; each entry is a version-pinned ref \
             {entity_kind, id, version} plus depth (min hops from the root) and status \
             ('current'|'superseded'|'absent'). Lineage records are immutable, so a source that \
             was later retracted/deleted still resolves and is marked 'absent'. Declare lineage at \
             write time via the create_node/create_edge/update_node/update_edge `derived_from` \
             parameter.",
            make_input_schema::<LineageQueryRequest>(),
        ),
        Tool::new(
            "lineage_downstream",
            "Query the DOWNSTREAM derivation lineage of a fact (Issue #3371): 'what has been \
             derived from this fact?', transitively — the retraction BLAST RADIUS. When an input \
             fact is found wrong or retracted, one call enumerates every transitively derived \
             fact so you can assess contamination. Arguments: entity_kind ('node'|'edge'), id, \
             version, optional max_depth (transitive hop cap; 1 = direct children), optional limit \
             (default 100) and offset for pagination, and optional as_of_transaction_time (ISO \
             8601 / RFC 3339 or integer microseconds). Returns {direction:'downstream', root, \
             entries[], count, has_more, next_offset?}; each entry is a version-pinned ref \
             {entity_kind, id, version} plus depth (min hops from the root) and status \
             ('current'|'superseded'|'absent'). Version-pinned: the closure reflects exactly the \
             versions that declared this fact as a source.",
            make_input_schema::<LineageQueryRequest>(),
        ),
        Tool::new(
            "audit_export",
            "Produce a SIGNED, self-contained audit export of a single entity's (node or edge) \
             complete bi-temporal history plus provenance — a portable evidence artifact a \
             third party can verify OFFLINE with only the signer's public key (no database, no \
             network). Use this to answer 'prove what the system knew about entity X, and \
             when' for compliance, audits, GDPR/CCPA subject-access requests, or legal \
             discovery. Arguments: entity_type ('node'|'edge'), entity_id, optional \
             database_id (recorded in the artifact), optional redact_keys (property keys whose \
             VALUES are omitted while the redaction stays recorded and verifiable). The \
             artifact contains every version across both time dimensions (including superseded \
             versions and delete tombstones), per-version provenance/principal where recorded, \
             a per-version SHA-256 hash chain, and an Ed25519 signature over the chain root. \
             The Ed25519 signing key is operator-provided out of band via the \
             ALETHEIADB_AUDIT_SIGNING_KEY environment variable (32-byte hex seed); the secret \
             is never returned — only the public key travels in the artifact. Returns the \
             artifact JSON plus public_key, chain_root, and entity/version counts. Note: \
             delete/retract operations do not yet stamp an authenticated principal (#3427); \
             the export surfaces provenance faithfully, including its absence, and never \
             fabricates attribution.",
            make_input_schema::<AuditExportRequest>(),
        ),
        Tool::new(
            "database_stats",
            "Get a holistic database statistics snapshot in one call (no arguments). Use it \
             to orient yourself before querying: how big is the dataset, how much history \
             exists to time-travel through, where that history lives, and what durability \
             is active. Returns: `current` {node_count, edge_count} (current-state graph \
             size); `historical` {total_node_versions, total_edge_versions, unique_nodes, \
             unique_edges, anchor_count, delta_count, node/edge anchor+delta breakdowns, \
             compression_ratio} (bi-temporal depth of the in-RAM store; anchors are full \
             snapshots, deltas are changes — anchor_count + delta_count always equals total \
             versions, and compression_ratio = anchors/total, lower is better); \
             `cold_storage` — `{enabled: false}` when no cold (disk) tier is configured \
             (this means NOT CONFIGURED, never '0 cold versions'), or `{enabled: true, \
             node_versions_stored, edge_versions_stored, compression_ratio, tier_access: \
             {hot_hits, warm_hits, cold_hits, misses}}` showing how many versions live on \
             disk (counts persist across restarts) and how reads distribute across the \
             hot/warm-cache/cold tiers (compression_ratio and tier_access count activity \
             since the current process opened the database); `wal` {enabled, \
             durability_mode (synchronous|async|group_commit|async_batched), current_lsn \
             (the NEXT log sequence number to be allocated), total_appends, healthy}. All \
             values are O(1)/cached counter reads — this call never scans versions and is \
             safe to call frequently. Counts only, point-in-time: for the calendar RANGE \
             of stored history (earliest/latest timestamps) use temporal_extent; \
             for per-label breakdowns use get_schema.",
            make_input_schema::<DatabaseStatsRequest>(),
        ),
        Tool::new(
            "verify_chain",
            "Verify the tamper-evident provenance hash chain (Issue #3351) — proof that the \
             recorded bi-temporal history has not been altered. Three modes: (1) FULL (no \
             arguments) walks the whole chain from genesis and, on tamper, reports the \
             `earliest_broken_seq`; (2) ENTITY-SCOPED (pass `entity_kind` 'node'|'edge' and \
             `id`) recomputes only that entity's contribution; (3) ANCHOR EXTENSION (pass \
             `against`, a previously exported chain head from export_chain_head) proves the \
             current chain append-only-extends that anchor, detecting rollback (truncation) \
             and fork (divergence). Read-only. Returns {scope, passed, head_seq, head_digest, \
             earliest_broken_seq, reason, transactions_checked}. Requires the chain to be \
             enabled for this database; if it is not, returns a FAILED_PRECONDITION error \
             (never a silent empty pass).",
            make_input_schema::<VerifyChainRequest>(),
        ),
        Tool::new(
            "export_chain_head",
            "Export the current provenance hash chain head as an external anchor (Issue \
             #3351). Store the returned checkpoint offsite; later pass it back as \
             verify_chain's `against` argument to prove the chain has only been appended to \
             (detecting rollback and fork). No arguments. Returns the chain head {seq, digest, \
             commit_ts, anchor_lsn, genesis_digest} with digests as lowercase hex. Requires \
             the chain to be enabled; otherwise returns a FAILED_PRECONDITION error.",
            make_input_schema::<ExportChainHeadRequest>(),
        ),
    ];

    // Advertise the Issue #3353 token budget on every budgetable read tool by
    // (a) injecting the three budget parameters into its generated
    // `inputSchema.properties` so they are machine-discoverable, and (b)
    // appending a uniform prose hint to its description as a secondary,
    // human-readable pointer. Both are kept in lockstep with
    // `BUDGETABLE_READ_TOOLS` (the dispatch-path source of truth) without
    // editing each tool literal.
    for tool in tools.iter_mut() {
        if is_budgetable_read_tool(&tool.name) {
            inject_budget_schema_params(&mut tool.input_schema);
            let mut desc = tool.description.as_deref().unwrap_or("").to_string();
            desc.push_str(BUDGET_TOOL_HINT);
            tool.description = Some(std::borrow::Cow::Owned(desc));
        }
    }
    tools
}

/// Inject the three optional Issue #3353 token-budget parameters into a tool's
/// generated JSON `inputSchema.properties`, so a client that introspects the
/// schema (not just the prose description) discovers them with correct types.
/// All three are optional, so `required` is deliberately left untouched. Idempotent.
fn inject_budget_schema_params(schema: &mut Arc<serde_json::Map<String, serde_json::Value>>) {
    let schema = Arc::make_mut(schema);
    let props = schema
        .entry("properties".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(props) = props.as_object_mut() else {
        return;
    };
    props
        .entry("max_response_tokens".to_string())
        .or_insert_with(|| {
            json!({
                "type": "integer",
                "minimum": 1,
                "description": "Optional (Issue #3353): cap the response at approximately this \
                    many tokens (estimated as ceil(utf8_bytes/4)). The serialized response, \
                    including its truncation metadata, is guaranteed to fit.",
            })
        });
    props
        .entry("max_response_bytes".to_string())
        .or_insert_with(|| {
            json!({
                "type": "integer",
                "minimum": 1,
                "description": "Optional (Issue #3353): byte-exact cap on the serialized response. \
                    When both this and max_response_tokens are given, the tighter bound wins.",
            })
        });
    props
        .entry("priority_properties".to_string())
        .or_insert_with(|| {
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional (Issue #3353): property keys to protect from elision at \
                    every degradation rung when a response budget is active.",
            })
        });
}

/// Uniform description suffix documenting the token-budget parameters
/// (Issue #3353), appended to every budgetable read tool.
const BUDGET_TOOL_HINT: &str = " Optional token budget (Issue #3353): pass \
    `max_response_tokens` (estimated as ceil(utf8_bytes/4)) or the byte-exact \
    `max_response_bytes` to cap the response size; the serialized response \
    (including its truncation metadata) is guaranteed to fit. Over budget, the \
    response degrades deterministically (elide bulky property values → per-entity \
    summaries → counts-plus-handles), carrying a `budget` block naming the rung \
    applied and a fetch handle at every elision site. Protect specific properties \
    with `priority_properties`. A budget too small for the minimal response \
    returns INVALID_ARGUMENT stating the minimum viable budget. Omit for \
    unchanged (row-limit) behavior.";

impl ServerHandler for AletheiaMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "AletheiaDB MCP Server - A bi-temporal graph database with vector search. \
                 Use the provided tools to query and manipulate graph data with full \
                 temporal versioning and vector similarity search capabilities.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, RmcpErrorData> {
        Ok(ListToolsResult {
            tools: tool_definitions(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, RmcpErrorData> {
        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);

        Ok(self.dispatch_tool(request.name.as_ref(), args))
    }
}

#[cfg(test)]
mod server_unit_tests {
    use std::sync::Arc;

    use super::AletheiaMcpServer;
    use crate::core::PropertyValue;
    use crate::core::error::{Error, QueryError};
    use crate::core::id::{EdgeId, NodeId};
    use crate::db::AletheiaDB;
    use crate::query::executor::{EntityResult, QueryRow};

    fn make_server() -> AletheiaMcpServer {
        AletheiaMcpServer::new(Arc::new(AletheiaDB::new().expect("db init")))
    }

    fn error_kind(server: &AletheiaMcpServer, err: Error) -> String {
        let result = server.map_query_error(err, "aql");
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        val["error"]["kind"].as_str().unwrap_or("").to_string()
    }

    /// Like [`error_kind`], but returning the whole serialized `error` object
    /// so tests can assert `code`/`retriable` alongside `kind`.
    fn error_payload(server: &AletheiaMcpServer, err: Error) -> serde_json::Value {
        let result = server.map_query_error(err, "aql");
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        val["error"].clone()
    }

    /// EXPLAIN (Issue #562) flows through the MCP `query` tool's
    /// computed-columns path unchanged: it is not a mutating clause, and its
    /// single `plan`-column row renders like any aggregate/computed row.
    #[cfg(feature = "cypher")]
    #[test]
    fn handle_query_explain_returns_plan_column() {
        let db = Arc::new(AletheiaDB::new().expect("db init"));
        db.create_node("Person", crate::core::property::PropertyMap::new())
            .unwrap();
        let server = AletheiaMcpServer::new(db);

        let result = server.handle_query(serde_json::json!({
            "language": "cypher",
            "query": "EXPLAIN MATCH (n:Person) RETURN n",
        }));
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert!(
            val.get("error").is_none(),
            "EXPLAIN of a read query must not be rejected: {val}"
        );
        assert_eq!(val["row_count"], 1, "EXPLAIN yields one row: {val}");
        let rows = val["rows"].as_array().expect("rows array");
        let plan = rows[0]["plan"]
            .as_str()
            .expect("the row carries a `plan` string column");
        assert!(
            plan.contains("NodeScan"),
            "the plan text is surfaced through MCP: {plan}"
        );
        let columns = val["columns"].as_array().expect("columns array");
        assert!(
            columns.iter().any(|c| c["name"] == "plan"),
            "column metadata names the `plan` column: {val}"
        );
    }

    #[test]
    fn map_query_error_unsupported_feature_yields_unsupported_construct() {
        let server = make_server();
        let err = Error::Query(QueryError::UnsupportedFeature {
            feature: "DISTINCT".to_string(),
        });
        assert_eq!(error_kind(&server, err), "unsupported_construct");
    }

    #[test]
    fn map_query_error_invalid_parameter_yields_invalid_params() {
        let server = make_server();
        let err = Error::Query(QueryError::InvalidParameter {
            parameter: "p".to_string(),
            reason: "out of range".to_string(),
        });
        assert_eq!(error_kind(&server, err), "invalid_params");
    }

    #[test]
    fn map_query_error_execution_error_yields_runtime_error() {
        let server = make_server();
        let err = Error::Query(QueryError::ExecutionError {
            message: "boom".to_string(),
        });
        assert_eq!(error_kind(&server, err), "runtime_error");
    }

    #[test]
    fn map_query_error_other_variant_yields_runtime_error() {
        let server = make_server();
        // Error::Other is a variant not matched by any specific arm — falls through to `other`.
        let err = Error::Other("unexpected situation".to_string());
        assert_eq!(error_kind(&server, err), "runtime_error");
    }

    #[test]
    fn map_query_error_timeout_yields_retriable_unavailable_runtime_error() {
        // A timeout keeps the query tool's own `kind` contract
        // ("runtime_error") but is classified UNAVAILABLE/retriable from the
        // underlying engine error — and `retriable: true` must survive the
        // query-tool serialization path, not just the in-memory struct.
        let server = make_server();
        let error = error_payload(
            &server,
            Error::Query(QueryError::Timeout { duration_ms: 5000 }),
        );
        assert_eq!(error["kind"], "runtime_error", "got: {error}");
        assert_eq!(error["code"], "UNAVAILABLE", "got: {error}");
        assert_eq!(error["retriable"], true, "got: {error}");
    }

    #[test]
    fn query_row_to_json_node_id_variant() {
        let server = make_server();
        let row = QueryRow::from_entity(EntityResult::NodeId(NodeId::new(42).unwrap()));
        let json = server.query_row_to_json(row);
        assert_eq!(json["entity"]["type"].as_str(), Some("node"));
        assert_eq!(json["entity"]["id"].as_u64(), Some(42));
    }

    #[test]
    fn query_row_to_json_edge_id_variant() {
        let server = make_server();
        let row = QueryRow::from_entity(EntityResult::EdgeId(EdgeId::new(99).unwrap()));
        let json = server.query_row_to_json(row);
        assert_eq!(json["entity"]["type"].as_str(), Some("edge"));
        assert_eq!(json["entity"]["id"].as_u64(), Some(99));
    }

    // --- #558 MCP surface: computed/aggregate rows render their named columns.
    // These are deliberately feature-independent (no `#[cfg(feature = "cypher")]`)
    // so the coverage job -- which compiles without `cypher` -- still exercises
    // the aggregate branches of `query_row_to_json`, `handle_query`'s column
    // selection, and `computed_query_columns`. `QueryRow::from_columns` builds
    // exactly the `entity: Null, columns: Some(..)` shape those branches key on.

    #[test]
    fn query_row_to_json_renders_computed_columns() {
        let server = make_server();
        let row = QueryRow::from_columns(vec![("count(*)".to_string(), PropertyValue::Int(5))]);
        let json = server.query_row_to_json(row);
        // The aggregate value is surfaced under its column name, not lost.
        assert_eq!(json["count(*)"].as_i64(), Some(5), "got: {json}");
        // Computed rows use the bare column-map shape: no entity/score/path/timestamp keys.
        let obj = json
            .as_object()
            .expect("computed row must be a JSON object");
        assert!(!obj.contains_key("entity"), "no entity key: {json}");
        assert!(!obj.contains_key("score"), "no score key: {json}");
        assert!(!obj.contains_key("path"), "no path key: {json}");
        assert!(!obj.contains_key("timestamp"), "no timestamp key: {json}");
    }

    #[test]
    fn query_row_to_json_renders_multi_column_computed_row() {
        let server = make_server();
        let row = QueryRow::from_columns(vec![
            ("n.dept".to_string(), PropertyValue::String("Eng".into())),
            ("c".to_string(), PropertyValue::Int(3)),
        ]);
        let json = server.query_row_to_json(row);
        assert_eq!(json["n.dept"].as_str(), Some("Eng"), "got: {json}");
        assert_eq!(json["c"].as_i64(), Some(3), "got: {json}");
    }

    #[test]
    fn computed_query_columns_names_the_columns() {
        let names = vec!["count(*)".to_string(), "c".to_string()];
        let cols = super::computed_query_columns(&names);
        let arr = cols.as_array().expect("columns must be a JSON array");
        assert_eq!(arr.len(), 2, "one entry per column, in order: {cols}");
        assert_eq!(arr[0]["name"].as_str(), Some("count(*)"), "got: {cols}");
        assert_eq!(arr[1]["name"].as_str(), Some("c"), "got: {cols}");
        // Each entry carries the response schema shape (name/type/description).
        assert!(arr[0].get("type").is_some(), "type present: {cols}");
        assert!(
            arr[0].get("description").is_some(),
            "description present: {cols}"
        );
    }

    #[test]
    fn handle_enable_unique_constraint_invalid_json_returns_error() {
        // Covers the `Err(e) => return self.invalid_argument(...)` parse-error arm of
        // handle_enable_unique_constraint (added for Issue #3218).  The public
        // `enable_unique_constraint(req)` API always serialises a valid struct,
        // so this arm is only reachable via the internal handle_ function.
        let server = make_server();
        let result = server.handle_enable_unique_constraint(serde_json::Value::Null);
        // Must be an error CallToolResult (is_error = Some(true))
        assert!(
            result.is_error.unwrap_or(false),
            "Null JSON input must produce an error result"
        );
    }

    #[test]
    fn timestamp_to_rfc3339_out_of_chrono_range_falls_back_to_micros() {
        // Coordinates outside chrono's representable range must render as
        // the raw-microseconds fallback instead of panicking. Timestamps up
        // to MAX_VALID_TIMESTAMP (i64::MAX - 1000 µs) are storable but far
        // beyond chrono's ~year-262143 ceiling.
        let ts = crate::core::temporal::Timestamp::from(i64::MAX - 1000);
        let rendered = AletheiaMcpServer::timestamp_to_rfc3339(ts);
        assert_eq!(rendered, format!("{}us", i64::MAX - 1000));

        // Sanity: an in-range coordinate renders as RFC3339, not the fallback.
        let ts = crate::core::temporal::Timestamp::from(1_614_556_800_000_000); // 2021-03-01
        let rendered = AletheiaMcpServer::timestamp_to_rfc3339(ts);
        assert_eq!(rendered, "2021-03-01T00:00:00.000000Z");
    }

    #[test]
    fn handle_temporal_extent_invalid_by_label_type_routes_through_invalid_argument() {
        // A mistyped argument (string instead of bool) must produce the
        // structured Issue #3234 error payload with code INVALID_ARGUMENT
        // (Issue #3238).
        let server = make_server();
        let result = server.handle_temporal_extent(serde_json::json!({"by_label": "yes"}));
        assert!(
            result.is_error.unwrap_or(false),
            "mistyped by_label must produce an error result"
        );
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            val["error"]["code"], "INVALID_ARGUMENT",
            "mistyped by_label must classify as INVALID_ARGUMENT: {val}"
        );
        assert_eq!(
            val["error"]["retriable"], false,
            "INVALID_ARGUMENT must not be retriable: {val}"
        );
        assert!(
            val["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("Invalid arguments")),
            "message must preserve the free-text detail: {val}"
        );
    }

    #[test]
    fn handle_temporal_extent_null_args_behaves_like_no_arguments() {
        // The tool has no required arguments: an MCP call with the
        // `arguments` object omitted entirely (routed here as Null) must
        // succeed exactly like `{}`.
        let server = make_server();
        let result = server.handle_temporal_extent(serde_json::Value::Null);
        assert!(
            !result.is_error.unwrap_or(false),
            "temporal_extent with no arguments must succeed"
        );
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(val["valid_time"]["earliest"].is_null());
        assert!(val["transaction_time"]["latest"].is_null());
    }

    #[test]
    fn query_row_to_json_null_binding_serializes_entity_as_json_null() {
        // A null binding from an unmatched OPTIONAL MATCH pattern must
        // surface as an explicit JSON null entity (row preserved).
        let server = make_server();
        let value = server.query_row_to_json(QueryRow::from_entity(EntityResult::Null));
        assert!(
            value["entity"].is_null(),
            "null binding must serialize as JSON null: {value}"
        );
        assert!(value["score"].is_null());
        assert!(value["path"].is_null());
    }

    // --- #549 MCP surface: multi-variable binding rows serialize each bound
    // entity under its variable name (never a lossy `entity: null`).

    #[test]
    fn query_row_to_json_bindings_serializes_each_entity() {
        use crate::core::graph::Node;
        use crate::core::id::VersionId;
        use crate::core::{GLOBAL_INTERNER, PropertyMapBuilder};

        let server = make_server();
        let mk = |id: u64, name: &str| {
            let label = GLOBAL_INTERNER.intern("Person").unwrap();
            Node::new(
                NodeId::new(id).unwrap(),
                label,
                PropertyMapBuilder::new().insert("name", name).build(),
                VersionId::new(1).unwrap(),
            )
        };
        let row = QueryRow::from_bindings(
            vec![
                ("a".to_string(), EntityResult::Node(mk(1, "Alice"))),
                ("b".to_string(), EntityResult::Node(mk(2, "Bob"))),
            ],
            None,
        );
        let json = server.query_row_to_json(row);
        let obj = json
            .as_object()
            .expect("bindings row must be a JSON object");
        // Both variables surface as node objects, NOT a lossy null.
        assert_eq!(obj["a"]["type"].as_str(), Some("node"), "got: {json}");
        assert_eq!(obj["a"]["id"].as_u64(), Some(1), "got: {json}");
        assert_eq!(
            obj["a"]["properties"]["name"].as_str(),
            Some("Alice"),
            "got: {json}"
        );
        assert_eq!(obj["b"]["type"].as_str(), Some("node"), "got: {json}");
        assert_eq!(obj["b"]["id"].as_u64(), Some(2), "got: {json}");
    }

    #[test]
    fn query_row_to_json_bindings_merged_with_columns() {
        use crate::core::graph::Node;
        use crate::core::id::VersionId;
        use crate::core::{GLOBAL_INTERNER, PropertyMapBuilder};

        let server = make_server();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let node = Node::new(
            NodeId::new(7).unwrap(),
            label,
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            VersionId::new(1).unwrap(),
        );
        let row = QueryRow::from_bindings(
            vec![("a".to_string(), EntityResult::Node(node))],
            Some(vec![(
                "a.name".to_string(),
                PropertyValue::String("Alice".into()),
            )]),
        );
        let json = server.query_row_to_json(row);
        // The bound entity and the scalar column are both present, merged.
        assert_eq!(json["a"]["type"].as_str(), Some("node"), "got: {json}");
        assert_eq!(json["a.name"].as_str(), Some("Alice"), "got: {json}");
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn handle_query_multi_pattern_returns_non_null_bindings() {
        use crate::core::PropertyMapBuilder;
        let server = make_server();
        server
            .db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        server
            .db
            .create_node(
                "Company",
                PropertyMapBuilder::new().insert("name", "Acme").build(),
            )
            .unwrap();
        let result = server.handle_query(serde_json::json!({
            "language": "cypher",
            "query": "MATCH (a:Person),(b:Company) RETURN a,b"
        }));
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        let rows = val["rows"].as_array().expect("rows array");
        assert_eq!(rows.len(), 1, "got: {val}");
        assert_eq!(rows[0]["a"]["type"].as_str(), Some("node"), "got: {val}");
        assert_eq!(rows[0]["b"]["type"].as_str(), Some("node"), "got: {val}");
        // The response columns are derived dynamically from the binding names.
        let cols: Vec<String> = val["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        assert!(
            cols.contains(&"a".to_string()) && cols.contains(&"b".to_string()),
            "columns must name the bound variables: {val}"
        );
    }
}
