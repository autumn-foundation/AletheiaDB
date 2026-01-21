//! GallifreyDB MCP Server implementation.
//!
//! This module implements the MCP server that exposes GallifreyDB functionality
//! through the Model Context Protocol.

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CallToolRequestParam, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParam, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
};
use serde_json::json;

use crate::api::transaction::WriteOps;
use crate::core::temporal::time;
use crate::core::{
    EdgeId, GLOBAL_INTERNER, NodeId, PropertyMap, PropertyMapBuilder, PropertyValue, Timestamp,
};
use crate::db::GallifreyDB;
use crate::index::vector::{DistanceMetric, HnswConfig};
use crate::query::executor::{EntityId as ResultEntityId, EntityResult};

use super::tools::*;

/// GallifreyDB MCP Server.
///
/// Exposes GallifreyDB's graph, vector, and temporal capabilities through MCP.
#[derive(Clone)]
pub struct GallifreyMcpServer {
    db: Arc<GallifreyDB>,
}

impl GallifreyMcpServer {
    /// Create a new MCP server wrapping a GallifreyDB instance.
    pub fn new(db: Arc<GallifreyDB>) -> Self {
        Self { db }
    }

    /// Get a reference to the underlying database.
    pub fn db(&self) -> &Arc<GallifreyDB> {
        &self.db
    }

    // ========================================================================
    // Public Tool Methods (for testing and direct access)
    // ========================================================================

    /// Extract text content from a CallToolResult.
    fn extract_text(result: CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text().map(|s| s.text.clone()))
            .unwrap_or_default()
    }

    /// Get a node by its ID.
    pub fn get_node(&self, req: GetNodeRequest) -> String {
        Self::extract_text(self.handle_get_node(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Create a new node.
    pub fn create_node(&self, req: CreateNodeRequest) -> String {
        Self::extract_text(self.handle_create_node(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Update a node's properties.
    pub fn update_node(&self, req: UpdateNodeRequest) -> String {
        Self::extract_text(self.handle_update_node(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Delete a node.
    pub fn delete_node(&self, req: DeleteNodeRequest) -> String {
        Self::extract_text(self.handle_delete_node(serde_json::to_value(req).unwrap_or_default()))
    }

    /// List nodes with optional filtering.
    pub fn list_nodes(&self, req: ListNodesRequest) -> String {
        Self::extract_text(self.handle_list_nodes(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Count nodes.
    pub fn count_nodes(&self, req: CountNodesRequest) -> String {
        Self::extract_text(self.handle_count_nodes(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Get an edge by its ID.
    pub fn get_edge(&self, req: GetEdgeRequest) -> String {
        Self::extract_text(self.handle_get_edge(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Create a new edge.
    pub fn create_edge(&self, req: CreateEdgeRequest) -> String {
        Self::extract_text(self.handle_create_edge(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Update an edge's properties.
    pub fn update_edge(&self, req: UpdateEdgeRequest) -> String {
        Self::extract_text(self.handle_update_edge(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Delete an edge.
    pub fn delete_edge(&self, req: DeleteEdgeRequest) -> String {
        Self::extract_text(self.handle_delete_edge(serde_json::to_value(req).unwrap_or_default()))
    }

    /// List edges.
    pub fn list_edges(&self, req: ListEdgesRequest) -> String {
        Self::extract_text(self.handle_list_edges(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Count edges.
    pub fn count_edges(&self, req: CountEdgesRequest) -> String {
        Self::extract_text(self.handle_count_edges(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Get outgoing edges from a node.
    pub fn get_outgoing_edges(&self, req: GetOutgoingEdgesRequest) -> String {
        Self::extract_text(
            self.handle_get_outgoing_edges(serde_json::to_value(req).unwrap_or_default()),
        )
    }

    /// Get incoming edges to a node.
    pub fn get_incoming_edges(&self, req: GetIncomingEdgesRequest) -> String {
        Self::extract_text(
            self.handle_get_incoming_edges(serde_json::to_value(req).unwrap_or_default()),
        )
    }

    /// Traverse the graph.
    pub fn traverse(&self, req: TraverseRequest) -> String {
        Self::extract_text(self.handle_traverse(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Find similar nodes.
    pub fn find_similar(&self, req: FindSimilarRequest) -> String {
        Self::extract_text(self.handle_find_similar(serde_json::to_value(req).unwrap_or_default()))
    }

    /// Enable vector index.
    pub fn enable_vector_index(&self, req: EnableVectorIndexRequest) -> String {
        Self::extract_text(
            self.handle_enable_vector_index(serde_json::to_value(req).unwrap_or_default()),
        )
    }

    /// List vector indexes.
    pub fn list_vector_indexes(&self, req: ListVectorIndexesRequest) -> String {
        Self::extract_text(
            self.handle_list_vector_indexes(serde_json::to_value(req).unwrap_or_default()),
        )
    }

    /// Get node at a specific time.
    pub fn get_node_at_time(&self, req: GetNodeAtTimeRequest) -> String {
        Self::extract_text(
            self.handle_get_node_at_time(serde_json::to_value(req).unwrap_or_default()),
        )
    }

    /// Get edge at a specific time.
    pub fn get_edge_at_time(&self, req: GetEdgeAtTimeRequest) -> String {
        Self::extract_text(
            self.handle_get_edge_at_time(serde_json::to_value(req).unwrap_or_default()),
        )
    }

    /// Execute a hybrid query.
    pub fn hybrid_query(&self, req: HybridQueryRequest) -> String {
        Self::extract_text(self.handle_hybrid_query(serde_json::to_value(req).unwrap_or_default()))
    }

    // ========================================================================
    // Helper methods for converting between GallifreyDB and MCP types
    // ========================================================================

    fn interned_to_string(&self, interned: crate::core::InternedString) -> String {
        GLOBAL_INTERNER
            .resolve(interned)
            .map(|s| s.to_string())
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
            PropertyValue::Bytes(b) => serde_json::Value::String(base64_encode(b)),
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

    fn json_to_property_map(&self, json: &HashMap<String, serde_json::Value>) -> PropertyMap {
        let mut builder = PropertyMapBuilder::new();
        for (key, value) in json {
            if let Some(pv) = self.json_to_property_value(value) {
                builder = builder.insert(key.as_str(), pv);
            }
        }
        builder.build()
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
        if let Ok(micros) = s.parse::<i64>() {
            return Ok(Timestamp::from(micros));
        }
        Err(format!(
            "Invalid timestamp format: {}. Expected microseconds since epoch.",
            s
        ))
    }

    fn matches_label(&self, interned: crate::core::InternedString, label: &str) -> bool {
        GLOBAL_INTERNER
            .with_str(interned, |s| s == label)
            .unwrap_or(false)
    }

    fn success_json(&self, value: serde_json::Value) -> CallToolResult {
        CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        )])
    }

    fn error_json(&self, msg: &str) -> CallToolResult {
        CallToolResult::error(vec![Content::text(json!({"error": msg}).to_string())])
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
                self.success_json(serde_json::to_value(&response).unwrap_or_default())
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_create_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: CreateNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let properties = req
            .properties
            .map(|p| self.json_to_property_map(&p))
            .unwrap_or_default();

        match self.db.create_node(&req.label, properties) {
            Ok(node_id) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let response = self.node_to_response(&node);
                    self.success_json(serde_json::to_value(&response).unwrap_or_default())
                }
                Err(e) => self.error_json(&e.to_string()),
            },
            Err(e) => self.error_json(&e.to_string()),
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

        let properties = self.json_to_property_map(&req.properties);

        match self.db.write(|tx| tx.update_node(node_id, properties)) {
            Ok(()) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let response = self.node_to_response(&node);
                    self.success_json(serde_json::to_value(&response).unwrap_or_default())
                }
                Err(e) => self.error_json(&e.to_string()),
            },
            Err(e) => self.error_json(&e.to_string()),
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

        match self.db.write(|tx| tx.delete_node(node_id)) {
            Ok(()) => self.success_json(json!({
                "success": true,
                "deleted_node_id": req.node_id
            })),
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_list_nodes(&self, args: serde_json::Value) -> CallToolResult {
        let req: ListNodesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let limit = req.limit.unwrap_or(100);
        let offset = req.offset.unwrap_or(0);

        // If a label is provided, use QueryBuilder to scan by label
        if let Some(label) = &req.label {
            let builder = crate::query::QueryBuilder::new().scan_label(label);

            match builder.limit(limit + offset).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let nodes: Vec<NodeResponse> = rows
                            .into_iter()
                            .skip(offset)
                            .filter_map(|row| {
                                if let EntityResult::Node(node) = row.entity {
                                    Some(self.node_to_response(&node))
                                } else {
                                    None
                                }
                            })
                            .collect();

                        self.success_json(json!({
                            "nodes": nodes,
                            "count": nodes.len(),
                            "offset": offset,
                            "limit": limit
                        }))
                    }
                    Err(e) => self.error_json(&e.to_string()),
                },
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

        let count = if let Some(label) = &req.label {
            // Use QueryBuilder to count by label
            let builder = crate::query::QueryBuilder::new().scan_label(label);
            match builder.execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => rows.len(),
                    Err(_) => 0,
                },
                Err(_) => 0,
            }
        } else {
            self.db.node_count()
        };

        self.success_json(json!({"count": count}))
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
                self.success_json(serde_json::to_value(&response).unwrap_or_default())
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

        let properties = req
            .properties
            .map(|p| self.json_to_property_map(&p))
            .unwrap_or_default();

        match self
            .db
            .create_edge(source_id, target_id, &req.label, properties)
        {
            Ok(edge_id) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let response = self.edge_to_response(&edge);
                    self.success_json(serde_json::to_value(&response).unwrap_or_default())
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

        let properties = self.json_to_property_map(&req.properties);

        match self.db.write(|tx| tx.update_edge(edge_id, properties)) {
            Ok(()) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let response = self.edge_to_response(&edge);
                    self.success_json(serde_json::to_value(&response).unwrap_or_default())
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

        let limit = req.limit.unwrap_or(100);
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

        let depth = req.depth.unwrap_or(1);
        let limit = req.limit.unwrap_or(100);
        let direction = req.direction.as_deref().unwrap_or("outgoing");

        // Use simple traversal approach
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

        let k = req.k.unwrap_or(10);

        if !self.db.is_vector_index_enabled_for(&req.property_name) {
            return self.error_json(&format!(
                "Vector index not enabled for property '{}'. Use enable_vector_index first.",
                req.property_name
            ));
        }

        match self.db.find_similar_by_embedding(&req.embedding, k) {
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

        let tx_time = if let Some(ref tx) = req.transaction_time {
            match self.parse_timestamp(tx) {
                Ok(t) => t,
                Err(e) => return self.error_json(&e),
            }
        } else {
            time::now()
        };

        match self.db.get_node_at_time(node_id, valid_time, tx_time) {
            Ok(node) => {
                let response = self.node_to_response(&node);
                self.success_json(json!({
                    "node": response,
                    "valid_time": req.valid_time,
                    "transaction_time": req.transaction_time.unwrap_or_else(|| "now".to_string())
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

        let tx_time = if let Some(ref tx) = req.transaction_time {
            match self.parse_timestamp(tx) {
                Ok(t) => t,
                Err(e) => return self.error_json(&e),
            }
        } else {
            time::now()
        };

        match self.db.get_edge_at_time(edge_id, valid_time, tx_time) {
            Ok(edge) => {
                let response = self.edge_to_response(&edge);
                self.success_json(json!({
                    "edge": response,
                    "valid_time": req.valid_time,
                    "transaction_time": req.transaction_time.unwrap_or_else(|| "now".to_string())
                }))
            }
            Err(e) => self.error_json(&e.to_string()),
        }
    }

    fn handle_hybrid_query(&self, args: serde_json::Value) -> CallToolResult {
        let req: HybridQueryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.error_json(&format!("Invalid arguments: {}", e)),
        };

        let limit = req.limit.unwrap_or(100);

        // Use QueryBuilder for hybrid queries
        if let Some(start_id) = req.start_node_id {
            let node_id = match NodeId::new(start_id) {
                Ok(id) => id,
                Err(e) => return self.error_json(&e.to_string()),
            };

            // Graph-first query with optional vector ranking
            let builder = crate::query::QueryBuilder::new().start(node_id);

            let builder = if let Some(ref edge_label) = req.traverse_edge {
                let depth = req.traverse_depth.unwrap_or(1);
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
                        let hybrid_results: Vec<HybridQueryResult> = rows
                            .into_iter()
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
                            .collect();

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
            let k = req.top_k.unwrap_or(10);
            let builder = crate::query::QueryBuilder::new().find_similar(embedding, k);

            match builder.limit(limit).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let hybrid_results: Vec<HybridQueryResult> = rows
                            .into_iter()
                            .filter_map(|row| {
                                if let EntityResult::Node(node) = row.entity {
                                    Some(HybridQueryResult {
                                        node: self.node_to_response(&node),
                                        similarity_score: row.score,
                                        traversal_path: None,
                                        timestamp: row.timestamp.map(|t| t.wallclock().to_string()),
                                    })
                                } else {
                                    None
                                }
                            })
                            .collect();

                        self.success_json(json!({
                            "results": hybrid_results,
                            "count": hybrid_results.len()
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
                        let hybrid_results: Vec<HybridQueryResult> = rows
                            .into_iter()
                            .filter_map(|row| {
                                if let EntityResult::Node(node) = row.entity {
                                    Some(HybridQueryResult {
                                        node: self.node_to_response(&node),
                                        similarity_score: None,
                                        traversal_path: None,
                                        timestamp: row.timestamp.map(|t| t.wallclock().to_string()),
                                    })
                                } else {
                                    None
                                }
                            })
                            .collect();

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
}

// Simple base64 encoding for bytes
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn make_input_schema<T: rmcp::schemars::JsonSchema>()
-> Arc<serde_json::Map<String, serde_json::Value>> {
    let schema = rmcp::schemars::schema_for!(T);
    let value = serde_json::to_value(schema).unwrap_or_default();
    match value {
        serde_json::Value::Object(map) => Arc::new(map),
        _ => Arc::new(serde_json::Map::new()),
    }
}

/// Implement the MCP ServerHandler trait.
impl ServerHandler for GallifreyMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "GallifreyDB MCP Server - A bi-temporal graph database with vector search. \
                 Use the provided tools to query and manipulate graph data with full \
                 temporal versioning and vector similarity search capabilities."
                    .to_string(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: vec![
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
                    "Delete a node by its ID.",
                    make_input_schema::<DeleteNodeRequest>(),
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
                    "hybrid_query",
                    "Execute a hybrid query combining graph traversal, vector similarity, and temporal filtering.",
                    make_input_schema::<HybridQueryRequest>(),
                ),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
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
            "get_node_at_time" => self.handle_get_node_at_time(args),
            "get_edge_at_time" => self.handle_get_edge_at_time(args),
            "hybrid_query" => self.handle_hybrid_query(args),
            _ => self.error_json(&format!("Unknown tool: {}", request.name)),
        };

        Ok(result)
    }
}
