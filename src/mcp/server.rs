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
//! | **Temporal** | `get_node_at_time`, `get_node_history` | Time-travel queries and history |
//! | **Hybrid** | `hybrid_query` | Combined graph + vector + temporal queries |
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
    ErrorData as McpError, ServerHandler,
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
    PropertyValue, Timestamp,
};
use crate::db::AletheiaDB;
use crate::index::vector::{DistanceMetric, HnswConfig};
use crate::query::executor::{EntityId as ResultEntityId, EntityResult};

use super::tools::*;

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
}

impl AletheiaMcpServer {
    /// Create a new MCP server wrapping a AletheiaDB instance.
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
        Self { db }
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
    fn extract_text(result: CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text().map(|s| s.text.clone()))
            .unwrap_or_else(|| r#"{"error": "No content in response"}"#.to_string())
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
    /// Returns a JSON object with an "error" key if the node is not found.
    /// ```json
    /// { "error": "Node not found: 123" }
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
    /// structured `{error:{kind,message,clause?,language}}` payload. Mutating
    /// statements are rejected before execution and never write.
    pub fn query(&self, req: QueryRequest) -> String {
        Self::extract_text(self.handle_query(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    // ========================================================================
    // Helper methods for converting between AletheiaDB and MCP types
    // ========================================================================

    fn interned_to_string(&self, interned: crate::core::InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(interned, |s| s.to_string())
            .unwrap_or_else(|| format!("<unknown:{}>", interned.as_u32()))
    }

    fn node_to_response(&self, node: &crate::core::Node) -> NodeResponse {
        NodeResponse {
            id: node.id.as_u64(),
            label: self.interned_to_string(node.label),
            properties: self.property_map_to_json(&node.properties),
        }
    }

    fn edge_to_response(&self, edge: &crate::core::Edge) -> EdgeResponse {
        EdgeResponse {
            id: edge.id.as_u64(),
            source_id: edge.source.as_u64(),
            target_id: edge.target.as_u64(),
            label: self.interned_to_string(edge.label),
            properties: self.property_map_to_json(&edge.properties),
        }
    }

    fn property_map_to_json(&self, props: &PropertyMap) -> HashMap<String, serde_json::Value> {
        let mut result = HashMap::new();
        for (key, value) in props.iter() {
            let key_str = self.interned_to_string(*key);
            result.insert(key_str, self.property_value_to_json(value));
        }
        result
    }

    fn property_value_to_json(&self, value: &PropertyValue) -> serde_json::Value {
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
                arr.iter().map(|v| self.property_value_to_json(v)).collect(),
            ),
            PropertyValue::Vector(v) => {
                serde_json::Value::Array(v.iter().map(|f| json!(*f)).collect())
            }
            PropertyValue::SparseVector(sv) => {
                json!({
                    "indices": sv.indices(),
                    "values": sv.values()
                })
            }
        }
    }

    fn json_to_property_map(
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

    fn parse_timestamp(&self, s: &str) -> Result<Timestamp, String> {
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

    /// Parse an optional timestamp argument, returning an `error_json` result on a parse failure.
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
            .map_err(|e| self.error_json(&format!("Invalid {label}: {e}")))
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

    fn success_json(&self, value: serde_json::Value) -> CallToolResult {
        CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        )])
    }

    fn error_json(&self, msg: &str) -> CallToolResult {
        CallToolResult::error(vec![Content::text(json!({"error": msg}).to_string())])
    }

    /// Build a structured `CallToolResult` for a `UniqueViolation` constraint error.
    /// Returns `None` if `e` is not a `UniqueViolation`.
    fn constraint_violation_result(&self, e: &crate::core::error::Error) -> Option<CallToolResult> {
        if let Some(crate::core::error::ConstraintError::UniqueViolation {
            label,
            property,
            value,
            existing_node_id,
        }) = e.as_constraint()
        {
            Some(CallToolResult::error(vec![Content::text(
                serde_json::to_string_pretty(&json!({
                    "error": e.to_string(),
                    "success": false,
                    "constraint_violation": true,
                    "label": label,
                    "property": property,
                    "value": value,
                    "existing_node_id": existing_node_id.as_u64()
                }))
                .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string()),
            )]))
        } else {
            None
        }
    }

    // ========================================================================
    // Tool Implementations
    // ========================================================================

    fn handle_get_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        match self.db.get_node(node_id) {
            Ok(node) => {
                let response = self.node_to_response(&node);
                self.success_json(
                    serde_json::to_value(&response)
                        .expect("response serialization should not fail"),
                )
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_create_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: CreateNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let properties = match req.properties {
            Some(p) => match self.json_to_property_map(&p) {
                Ok(map) => map,
                Err(e) => return self.error_json(&format!("Invalid properties: {}", e)),
            },
            None => PropertyMap::default(),
        };

        match self.db.create_node(&req.label, properties) {
            Ok(node_id) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let response = self.node_to_response(&node);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.error_json(&e.to_string()),
            },
            Err(ref e) => {
                if let Some(result) = self.constraint_violation_result(e) {
                    return result;
                }
                self.error_json(&e.to_string())
            }
        }
    }

    fn handle_update_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: UpdateNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let properties = match self.json_to_property_map(&req.properties) {
            Ok(map) => map,
            Err(e) => return self.error_json(&format!("Invalid properties: {}", e)),
        };

        match self.db.write(|tx| tx.update_node(node_id, properties)) {
            Ok(()) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let response = self.node_to_response(&node);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.error_json(&e.to_string()),
            },
            Err(ref e) => {
                if let Some(result) = self.constraint_violation_result(e) {
                    return result;
                }
                self.error_json(&e.to_string())
            }
        }
    }

    fn handle_delete_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let detach = req.detach.unwrap_or(false);

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
                // edges, reporting exactly how many edges were removed.
                tx.delete_node_cascade(node_id)?;
                Ok(Outcome::Deleted {
                    edges_removed: connected_edges,
                })
            } else {
                // No connected edges: a plain delete cannot orphan anything.
                tx.delete_node(node_id)?;
                Ok(Outcome::Deleted { edges_removed: 0 })
            }
        });

        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => return self.error_json(&e.to_string()),
        };

        match outcome {
            Outcome::Refused { connected_edges } => CallToolResult::error(vec![Content::text(
                serde_json::to_string_pretty(&json!({
                    "error": format!(
                        "Node {} has {} connected edge(s); refusing to delete. \
                         Pass `detach: true` to delete the node and its connected edges, \
                         or remove the edges first.",
                        req.node_id, connected_edges
                    ),
                    "success": false,
                    "node_id": req.node_id,
                    "connected_edges": connected_edges,
                    "detach_required": true
                }))
                .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string()),
            )]),
            Outcome::Deleted { edges_removed } => self.success_json(json!({
                "success": true,
                "deleted_node_id": req.node_id,
                "detached": detach,
                "edges_removed": edges_removed
            })),
        }
    }

    fn handle_delete_node_cascade(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteNodeCascadeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        match self.db.write(|tx| tx.delete_node_cascade(node_id)) {
            Ok(()) => self.success_json(json!({
                "success": true,
                "deleted_node_id": req.node_id,
                "cascade": true
            })),
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_list_nodes(&self, args: serde_json::Value) -> CallToolResult {
        let req: ListNodesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0).min(MAX_PAGINATION_OFFSET);

        // Validate property filter: both key and value are required together with label
        if req.property_key.is_some() != req.property_value.is_some() {
            return self
                .error_json("Both 'property_key' and 'property_value' are required together");
        }
        if req.property_key.is_some() && req.label.is_none() {
            return self.error_json("Property filtering requires 'label' to be specified");
        }

        // Property-based lookup: label + property_key + property_value
        if let (Some(label), Some(prop_key), Some(prop_val)) =
            (&req.label, &req.property_key, &req.property_value)
        {
            let property_value =
                match self.json_to_property_value(prop_val) {
                    Some(v) => v,
                    None => return self.error_json(
                        "Unsupported property_value type. Use strings, numbers, booleans, or null.",
                    ),
                };

            let node_ids = self
                .db
                .find_nodes_by_property(label, prop_key, &property_value);

            let mut nodes = Vec::with_capacity(limit);
            for node_id in node_ids.into_iter().skip(offset).take(limit) {
                if let Ok(node) = self.db.get_node(node_id) {
                    nodes.push(self.node_to_response(&node));
                }
            }

            return self.success_json(json!({
                "nodes": nodes,
                "count": nodes.len(),
                "offset": offset,
                "limit": limit
            }));
        }

        // Label-only scan
        if let Some(label) = &req.label {
            let builder = crate::query::QueryBuilder::new().scan_label(label);

            // Note: We fetch offset+limit rows then skip offset.
            // Offset is capped to prevent excessive memory use.
            match builder.limit(limit + offset).execute(&self.db) {
                Ok(results) => {
                    // Use iterator-based approach to avoid allocating full Vec
                    let mut nodes = Vec::with_capacity(limit);
                    let mut skipped = 0;

                    for row_result in results {
                        match row_result {
                            Ok(row) => {
                                if skipped < offset {
                                    skipped += 1;
                                    continue;
                                }
                                if let EntityResult::Node(node) = row.entity {
                                    nodes.push(self.node_to_response(&node));
                                    if nodes.len() >= limit {
                                        break;
                                    }
                                }
                            }
                            Err(e) => return self.error_json(&e.to_string()),
                        }
                    }

                    self.success_json(json!({
                        "nodes": nodes,
                        "count": nodes.len(),
                        "offset": offset,
                        "limit": limit
                    }))
                }
                Err(e) => self.error_json(&e.to_string()),
            }
        } else {
            // Without a label filter, we cannot efficiently list all nodes
            // Return a helpful message
            self.success_json(json!({
                "message": "Use 'label' filter to list nodes by type, or use 'count_nodes' for total count",
                "total_count": self.db.node_count(),
                "nodes": [],
                "count": 0,
                "offset": offset,
                "limit": limit
            }))
        }
    }

    fn handle_count_nodes(&self, args: serde_json::Value) -> CallToolResult {
        let req: CountNodesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        if let Some(label) = &req.label {
            // Use QueryBuilder to count by label efficiently without collecting all rows
            let builder = crate::query::QueryBuilder::new().scan_label(label);
            match builder.execute(&self.db) {
                Ok(mut results) => {
                    // Efficiently count without allocating a Vec
                    match results.try_fold(0usize, |acc, row| row.map(|_| acc + 1)) {
                        Ok(count) => self.success_json(json!({"count": count, "label": label})),
                        Err(e) => self.error_json(&format!(
                            "Error counting nodes with label '{}': {}",
                            label, e
                        )),
                    }
                }
                Err(e) => self.error_json(&format!(
                    "Error executing count query for label '{}': {}",
                    label, e
                )),
            }
        } else {
            self.success_json(json!({"count": self.db.node_count()}))
        }
    }

    fn handle_get_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        match self.db.get_edge(edge_id) {
            Ok(edge) => {
                let response = self.edge_to_response(&edge);
                self.success_json(
                    serde_json::to_value(&response)
                        .expect("response serialization should not fail"),
                )
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_create_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: CreateEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let source_id = match NodeId::new(req.source_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&format!("Invalid source_id: {}", e)),
        };

        let target_id = match NodeId::new(req.target_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&format!("Invalid target_id: {}", e)),
        };

        let properties = match req.properties {
            Some(p) => match self.json_to_property_map(&p) {
                Ok(map) => map,
                Err(e) => return self.error_json(&format!("Invalid properties: {}", e)),
            },
            None => PropertyMap::default(),
        };

        match self
            .db
            .create_edge(source_id, target_id, &req.label, properties)
        {
            Ok(edge_id) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let response = self.edge_to_response(&edge);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.error_json(&e.to_string()),
            },
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_update_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: UpdateEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let properties = match self.json_to_property_map(&req.properties) {
            Ok(map) => map,
            Err(e) => return self.error_json(&format!("Invalid properties: {}", e)),
        };

        match self.db.write(|tx| tx.update_edge(edge_id, properties)) {
            Ok(()) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let response = self.edge_to_response(&edge);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.error_json(&e.to_string()),
            },
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_delete_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        match self.db.write(|tx| tx.delete_edge(edge_id)) {
            Ok(()) => self.success_json(json!({
                "success": true,
                "deleted_edge_id": req.edge_id
            })),
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_list_edges(&self, args: serde_json::Value) -> CallToolResult {
        let req: ListEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0);

        // Edges cannot be efficiently listed without knowing source/target nodes.
        // Provide helpful guidance to use get_outgoing_edges or get_incoming_edges.
        self.success_json(json!({
            "message": "Use 'get_outgoing_edges' or 'get_incoming_edges' from a known node to list edges",
            "total_count": self.db.edge_count(),
            "edges": [],
            "count": 0,
            "offset": offset,
            "limit": limit,
            "label_filter": req.label
        }))
    }

    fn handle_count_edges(&self, args: serde_json::Value) -> CallToolResult {
        let req: CountEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
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
        let req: GetOutgoingEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let edge_ids = if let Some(label) = &req.label {
            self.db.get_outgoing_edges_with_label(node_id, label)
        } else {
            self.db.get_outgoing_edges(node_id)
        };

        let edges: Vec<EdgeResponse> = edge_ids
            .into_iter()
            .filter_map(|eid| self.db.get_edge(eid).ok())
            .map(|e| self.edge_to_response(&e))
            .collect();

        self.success_json(json!({
            "edges": edges,
            "count": edges.len()
        }))
    }

    fn handle_get_incoming_edges(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetIncomingEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let edge_ids = self.db.get_incoming_edges(node_id);

        // Filter by label if provided
        let edges: Vec<EdgeResponse> = edge_ids
            .into_iter()
            .filter_map(|eid| self.db.get_edge(eid).ok())
            .filter(|e| {
                req.label
                    .as_ref()
                    .map(|l| self.matches_label(e.label, l))
                    .unwrap_or(true)
            })
            .map(|e| self.edge_to_response(&e))
            .collect();

        self.success_json(json!({
            "edges": edges,
            "count": edges.len()
        }))
    }

    fn handle_traverse(&self, args: serde_json::Value) -> CallToolResult {
        let req: TraverseRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let start_id = match NodeId::new(req.start_node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        // Apply resource limits to prevent DoS
        let depth = req.depth.unwrap_or(1).min(MAX_TRAVERSAL_DEPTH);
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let direction = req.direction.as_deref().unwrap_or("outgoing");

        // Use depth-first search (DFS) traversal.
        // DFS is chosen for memory efficiency: it processes nodes immediately rather than
        // queuing all nodes at each level. For large graphs with high branching factors,
        // this significantly reduces peak memory usage compared to BFS.
        let mut results: Vec<TraversalResult> = Vec::new();
        let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut frontier: Vec<(NodeId, Vec<u64>, usize)> =
            vec![(start_id, vec![start_id.as_u64()], 0)];

        while let Some((current_id, path, current_depth)) = frontier.pop() {
            if current_depth > 0 && !visited.contains(&current_id.as_u64()) {
                visited.insert(current_id.as_u64());
                if let Ok(node) = self.db.get_node(current_id) {
                    results.push(TraversalResult {
                        node: self.node_to_response(&node),
                        path: path.clone(),
                        depth: current_depth,
                    });
                    if results.len() >= limit {
                        break;
                    }
                }
            }

            if current_depth < depth {
                let edge_ids: Vec<EdgeId> = match direction {
                    "incoming" => {
                        // Filter incoming edges by label manually
                        self.db
                            .get_incoming_edges(current_id)
                            .into_iter()
                            .filter(|eid| {
                                self.db
                                    .get_edge(*eid)
                                    .map(|e| self.matches_label(e.label, &req.edge_label))
                                    .unwrap_or(false)
                            })
                            .collect()
                    }
                    "both" => {
                        let mut edges = self
                            .db
                            .get_outgoing_edges_with_label(current_id, &req.edge_label);
                        let incoming: Vec<EdgeId> = self
                            .db
                            .get_incoming_edges(current_id)
                            .into_iter()
                            .filter(|eid| {
                                self.db
                                    .get_edge(*eid)
                                    .map(|e| self.matches_label(e.label, &req.edge_label))
                                    .unwrap_or(false)
                            })
                            .collect();
                        edges.extend(incoming);
                        edges
                    }
                    _ => self
                        .db
                        .get_outgoing_edges_with_label(current_id, &req.edge_label),
                };

                for edge_id in edge_ids {
                    if let Ok(edge) = self.db.get_edge(edge_id) {
                        let next_id = match direction {
                            "incoming" => edge.source,
                            _ => edge.target,
                        };
                        if !visited.contains(&next_id.as_u64()) {
                            let mut new_path = path.clone();
                            new_path.push(next_id.as_u64());
                            frontier.push((next_id, new_path, current_depth + 1));
                        }
                    }
                }
            }
        }

        self.success_json(json!({
            "results": results,
            "count": results.len()
        }))
    }

    fn handle_find_similar(&self, args: serde_json::Value) -> CallToolResult {
        let req: FindSimilarRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits
        let k = req.k.unwrap_or(DEFAULT_VECTOR_K).min(MAX_VECTOR_K);

        if !self.db.is_vector_index_enabled_for(&req.property_name) {
            return self.error_json(&format!(
                "Vector index not enabled for property '{}'. Use enable_vector_index first.",
                req.property_name
            ));
        }

        // Validate embedding dimensions
        if let Err(e) = self.validate_embedding_dimensions(&req.embedding, &req.property_name) {
            return self.error_json(&e);
        }

        match self
            .db
            .similarity_search(crate::SimilarityQuery::from_embedding(req.embedding).k(k))
        {
            Ok(results) => {
                let similarity_results: Vec<SimilarityResult> = results
                    .into_iter()
                    .filter_map(|(node_id, score)| {
                        self.db.get_node(node_id).ok().map(|node| SimilarityResult {
                            node: self.node_to_response(&node),
                            score,
                        })
                    })
                    .collect();

                self.success_json(json!({
                    "results": similarity_results,
                    "count": similarity_results.len()
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_enable_vector_index(&self, args: serde_json::Value) -> CallToolResult {
        let req: EnableVectorIndexRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
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
            Err(e) => self.error_json(&e.to_string()),
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
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
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
            Err(e) => self.error_json(&e.to_string()),
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
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        match self.db.get_node_at_time(node_id, valid_time, tx_time) {
            Ok(node) => {
                let response = self.node_to_response(&node);
                self.success_json(json!({
                    "node": response,
                    "valid_time": req.valid_time,
                    "transaction_time": Self::format_tx_time_response(req.transaction_time)
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_get_edge_at_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        match self.db.get_edge_at_time(edge_id, valid_time, tx_time) {
            Ok(edge) => {
                let response = self.edge_to_response(&edge);
                self.success_json(json!({
                    "edge": response,
                    "valid_time": req.valid_time,
                    "transaction_time": Self::format_tx_time_response(req.transaction_time)
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_list_changes(&self, args: serde_json::Value) -> CallToolResult {
        let req: ListChangesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let tx_from = match self.parse_timestamp(&req.tx_from) {
            Ok(t) => t,
            Err(e) => return self.error_json(&format!("Invalid tx_from: {}", e)),
        };
        let tx_to = match self.parse_timestamp(&req.tx_to) {
            Ok(t) => t,
            Err(e) => return self.error_json(&format!("Invalid tx_to: {}", e)),
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
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    // ============================================================================
    // Phase 11: Independent Dimension Temporal Queries & Version History
    // ============================================================================

    fn handle_get_node_at_valid_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeAtValidTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        match self.db.get_node_at_valid_time(node_id, valid_time) {
            Ok(node) => {
                let response = self.node_to_response(&node);
                self.success_json(json!({
                    "node": response,
                    "valid_time": req.valid_time
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_get_node_at_transaction_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeAtTransactionTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let tx_time = match self.parse_timestamp(&req.transaction_time) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        match self.db.get_node_at_transaction_time(node_id, tx_time) {
            Ok(node) => {
                let response = self.node_to_response(&node);
                self.success_json(json!({
                    "node": response,
                    "transaction_time": req.transaction_time
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_get_node_history(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeHistoryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
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
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_diff_node_versions(&self, args: serde_json::Value) -> CallToolResult {
        let req: DiffNodeVersionsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let from_version = match crate::core::id::VersionId::new(req.from_version) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let to_version = match crate::core::id::VersionId::new(req.to_version) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        match self
            .db
            .diff_node_versions(node_id, from_version, to_version)
        {
            Ok(diff) => {
                let response = self.version_diff_to_response(&diff);
                self.success_json(json!(response))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_get_edge_at_valid_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtValidTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        match self.db.get_edge_at_valid_time(edge_id, valid_time) {
            Ok(edge) => {
                let response = self.edge_to_response(&edge);
                self.success_json(json!({
                    "edge": response,
                    "valid_time": req.valid_time
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_get_edge_at_transaction_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtTransactionTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let tx_time = match self.parse_timestamp(&req.transaction_time) {
            Ok(t) => t,
            Err(e) => return self.error_json(&e),
        };

        match self.db.get_edge_at_transaction_time(edge_id, tx_time) {
            Ok(edge) => {
                let response = self.edge_to_response(&edge);
                self.success_json(json!({
                    "edge": response,
                    "transaction_time": req.transaction_time
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_get_edge_history(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeHistoryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
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
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_diff_edge_versions(&self, args: serde_json::Value) -> CallToolResult {
        let req: DiffEdgeVersionsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let from_version = match crate::core::id::VersionId::new(req.from_version) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        let to_version = match crate::core::id::VersionId::new(req.to_version) {
            Ok(id) => id,
            Err(e) => return self.error_json(&e.to_string()),
        };

        match self
            .db
            .diff_edge_versions(edge_id, from_version, to_version)
        {
            Ok(diff) => {
                let response = self.version_diff_to_response(&diff);
                self.success_json(json!(response))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    // Helper methods for converting internal types to response types

    fn version_info_to_response(&self, info: &crate::query::VersionInfo) -> serde_json::Value {
        use crate::core::temporal::TIMESTAMP_MAX;

        let properties = self.property_map_to_json(&info.properties);

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

        json!({
            "version_number": info.version_number,
            "version_id": info.version_id.as_u64(),
            "valid_from": info.temporal.valid_time().start().wallclock().to_string(),
            "valid_to": valid_to,
            "transaction_from": info.temporal.transaction_time().start().wallclock().to_string(),
            "transaction_to": transaction_to,
            "properties": properties,
            "label": info.label
        })
    }

    fn version_diff_to_response(&self, diff: &crate::query::VersionDiff) -> serde_json::Value {
        let added = self.property_map_to_json(&diff.added);
        let removed = self.property_map_to_json(&diff.removed);

        let modified: Vec<_> = diff
            .modified
            .iter()
            .map(|(key, old_val, new_val)| {
                let key_str = GLOBAL_INTERNER
                    .resolve_with(*key, |s| s.to_string())
                    .unwrap_or_else(|| format!("{:?}", key));

                json!({
                    "key": key_str,
                    "old_value": self.property_value_to_json(old_val),
                    "new_value": self.property_value_to_json(new_val)
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

    fn handle_hybrid_query(&self, args: serde_json::Value) -> CallToolResult {
        let req: HybridQueryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
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
                Err(e) => return self.error_json(&format!("Invalid valid_time: {}", e)),
            }
        } else {
            None
        };

        let tx_time = if let Some(ref tt) = req.transaction_time {
            match self.parse_timestamp(tt) {
                Ok(t) => Some(t),
                Err(e) => return self.error_json(&format!("Invalid transaction_time: {}", e)),
            }
        } else {
            None
        };

        // Helper to convert rows to hybrid results with temporal info
        let rows_to_results =
            |rows: Vec<crate::query::executor::QueryRow>| -> Vec<HybridQueryResult> {
                rows.into_iter()
                    .filter_map(|row| {
                        if let EntityResult::Node(node) = row.entity {
                            Some(HybridQueryResult {
                                node: self.node_to_response(&node),
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
                Err(e) => return self.error_json(&e.to_string()),
            };

            // If temporal filtering requested, use temporal query
            if let (Some(vt), Some(tt)) = (valid_time, tx_time) {
                // Temporal query for a single node
                return match self.db.get_node_at_time(node_id, vt, tt) {
                    Ok(node) => {
                        let response = self.node_to_response(&node);
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
                    Err(e) => self.error_json(&e.to_string()),
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
                        let response = self.node_to_response(&node);
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
                    Err(e) => self.error_json(&e.to_string()),
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
                    Err(e) => self.error_json(&e.to_string()),
                },
                Err(e) => self.error_json(&e.to_string()),
            }
        } else if let Some(ref embedding) = req.query_embedding {
            // Vector-first query
            // Use vector_property if specified
            let property_name = req.vector_property.as_deref().unwrap_or("embedding");

            // Check if vector index is enabled for the property
            if !self.db.is_vector_index_enabled_for(property_name) {
                return self.error_json(&format!(
                    "Vector index not enabled for property '{}'. Use enable_vector_index first.",
                    property_name
                ));
            }

            // Validate embedding dimensions
            if let Err(e) = self.validate_embedding_dimensions(embedding, property_name) {
                return self.error_json(&e);
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
                    Err(e) => self.error_json(&e.to_string()),
                },
                Err(e) => self.error_json(&e.to_string()),
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
                    Err(e) => self.error_json(&e.to_string()),
                },
                Err(e) => self.error_json(&e.to_string()),
            }
        } else {
            self.error_json("Must specify either start_node_id, query_embedding, or filter_label")
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
    fn query_error(
        &self,
        kind: &str,
        message: &str,
        clause: Option<&str>,
        language: Option<&str>,
    ) -> CallToolResult {
        let mut obj = serde_json::Map::new();
        obj.insert("kind".to_string(), json!(kind));
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
            other => self.query_error("runtime_error", &other.to_string(), None, Some(language)),
        }
    }

    /// Serialize a single query row (entity + score/path/timestamp) to JSON.
    fn query_row_to_json(&self, row: crate::query::executor::QueryRow) -> serde_json::Value {
        let entity = match row.entity {
            EntityResult::Node(node) => {
                let r = self.node_to_response(&node);
                json!({"type": "node", "id": r.id, "label": r.label, "properties": r.properties})
            }
            EntityResult::Edge(edge) => {
                let r = self.edge_to_response(&edge);
                json!({
                    "type": "edge",
                    "id": r.id,
                    "source_id": r.source_id,
                    "target_id": r.target_id,
                    "label": r.label,
                    "properties": r.properties,
                })
            }
            EntityResult::NodeId(id) => json!({"type": "node", "id": id.as_u64()}),
            EntityResult::EdgeId(id) => json!({"type": "edge", "id": id.as_u64()}),
        };
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
        let rows: Vec<serde_json::Value> = collected
            .into_iter()
            .take(limit)
            .map(|row| self.query_row_to_json(row))
            .collect();
        let row_count = rows.len();

        self.success_json(json!({
            "language": language,
            "columns": query_columns(),
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

/// Clauses that would mutate state. The read-only `query` tool rejects any
/// statement containing one of these (as a whole token) before execution.
const MUTATING_KEYWORDS: &[&str] = &[
    "CREATE", "MERGE", "SET", "DELETE", "REMOVE", "DETACH", "DROP", "CALL", "FOREACH", "LOAD",
];

/// Scan a query string for a mutating clause, ignoring string-literal contents
/// (so `{name: 'DELETE'}` does not trip the guard), single-line `//` comments,
/// node labels (`:CALL`), and property keys (`n.set`). Returns the offending
/// keyword so the error can name it.
fn detect_mutating_clause(query: &str) -> Option<&'static str> {
    // First pass: strip string literals (single and double quoted, with backslash
    // escapes) and single-line comments (//) into a sanitized string.
    let mut sanitized = String::with_capacity(query.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut chars = query.chars().peekable();
    while let Some(c) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(q) => {
                if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                // Inside a string literal — don't emit characters.
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if c == '/' && chars.peek() == Some(&'/') {
                    // Single-line comment: skip to end of line.
                    for next in chars.by_ref() {
                        if next == '\n' {
                            break;
                        }
                    }
                } else {
                    sanitized.push(c);
                }
            }
        }
    }

    // Second pass: tokenise and match, but skip tokens immediately preceded by
    // ':' or '.' so that node labels (`:CALL`) and property keys (`n.set`) do
    // not trigger a false positive.
    let mut last_non_ws: Option<char> = None;
    let mut current_token = String::new();

    for c in sanitized.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' {
            current_token.push(c);
        } else {
            if !current_token.is_empty() {
                let preceded_by_label_or_prop =
                    last_non_ws == Some(':') || last_non_ws == Some('.');
                if !preceded_by_label_or_prop
                    && let Some(kw) = MUTATING_KEYWORDS
                        .iter()
                        .copied()
                        .find(|kw| kw.eq_ignore_ascii_case(&current_token))
                {
                    return Some(kw);
                }
                current_token.clear();
            }
            if !c.is_whitespace() {
                last_non_ws = Some(c);
            }
        }
    }

    None
}

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

/// Build the list of tool definitions advertised by this MCP server.
///
/// Shared between [`ServerHandler::list_tools`] and test helpers so the advertised tool
/// set and the `call_tool` dispatch table cannot silently drift apart.
fn tool_definitions() -> Vec<Tool> {
    vec![
        Tool::new(
            "get_node",
            "Get a node by its ID. Returns the node's label and properties.",
            make_input_schema::<GetNodeRequest>(),
        ),
        Tool::new(
            "create_node",
            "Create a new node with a label and optional properties.",
            make_input_schema::<CreateNodeRequest>(),
        ),
        Tool::new(
            "update_node",
            "Update an existing node's properties.",
            make_input_schema::<UpdateNodeRequest>(),
        ),
        Tool::new(
            "delete_node",
            "Delete a node by its ID (safe-by-default). If the node has connected \
                     edges and `detach` is not true, the deletion is refused and the response \
                     reports `connected_edges`. Pass `detach: true` to delete the node together \
                     with all connected edges; the response then reports `edges_removed`.",
            make_input_schema::<DeleteNodeRequest>(),
        ),
        Tool::new(
            "delete_node_cascade",
            "Delete a node and all its connected edges (cascade delete). \
                     This maintains referential integrity by removing orphaned edges.",
            make_input_schema::<DeleteNodeCascadeRequest>(),
        ),
        Tool::new(
            "list_nodes",
            "List nodes with optional label filter and pagination.",
            make_input_schema::<ListNodesRequest>(),
        ),
        Tool::new(
            "count_nodes",
            "Count the total number of nodes.",
            make_input_schema::<CountNodesRequest>(),
        ),
        Tool::new(
            "get_edge",
            "Get an edge by its ID.",
            make_input_schema::<GetEdgeRequest>(),
        ),
        Tool::new(
            "create_edge",
            "Create a new edge between two nodes.",
            make_input_schema::<CreateEdgeRequest>(),
        ),
        Tool::new(
            "update_edge",
            "Update an existing edge's properties.",
            make_input_schema::<UpdateEdgeRequest>(),
        ),
        Tool::new(
            "delete_edge",
            "Delete an edge by its ID.",
            make_input_schema::<DeleteEdgeRequest>(),
        ),
        Tool::new(
            "list_edges",
            "List edges with optional label filter and pagination.",
            make_input_schema::<ListEdgesRequest>(),
        ),
        Tool::new(
            "count_edges",
            "Count the total number of edges.",
            make_input_schema::<CountEdgesRequest>(),
        ),
        Tool::new(
            "get_outgoing_edges",
            "Get all outgoing edges from a node.",
            make_input_schema::<GetOutgoingEdgesRequest>(),
        ),
        Tool::new(
            "get_incoming_edges",
            "Get all incoming edges to a node.",
            make_input_schema::<GetIncomingEdgesRequest>(),
        ),
        Tool::new(
            "traverse",
            "Traverse the graph starting from a node.",
            make_input_schema::<TraverseRequest>(),
        ),
        Tool::new(
            "find_similar",
            "Find nodes similar to a query embedding.",
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
            "Execute a hybrid query combining graph traversal, vector similarity, and temporal filtering.",
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
             structured {error:{kind,message,clause?,language}} payload \
             (kinds: invalid_request, read_only_violation, language_unavailable, parse_error, \
             unsupported_construct, invalid_params, runtime_error) so callers can self-correct.",
            make_input_schema::<QueryRequest>(),
        ),
    ]
}

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
    ) -> Result<ListToolsResult, McpError> {
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
    ) -> Result<CallToolResult, McpError> {
        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);

        let result = match request.name.as_ref() {
            "get_node" => self.handle_get_node(args),
            "create_node" => self.handle_create_node(args),
            "update_node" => self.handle_update_node(args),
            "delete_node" => self.handle_delete_node(args),
            "delete_node_cascade" => self.handle_delete_node_cascade(args),
            "list_nodes" => self.handle_list_nodes(args),
            "count_nodes" => self.handle_count_nodes(args),
            "get_edge" => self.handle_get_edge(args),
            "create_edge" => self.handle_create_edge(args),
            "update_edge" => self.handle_update_edge(args),
            "delete_edge" => self.handle_delete_edge(args),
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
            _ => self.error_json(&format!("Unknown tool: {}", request.name)),
        };

        Ok(result)
    }
}

#[cfg(test)]
mod server_unit_tests {
    use std::sync::Arc;

    use super::AletheiaMcpServer;
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
}
