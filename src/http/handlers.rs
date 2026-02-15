//! HTTP request handlers.
//!
//! This module implements the REST API endpoints for AletheiaDB.
//!
//! # Endpoints
//!
//! *   `GET /status`: Health check.
//! *   `POST /query`: General purpose query endpoint supporting multiple operations.
//!
//! # Query Operations
//!
//! The `/query` endpoint accepts a JSON payload with an `operation` field. Supported operations:
//!
//! *   `create_node`: Create a new node with properties.
//! *   `get_node`: Retrieve a node by ID.
//! *   `find_node`: Search for nodes by label and properties.
//! *   `find_neighbors`: Traverse edges to find connected nodes.

use crate::core::NodeId;
use crate::http::converters::{interned_to_string, json_to_property_map, property_map_to_json};
use crate::http::state::AppState;
use crate::query::QueryBuilder;
use crate::query::ir::{Predicate, PredicateValue};
use crate::AletheiaDB;
use actix_web::{HttpResponse, web};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};

// ============================================================================
// Health Check
// ============================================================================

/// Health check response structure.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: String,
}

/// Health check endpoint handler.
///
/// Returns a JSON response with `{"status": "healthy"}` and HTTP 200 OK.
///
/// # Example Response
///
/// ```json
/// {
///   "status": "healthy"
/// }
/// ```
pub async fn health_check() -> HttpResponse {
    let response = HealthResponse {
        status: "healthy".to_string(),
    };
    HttpResponse::Ok().json(response)
}

/// Configure the health routes.
pub fn configure_health_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/status", web::get().to(health_check));
}

// ============================================================================
// Query Types
// ============================================================================

/// Request payload for the `/query` endpoint.
#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum QueryRequest {
    /// Find nodes by label and optional property filters.
    FindNode {
        /// Label to filter by (optional).
        label: Option<String>,
        /// Property key-value pairs to match.
        properties: Option<HashMap<String, serde_json::Value>>,
        /// Maximum number of results to return.
        limit: Option<usize>,
        /// Number of results to skip.
        offset: Option<usize>,
    },
    /// Retrieve a single node by its ID.
    GetNode {
        /// The unique node ID.
        node_id: u64,
    },
    /// Create a new node.
    CreateNode {
        /// The label for the new node.
        label: String,
        /// Properties to assign to the node.
        properties: Option<HashMap<String, serde_json::Value>>,
    },
    /// Find neighbors of a starting node.
    FindNeighbors {
        /// The starting node ID.
        node_id: u64,
        /// Maximum number of neighbors to return (default: 100, max: 1000).
        #[serde(default)]
        limit: Option<usize>,
        /// Number of neighbors to skip.
        #[serde(default)]
        offset: Option<usize>,
    },
}

/// Standard JSON response wrapper.
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    /// Whether the operation was successful.
    success: bool,
    /// The result data (if success is true).
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    /// Error message (if success is false).
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ApiResponse {
    /// Create a success response.
    fn success(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    /// Create an error response.
    fn error(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// Main query endpoint handler.
///
/// Dispatches to specific handlers based on the `operation` field in the JSON body.
pub async fn handle_query(
    state: web::Data<AppState>,
    req: web::Json<QueryRequest>,
) -> HttpResponse {
    let db = state.db();

    match req.into_inner() {
        QueryRequest::CreateNode { label, properties } => {
            handle_create_node(db, label, properties).await
        }
        QueryRequest::GetNode { node_id } => handle_get_node(db, node_id).await,
        QueryRequest::FindNode {
            label,
            properties,
            limit,
            offset,
        } => handle_find_node(db, label, properties, limit, offset).await,
        QueryRequest::FindNeighbors {
            node_id,
            limit,
            offset,
        } => handle_find_neighbors(db, node_id, limit, offset).await,
    }
}

/// Handle `create_node` operation.
///
/// Creates a new node and returns the created node structure.
async fn handle_create_node(
    db: &AletheiaDB,
    label: String,
    properties: Option<HashMap<String, serde_json::Value>>,
) -> HttpResponse {
    let props = match properties {
        Some(p) => match json_to_property_map(&p) {
            Ok(map) => map,
            Err(e) => return HttpResponse::BadRequest().json(ApiResponse::error(e)),
        },
        None => crate::core::PropertyMap::new(),
    };

    match db.create_node(&label, props) {
        Ok(node_id) => match db.get_node(node_id) {
            Ok(node) => {
                let props_json = match property_map_to_json(&node.properties) {
                    Ok(p) => p,
                    Err(e) => {
                        return HttpResponse::InternalServerError().json(ApiResponse::error(e));
                    }
                };
                let node_json = json!({
                    "id": node.id.as_u64(),
                    "label": interned_to_string(node.label),
                    "properties": props_json
                });
                HttpResponse::Ok().json(ApiResponse::success(node_json))
            }
            Err(e) => HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string())),
        },
        Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    }
}

/// Handle `get_node` operation.
///
/// Retrieves a node by ID. Returns 404 if not found.
async fn handle_get_node(db: &AletheiaDB, node_id: u64) -> HttpResponse {
    match NodeId::new(node_id) {
        Ok(nid) => match db.get_node(nid) {
            Ok(node) => {
                let props_json = match property_map_to_json(&node.properties) {
                    Ok(p) => p,
                    Err(e) => {
                        return HttpResponse::InternalServerError().json(ApiResponse::error(e));
                    }
                };
                let node_json = json!({
                    "id": node.id.as_u64(),
                    "label": interned_to_string(node.label),
                    "properties": props_json
                });
                HttpResponse::Ok().json(ApiResponse::success(node_json))
            }
            Err(_) => HttpResponse::NotFound()
                .json(ApiResponse::error(format!("Node {} not found", node_id))),
        },
        Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    }
}

/// Handle `find_node` operation.
///
/// Searches for nodes matching the given label and properties.
/// Supports pagination via `limit` and `offset`.
async fn handle_find_node(
    db: &AletheiaDB,
    label: Option<String>,
    properties: Option<HashMap<String, serde_json::Value>>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> HttpResponse {
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

    let limit_val = limit.unwrap_or(100);
    let offset_val = offset.unwrap_or(0);

    // Prevent deep pagination attacks (CPU DoS)
    let max_deep_pagination = 10_000;
    if offset_val.saturating_add(limit_val) > max_deep_pagination {
        return HttpResponse::BadRequest().json(ApiResponse::error(format!(
            "Pagination limit exceeded: offset + limit must be <= {}",
            max_deep_pagination
        )));
    }

    if let Some(skip) = offset {
        builder = builder.skip(skip);
    }

    match builder.limit(limit_val).execute(db) {
        Ok(results) => {
            let mut nodes = Vec::new();
            for row in results.flatten() {
                if let crate::query::executor::EntityResult::Node(node) = row.entity {
                    let props_json = match property_map_to_json(&node.properties) {
                        Ok(p) => p,
                        Err(e) => {
                            return HttpResponse::InternalServerError().json(ApiResponse::error(e));
                        }
                    };
                    nodes.push(json!({
                        "id": node.id.as_u64(),
                        "label": interned_to_string(node.label),
                        "properties": props_json
                    }));
                }
            }
            HttpResponse::Ok().json(ApiResponse::success(json!(nodes)))
        }
        Err(e) => HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string())),
    }
}

/// Handle `find_neighbors` operation.
///
/// Finds nodes directly connected to the source node.
async fn handle_find_neighbors(
    db: &AletheiaDB,
    node_id: u64,
    limit: Option<usize>,
    offset: Option<usize>,
) -> HttpResponse {
    match NodeId::new(node_id) {
        Ok(nid) => {
            // Safety limits to prevent DoS
            let max_limit = 1000;
            let limit_val = limit.unwrap_or(100).min(max_limit);
            let offset_val = offset.unwrap_or(0);

            // Prevent deep pagination attacks (CPU DoS)
            // Even though neighbor scan is localized, deep offset can still waste CPU
            let max_deep_pagination = 10_000;
            if offset_val.saturating_add(limit_val) > max_deep_pagination {
                return HttpResponse::BadRequest().json(ApiResponse::error(format!(
                    "Pagination limit exceeded: offset + limit must be <= {}",
                    max_deep_pagination
                )));
            }

            // Deduplication
            let mut seen_ids = HashSet::new();
            let mut neighbors = Vec::with_capacity(limit_val);

            // Use zero-allocation iterators
            let outgoing_iter = db
                .get_outgoing_edges_iter(nid)
                .map(|edge_id| db.get_edge_target(edge_id).ok());

            let incoming_iter = db
                .get_incoming_edges_iter(nid)
                .map(|edge_id| db.get_edge_source(edge_id).ok());

            let combined_iter = outgoing_iter
                .chain(incoming_iter)
                .flatten() // remove None (failed lookups)
                .filter(|&neighbor_id| seen_ids.insert(neighbor_id)) // deduplicate
                .skip(offset_val)
                .take(limit_val);

            for neighbor_id in combined_iter {
                match db.get_node(neighbor_id) {
                    Ok(node) => {
                        let props_json = match property_map_to_json(&node.properties) {
                            Ok(p) => p,
                            Err(e) => {
                                return HttpResponse::InternalServerError()
                                    .json(ApiResponse::error(e));
                            }
                        };
                        neighbors.push(json!({
                            "id": node.id.as_u64(),
                            "label": interned_to_string(node.label),
                            "properties": props_json
                        }));
                    }
                    Err(e) => {
                        return HttpResponse::InternalServerError()
                            .json(ApiResponse::error(e.to_string()));
                    }
                }
            }

            HttpResponse::Ok().json(ApiResponse::success(json!(neighbors)))
        }
        Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    }
}

/// Convert serde_json Value to PredicateValue
fn json_to_predicate_value(v: &serde_json::Value) -> Option<PredicateValue> {
    match v {
        serde_json::Value::Null => Some(PredicateValue::Null),
        serde_json::Value::Bool(b) => Some(PredicateValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(PredicateValue::Int(i))
            } else {
                n.as_f64().map(PredicateValue::Float)
            }
        }
        serde_json::Value::String(s) => Some(PredicateValue::String(s.clone())),
        _ => None, // Arrays/Objects not supported for simple equality predicates yet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, test};

    #[actix_rt::test]
    async fn test_health_check_returns_ok() {
        let app =
            test::init_service(App::new().route("/status", web::get().to(health_check))).await;

        let req = test::TestRequest::get().uri("/status").to_request();
        let resp = test::call_service(&app, req).await;

        assert!(resp.status().is_success());
    }

    #[actix_rt::test]
    async fn test_health_check_returns_json() {
        let app =
            test::init_service(App::new().route("/status", web::get().to(health_check))).await;

        let req = test::TestRequest::get().uri("/status").to_request();
        let resp = test::call_service(&app, req).await;

        let body = test::read_body(resp).await;
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("Response body should be valid JSON");

        assert_eq!(json["status"], "healthy");
    }

    // Warden: Reproduction of integer overflow in FindNeighbors
    #[actix_rt::test]
    async fn test_warden_find_neighbors_overflow() {
        let db = std::sync::Arc::new(crate::AletheiaDB::new().unwrap());
        let state = web::Data::new(AppState::new(db));

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/query", web::post().to(handle_query)),
        )
        .await;

        // Malicious payload: offset = usize::MAX, limit = 1
        // offset + limit = usize::MAX + 1 = 0 (wrapped), which is < max_deep_pagination (10000)
        let payload = json!({
            "operation": "find_neighbors",
            "node_id": 1,
            "offset": usize::MAX,
            "limit": 1
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&payload)
            .to_request();

        let resp = test::call_service(&app, req).await;

        // SECURE: Should return 400 Bad Request
        assert!(
            resp.status().is_client_error(),
            "Should reject deep pagination"
        );
        let body = test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let error = json["error"].as_str().unwrap();
        assert!(
            error.contains("Pagination limit exceeded"),
            "Error: {}",
            error
        );
    }

    // Warden: Check if FindNode allows deep pagination
    #[actix_rt::test]
    async fn test_warden_find_node_deep_pagination() {
        let db = std::sync::Arc::new(crate::AletheiaDB::new().unwrap());
        let state = web::Data::new(AppState::new(db));

        let app = test::init_service(
            App::new()
                .app_data(state)
                .route("/query", web::post().to(handle_query)),
        )
        .await;

        // Deep pagination request
        let payload = json!({
            "operation": "find_node",
            "label": "Person",
            "offset": 100_000,
            "limit": 10
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&payload)
            .to_request();

        let resp = test::call_service(&app, req).await;

        // SECURE: Should return 400 Bad Request
        assert!(
            resp.status().is_client_error(),
            "Should reject deep pagination"
        );
        let body = test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let error = json["error"].as_str().unwrap();
        assert!(
            error.contains("Pagination limit exceeded"),
            "Error: {}",
            error
        );
    }
}
