//! HTTP request handlers for the AletheiaDB JSON API.
//!
//! Two public route handlers are exposed:
//!
//! - `GET /status` → [`health_check`]
//! - `POST /query` → [`handle_query`] (polymorphic JSON payload, see [`QueryRequest`])
//!
//! # Example Request
//!
//! ```json
//! {
//!   "operation": "find_node",
//!   "label": "Person",
//!   "properties": { "name": "Alice" }
//! }
//! ```

use crate::api::transaction::WriteOps;
use crate::core::NodeId;
use crate::http::converters::{
    interned_to_string, json_to_parameter_map, json_to_property_map, property_map_to_json,
    query_row_to_json,
};
use crate::http::error::AletheiaHttpError;
use crate::http::state::AppState;
use crate::query::QueryBuilder;
use crate::query::converter::{parse_query, parse_query_with_params};
use crate::query::ir::{Predicate, PredicateValue};
use autumn_web::Route;
use autumn_web::prelude::{get, post, routes};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// Resource caps to prevent DoS via deep pagination / large result sets.
const MAX_DEEP_PAGINATION: usize = 10_000;
const MAX_NEIGHBOR_LIMIT: usize = 1_000;
const MAX_EXEC_RESULTS: usize = 10_000;
const MAX_BULK_ITEMS: usize = 1_000;
const MAX_BULK_EXEC_TOTAL_ROWS: usize = 20_000;

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: String,
}

/// Health check endpoint. Returns `{"status": "healthy"}` with HTTP 200.
#[get("/status")]
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
    })
}

// ============================================================================
// Query Endpoint
// ============================================================================

/// Polymorphic request for the `/query` endpoint, discriminated by `operation`.
#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
#[allow(missing_docs)] // fields are self-describing via their names and variant-level docs
pub enum QueryRequest {
    /// Find nodes matching label/property filters. Paginated.
    FindNode {
        label: Option<String>,
        properties: Option<HashMap<String, Value>>,
        limit: Option<usize>,
        offset: Option<usize>,
    },
    /// Get a single node by its 64-bit ID.
    GetNode { node_id: u64 },
    /// Create a new node with an optional property map.
    CreateNode {
        label: String,
        properties: Option<HashMap<String, Value>>,
    },
    /// Create many nodes atomically in one transaction.
    BulkCreateNodes { nodes: Vec<CreateNodeInput> },
    /// Fetch many nodes by ID.
    BulkGetNodes { node_ids: Vec<u64> },
    /// Update many nodes atomically in one transaction.
    BulkUpdateNodes { updates: Vec<UpdateNodeInput> },
    /// Delete many nodes atomically in one transaction.
    BulkDeleteNodes {
        node_ids: Vec<u64>,
        #[serde(default)]
        cascade: bool,
    },
    /// Find neighbors (either direction) of a node. Paginated, deduped.
    FindNeighbors {
        node_id: u64,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
    /// Execute an AQL query string, optionally with bound parameters.
    ExecuteQuery {
        query: String,
        parameters: Option<HashMap<String, Value>>,
    },
    /// Execute many AQL queries in sequence and return per-query rows.
    BulkExecuteQuery { queries: Vec<ExecuteQueryInput> },
}

/// Payload for node creation inside a bulk request.
#[derive(Debug, Deserialize)]
pub struct CreateNodeInput {
    label: String,
    properties: Option<HashMap<String, Value>>,
}

/// Payload for node update inside a bulk request.
#[derive(Debug, Deserialize)]
pub struct UpdateNodeInput {
    node_id: u64,
    properties: Option<HashMap<String, Value>>,
}

/// Payload for query execution inside a bulk request.
#[derive(Debug, Deserialize)]
pub struct ExecuteQueryInput {
    query: String,
    parameters: Option<HashMap<String, Value>>,
}

/// Wrapper for all JSON responses: `{ success: bool, data?: ..., error?: ... }`.
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ApiResponse {
    fn success(data: Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}

/// Convert a `serde_json::Value` into a `PredicateValue`, or `None` for
/// unsupported kinds (arrays/objects aren't valid equality predicates).
fn json_to_predicate_value(v: &Value) -> Option<PredicateValue> {
    match v {
        Value::Null => Some(PredicateValue::Null),
        Value::Bool(b) => Some(PredicateValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(PredicateValue::Int(i))
            } else {
                n.as_f64().map(PredicateValue::Float)
            }
        }
        Value::String(s) => Some(PredicateValue::String(s.clone())),
        _ => None,
    }
}

/// Run a CPU/IO-bound closure on the blocking pool and unwrap the join error.
async fn blocking<F, T>(f: F) -> Result<T, AletheiaHttpError>
where
    F: FnOnce() -> Result<T, AletheiaHttpError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?
}

// ============================================================================
// Sub-handlers — each returns a JSON data payload or an error with a status.
// ============================================================================

async fn handle_create_node(
    db: Arc<crate::AletheiaDB>,
    label: String,
    properties: Option<HashMap<String, Value>>,
) -> Result<Value, AletheiaHttpError> {
    let props = match properties {
        Some(p) => json_to_property_map(&p).map_err(AletheiaHttpError::BadRequest)?,
        None => crate::core::PropertyMap::new(),
    };

    blocking(move || {
        let node_id = db
            .create_node(&label, props)
            .map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;
        let node = db
            .get_node(node_id)
            .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
        let props_json =
            property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
        Ok(json!({
            "id": node.id.as_u64(),
            "label": interned_to_string(node.label),
            "properties": props_json,
        }))
    })
    .await
}

async fn handle_get_node(
    db: Arc<crate::AletheiaDB>,
    node_id: u64,
) -> Result<Value, AletheiaHttpError> {
    let nid = NodeId::new(node_id).map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;

    blocking(move || {
        let node = db
            .get_node(nid)
            .map_err(|_| AletheiaHttpError::NotFound(format!("Node {node_id} not found")))?;
        let props_json =
            property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
        Ok(json!({
            "id": node.id.as_u64(),
            "label": interned_to_string(node.label),
            "properties": props_json,
        }))
    })
    .await
}

async fn handle_find_node(
    db: Arc<crate::AletheiaDB>,
    label: Option<String>,
    properties: Option<HashMap<String, Value>>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Value, AletheiaHttpError> {
    let limit_val = limit.unwrap_or(100);
    let offset_val = offset.unwrap_or(0);

    if offset_val.saturating_add(limit_val) > MAX_DEEP_PAGINATION {
        return Err(AletheiaHttpError::BadRequest(format!(
            "Pagination limit exceeded: offset + limit must be <= {MAX_DEEP_PAGINATION}"
        )));
    }

    blocking(move || {
        let mut builder = if let Some(lbl) = label {
            QueryBuilder::new().scan_label(&lbl)
        } else {
            QueryBuilder::new().scan(None)
        };

        if let Some(props) = properties {
            for (key, value) in props {
                if let Some(pred_value) = json_to_predicate_value(&value) {
                    builder = builder.filter(Predicate::eq(key, pred_value));
                }
            }
        }

        if let Some(skip) = offset {
            builder = builder.skip(skip);
        }

        let results = builder
            .limit(limit_val)
            .execute(&db)
            .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;

        // NOTE: explicit `row_result?` rather than `.flatten()` so storage
        // errors mid-scan propagate as 500 instead of being silently dropped
        // and producing a partial `success: true` response.
        let mut nodes = Vec::with_capacity(limit_val);
        for row_result in results {
            let row = row_result.map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            if let crate::query::executor::EntityResult::Node(node) = row.entity {
                let props_json =
                    property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
                nodes.push(json!({
                    "id": node.id.as_u64(),
                    "label": interned_to_string(node.label),
                    "properties": props_json,
                }));
            }
        }
        Ok(Value::Array(nodes))
    })
    .await
}

async fn handle_find_neighbors(
    db: Arc<crate::AletheiaDB>,
    node_id: u64,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Value, AletheiaHttpError> {
    let nid = NodeId::new(node_id).map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;

    let limit_val = limit.unwrap_or(100).min(MAX_NEIGHBOR_LIMIT);
    let offset_val = offset.unwrap_or(0);

    if offset_val.saturating_add(limit_val) > MAX_DEEP_PAGINATION {
        return Err(AletheiaHttpError::BadRequest(format!(
            "Pagination limit exceeded: offset + limit must be <= {MAX_DEEP_PAGINATION}"
        )));
    }

    blocking(move || {
        let mut seen_ids = HashSet::new();
        let mut neighbors = Vec::with_capacity(limit_val);

        let outgoing_iter = db
            .get_outgoing_edges_iter(nid)
            .map(|edge_id| db.get_edge_target(edge_id).ok());
        let incoming_iter = db
            .get_incoming_edges_iter(nid)
            .map(|edge_id| db.get_edge_source(edge_id).ok());

        let combined_iter = outgoing_iter
            .chain(incoming_iter)
            .flatten()
            .filter(|&neighbor_id| seen_ids.insert(neighbor_id))
            .skip(offset_val)
            .take(limit_val);

        for neighbor_id in combined_iter {
            let node = db
                .get_node(neighbor_id)
                .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            let props_json =
                property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
            neighbors.push(json!({
                "id": node.id.as_u64(),
                "label": interned_to_string(node.label),
                "properties": props_json,
            }));
        }
        Ok(Value::Array(neighbors))
    })
    .await
}

async fn handle_execute_query(
    db: Arc<crate::AletheiaDB>,
    query: String,
    parameters: Option<HashMap<String, Value>>,
) -> Result<Value, AletheiaHttpError> {
    blocking(move || {
        let parsed_query = if let Some(params_json) = parameters {
            let params =
                json_to_parameter_map(&params_json).map_err(AletheiaHttpError::BadRequest)?;
            parse_query_with_params(&query, params)
                .map_err(|e| AletheiaHttpError::QueryParse(e.to_string()))?
        } else {
            parse_query(&query).map_err(|e| classify_query_error(e.to_string()))?
        };

        let results = db
            .execute_query(parsed_query)
            .map_err(|e| classify_query_error(e.to_string()))?;

        let rows = results
            .take(MAX_EXEC_RESULTS)
            .map(|row_result| {
                let row = row_result.map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
                query_row_to_json(row).map_err(AletheiaHttpError::Internal)
            })
            .collect::<Result<Vec<_>, AletheiaHttpError>>()?;

        Ok(Value::Array(rows))
    })
    .await
}

fn convert_node_for_response(node: crate::core::graph::Node) -> Result<Value, AletheiaHttpError> {
    let props_json = property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
    Ok(json!({
        "id": node.id.as_u64(),
        "label": interned_to_string(node.label),
        "properties": props_json,
    }))
}

fn validate_bulk_size<T>(items: &[T], name: &str) -> Result<(), AletheiaHttpError> {
    if items.is_empty() {
        return Err(AletheiaHttpError::BadRequest(format!(
            "{name} cannot be empty"
        )));
    }
    if items.len() > MAX_BULK_ITEMS {
        return Err(AletheiaHttpError::BadRequest(format!(
            "{name} exceeds max size of {MAX_BULK_ITEMS}"
        )));
    }
    Ok(())
}

async fn handle_bulk_create_nodes(
    db: Arc<crate::AletheiaDB>,
    nodes: Vec<CreateNodeInput>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&nodes, "nodes")?;
    blocking(move || {
        let created_ids = db
            .write(|tx| {
                let mut ids = Vec::with_capacity(nodes.len());
                for node in &nodes {
                    let props = match &node.properties {
                        Some(p) => json_to_property_map(p)
                            .map_err(|e| crate::core::error::Error::other(e.to_string()))?,
                        None => crate::core::PropertyMap::new(),
                    };
                    ids.push(tx.create_node(&node.label, props)?);
                }
                Ok::<_, crate::core::error::Error>(ids)
            })
            .map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;

        let mut created = Vec::with_capacity(created_ids.len());
        for id in created_ids {
            let node = db
                .get_node(id)
                .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            created.push(convert_node_for_response(node)?);
        }
        Ok(Value::Array(created))
    })
    .await
}

async fn handle_bulk_get_nodes(
    db: Arc<crate::AletheiaDB>,
    node_ids: Vec<u64>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&node_ids, "node_ids")?;
    blocking(move || {
        let mut found = Vec::with_capacity(node_ids.len());
        let mut missing = Vec::with_capacity(node_ids.len());

        for node_id in node_ids {
            let nid =
                NodeId::new(node_id).map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;
            match db.get_node(nid) {
                Ok(node) => found.push(convert_node_for_response(node)?),
                Err(_) => missing.push(node_id),
            }
        }

        Ok(json!({
            "nodes": found,
            "missing_node_ids": missing,
        }))
    })
    .await
}

async fn handle_bulk_update_nodes(
    db: Arc<crate::AletheiaDB>,
    updates: Vec<UpdateNodeInput>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&updates, "updates")?;
    blocking(move || {
        let updated_ids = db
            .write(|tx| {
                let mut ids = Vec::with_capacity(updates.len());
                for update in &updates {
                    let node_id = NodeId::new(update.node_id)?;
                    let props = match &update.properties {
                        Some(p) => json_to_property_map(p)
                            .map_err(|e| crate::core::error::Error::other(e.to_string()))?,
                        None => crate::core::PropertyMap::new(),
                    };
                    tx.update_node(node_id, props)?;
                    ids.push(node_id);
                }
                Ok::<_, crate::core::error::Error>(ids)
            })
            .map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;

        let mut updated = Vec::with_capacity(updated_ids.len());
        for id in updated_ids {
            let node = db
                .get_node(id)
                .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            updated.push(convert_node_for_response(node)?);
        }
        Ok(Value::Array(updated))
    })
    .await
}

async fn handle_bulk_delete_nodes(
    db: Arc<crate::AletheiaDB>,
    node_ids: Vec<u64>,
    cascade: bool,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&node_ids, "node_ids")?;
    blocking(move || {
        let deleted_count = db
            .write(|tx| {
                for node_id in &node_ids {
                    let nid = NodeId::new(*node_id)?;
                    if cascade {
                        tx.delete_node_cascade(nid)?;
                    } else {
                        tx.delete_node(nid)?;
                    }
                }
                Ok::<_, crate::core::error::Error>(node_ids.len())
            })
            .map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;

        Ok(json!({
            "deleted_count": deleted_count,
            "cascade": cascade,
        }))
    })
    .await
}

async fn handle_bulk_execute_query(
    db: Arc<crate::AletheiaDB>,
    queries: Vec<ExecuteQueryInput>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&queries, "queries")?;
    blocking(move || {
        let mut all_results = Vec::with_capacity(queries.len());
        let mut total_rows = 0usize;
        for (idx, query_item) in queries.into_iter().enumerate() {
            let parsed_query = if let Some(params_json) = query_item.parameters {
                let params =
                    json_to_parameter_map(&params_json).map_err(AletheiaHttpError::BadRequest)?;
                parse_query_with_params(&query_item.query, params)
                    .map_err(|e| AletheiaHttpError::QueryParse(e.to_string()))?
            } else {
                parse_query(&query_item.query).map_err(|e| classify_query_error(e.to_string()))?
            };

            let rows = db
                .execute_query(parsed_query)
                .map_err(|e| classify_query_error(e.to_string()))?
                .take(MAX_EXEC_RESULTS)
                .map(|row_result| {
                    let row = row_result.map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
                    query_row_to_json(row).map_err(AletheiaHttpError::Internal)
                })
                .collect::<Result<Vec<_>, AletheiaHttpError>>()?;

            total_rows = total_rows.saturating_add(rows.len());
            if total_rows > MAX_BULK_EXEC_TOTAL_ROWS {
                return Err(AletheiaHttpError::BadRequest(format!(
                    "Bulk query result budget exceeded at query {}: total rows {} > {}",
                    idx + 1,
                    total_rows,
                    MAX_BULK_EXEC_TOTAL_ROWS
                )));
            }

            all_results.push(json!({
                "query": query_item.query,
                "rows": rows,
            }));
        }
        Ok(Value::Array(all_results))
    })
    .await
}

/// Heuristic: parser errors → 400, everything else → 500.
fn classify_query_error(msg: String) -> AletheiaHttpError {
    let lowered = msg.to_lowercase();
    if lowered.contains("syntax") || lowered.contains("parse") {
        AletheiaHttpError::QueryParse(msg)
    } else {
        AletheiaHttpError::Internal(msg)
    }
}

/// Polymorphic `/query` endpoint. Dispatches on the `operation` tag.
#[post("/query")]
pub async fn handle_query(
    state: AppState,
    Json(req): Json<QueryRequest>,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    let db = state.db_arc();

    let data = match req {
        QueryRequest::CreateNode { label, properties } => {
            handle_create_node(db, label, properties).await?
        }
        QueryRequest::GetNode { node_id } => handle_get_node(db, node_id).await?,
        QueryRequest::BulkCreateNodes { nodes } => handle_bulk_create_nodes(db, nodes).await?,
        QueryRequest::BulkGetNodes { node_ids } => handle_bulk_get_nodes(db, node_ids).await?,
        QueryRequest::BulkUpdateNodes { updates } => handle_bulk_update_nodes(db, updates).await?,
        QueryRequest::BulkDeleteNodes { node_ids, cascade } => {
            handle_bulk_delete_nodes(db, node_ids, cascade).await?
        }
        QueryRequest::FindNode {
            label,
            properties,
            limit,
            offset,
        } => handle_find_node(db, label, properties, limit, offset).await?,
        QueryRequest::FindNeighbors {
            node_id,
            limit,
            offset,
        } => handle_find_neighbors(db, node_id, limit, offset).await?,
        QueryRequest::ExecuteQuery { query, parameters } => {
            handle_execute_query(db, query, parameters).await?
        }
        QueryRequest::BulkExecuteQuery { queries } => {
            handle_bulk_execute_query(db, queries).await?
        }
    };

    Ok(Json(ApiResponse::success(data)))
}

/// Collect the crate's HTTP routes as a `Vec<Route>`.
///
/// Kept in this module so the `routes![...]` macro resolves the companion
/// functions generated by `#[get]` / `#[post]` in the same scope where the
/// handlers are declared.
#[must_use]
pub fn all_routes() -> Vec<Route> {
    routes![health_check, handle_query]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_to_predicate_value_covers_supported_kinds() {
        assert_eq!(
            json_to_predicate_value(&Value::Null),
            Some(PredicateValue::Null)
        );
        assert_eq!(
            json_to_predicate_value(&Value::Bool(true)),
            Some(PredicateValue::Bool(true))
        );
        assert_eq!(
            json_to_predicate_value(&Value::Bool(false)),
            Some(PredicateValue::Bool(false))
        );
        assert_eq!(
            json_to_predicate_value(&Value::Number(42.into())),
            Some(PredicateValue::Int(42))
        );
        assert_eq!(
            json_to_predicate_value(&Value::Number(serde_json::Number::from_f64(42.5).unwrap())),
            Some(PredicateValue::Float(42.5))
        );
        assert_eq!(
            json_to_predicate_value(&Value::String("hello".to_string())),
            Some(PredicateValue::String("hello".to_string()))
        );
    }

    #[test]
    fn json_to_predicate_value_rejects_composite_kinds() {
        assert_eq!(json_to_predicate_value(&Value::Array(vec![])), None);
        assert_eq!(
            json_to_predicate_value(&Value::Object(serde_json::Map::new())),
            None
        );
    }

    #[test]
    fn classify_query_error_routes_parse_errors_to_bad_request() {
        assert!(matches!(
            classify_query_error("Syntax error at line 1".into()),
            AletheiaHttpError::QueryParse(_)
        ));
        assert!(matches!(
            classify_query_error("Parse error".into()),
            AletheiaHttpError::QueryParse(_)
        ));
        assert!(matches!(
            classify_query_error("Storage failure".into()),
            AletheiaHttpError::Internal(_)
        ));
    }
}
