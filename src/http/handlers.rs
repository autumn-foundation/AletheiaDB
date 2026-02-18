//! HTTP request handlers.
//!
//! This module defines the JSON API contract for the AletheiaDB HTTP server.
//! It includes the request structures, response formats, and handler functions.
//!
//! # API Structure
//!
//! All query operations are handled via a single POST endpoint (`/query`) that accepts
//! a JSON payload. The payload is polymorphic, using the `operation` field to distinguish
//! between different request types.
//!
//! # Example Request
//!
//! ```json
//! {
//!   "operation": "find_node",
//!   "label": "Person",
//!   "properties": {
//!     "name": "Alice"
//!   }
//! }
//! ```

use crate::core::NodeId;
use crate::http::converters::{interned_to_string, json_to_property_map, property_map_to_json};
use crate::http::state::AppState;
use crate::query::QueryBuilder;
use crate::query::ir::{Predicate, PredicateValue};
use actix_web::{HttpResponse, web};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Health check response structure.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: String,
}

/// Health check endpoint handler.
///
/// Returns a JSON response with `{"status": "healthy"}` and HTTP 200 OK.
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
// Query Endpoint
// ============================================================================

/// Polymorphic request structure for the `/query` endpoint.
///
/// The structure deserializes based on the `operation` field tag.
///
/// # Examples
///
/// ## Create a Node
/// ```json
/// {
///   "operation": "create_node",
///   "label": "Person",
///   "properties": {
///     "name": "Alice",
///     "age": 30
///   }
/// }
/// ```
///
/// ## Find Nodes
/// ```json
/// {
///   "operation": "find_node",
///   "label": "Person",
///   "limit": 10
/// }
/// ```
#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum QueryRequest {
    /// Find nodes matching specific criteria.
    ///
    /// # Fields
    /// * `label` - Optional label to filter by (e.g., "Person").
    /// * `properties` - Optional map of property key-value pairs to match (exact match).
    /// * `limit` - Maximum number of results to return (default: 100, max: 1000).
    /// * `offset` - Number of results to skip (default: 0).
    ///
    /// # Example
    /// ```json
    /// {
    ///   "operation": "find_node",
    ///   "label": "Person",
    ///   "properties": { "active": true },
    ///   "limit": 50
    /// }
    /// ```
    FindNode {
        /// Optional label to filter by (e.g., "Person").
        label: Option<String>,
        /// Optional map of property key-value pairs to match (exact match).
        properties: Option<HashMap<String, serde_json::Value>>,
        /// Maximum number of results to return (default: 100, max: 1000).
        limit: Option<usize>,
        /// Number of results to skip (default: 0).
        offset: Option<usize>,
    },

    /// Get a single node by its internal ID.
    ///
    /// # Fields
    /// * `node_id` - The 64-bit unsigned integer ID of the node.
    ///
    /// # Example
    /// ```json
    /// {
    ///   "operation": "get_node",
    ///   "node_id": 12345
    /// }
    /// ```
    GetNode {
        /// The 64-bit unsigned integer ID of the node.
        node_id: u64,
    },

    /// Create a new node.
    ///
    /// # Fields
    /// * `label` - The label/type of the node (e.g., "User").
    /// * `properties` - Optional initial properties for the node.
    ///
    /// # Example
    /// ```json
    /// {
    ///   "operation": "create_node",
    ///   "label": "User",
    ///   "properties": {
    ///     "username": "jdoe",
    ///     "email": "jdoe@example.com"
    ///   }
    /// }
    /// ```
    CreateNode {
        /// The label/type of the node (e.g., "User").
        label: String,
        /// Optional initial properties for the node.
        properties: Option<HashMap<String, serde_json::Value>>,
    },

    /// Find all neighbors connected to a specific node.
    ///
    /// Returns nodes connected by *any* edge direction (incoming or outgoing).
    ///
    /// # Fields
    /// * `node_id` - The ID of the central node.
    /// * `limit` - Maximum number of neighbors to return (default: 100).
    /// * `offset` - Pagination offset (default: 0).
    ///
    /// # Example
    /// ```json
    /// {
    ///   "operation": "find_neighbors",
    ///   "node_id": 12345,
    ///   "limit": 20
    /// }
    /// ```
    FindNeighbors {
        /// The ID of the central node.
        node_id: u64,
        /// Maximum number of neighbors to return (default: 100).
        #[serde(default)]
        limit: Option<usize>,
        /// Pagination offset (default: 0).
        #[serde(default)]
        offset: Option<usize>,
    },
}

/// Standardized API response structure.
///
/// All API responses follow this wrapper format.
///
/// # Structure
///
/// * `success`: Boolean indicating if the operation succeeded.
/// * `data`: The result payload (only present if `success` is true).
/// * `error`: Error message string (only present if `success` is false).
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    /// Indicates whether the request was successful.
    success: bool,
    /// The successful response payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    /// The error message if the request failed.
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

/// Helper to validate pagination parameters.
///
/// Enforces limits on `limit` and `offset` to prevent DoS.
///
/// # Arguments
///
/// * `limit` - Requested limit (optional)
/// * `offset` - Requested offset (optional)
/// * `max_limit` - Maximum allowed limit (hard cap)
///
/// # Returns
///
/// * `Ok((limit, offset))` - Validated parameters
/// * `Err(String)` - Error message if invalid
fn validate_pagination(
    limit: Option<usize>,
    offset: Option<usize>,
    max_limit: usize,
) -> Result<(usize, usize), String> {
    let limit_val = limit.unwrap_or(100).min(max_limit);
    let offset_val = offset.unwrap_or(0);

    // Prevent deep pagination attacks (CPU DoS)
    // Use saturating_add to prevent integer overflow bypass
    const MAX_DEEP_PAGINATION: usize = 10_000;
    if offset_val.saturating_add(limit_val) > MAX_DEEP_PAGINATION {
        return Err(format!(
            "Pagination limit exceeded: offset + limit must be <= {}",
            MAX_DEEP_PAGINATION
        ));
    }

    Ok((limit_val, offset_val))
}

/// Query endpoint handler.
///
/// Accepts a JSON `QueryRequest` and executes the corresponding operation against the database.
/// Returns an `ApiResponse` wrapped in an `HttpResponse`.
///
/// # Resource Limits
///
/// * **Pagination**: `limit + offset` must not exceed 10,000 to prevent CPU exhaustion.
/// * **Result Size**: `limit` is capped at 1000 for neighbor queries.
pub async fn handle_query(
    state: web::Data<AppState>,
    req: web::Json<QueryRequest>,
) -> HttpResponse {
    let db = state.db();

    match req.into_inner() {
        QueryRequest::CreateNode { label, properties } => handle_create_node(&db, label, properties),
        QueryRequest::GetNode { node_id } => handle_get_node(&db, node_id),
        QueryRequest::FindNode {
            label,
            properties,
            limit,
            offset,
        } => handle_find_node(&db, label, properties, limit, offset),
        QueryRequest::FindNeighbors {
            node_id,
            limit,
            offset,
        } => handle_find_neighbors(&db, node_id, limit, offset),
    }
}

fn handle_create_node(
    db: &crate::AletheiaDB,
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
            Ok(node) => match node_to_json(&node) {
                Ok(node_json) => HttpResponse::Ok().json(ApiResponse::success(node_json)),
                Err(e) => {
                    HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string()))
                }
            },
            Err(e) => HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string())),
        },
        Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    }
}

fn handle_get_node(db: &crate::AletheiaDB, node_id: u64) -> HttpResponse {
    match NodeId::new(node_id) {
        Ok(nid) => match db.get_node(nid) {
            Ok(node) => match node_to_json(&node) {
                Ok(node_json) => HttpResponse::Ok().json(ApiResponse::success(node_json)),
                Err(e) => {
                    HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string()))
                }
            },
            // Issue 1: Return 404 for node not found
            Err(_) => HttpResponse::NotFound()
                .json(ApiResponse::error(format!("Node {} not found", node_id))),
        },
        Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    }
}

fn handle_find_node(
    db: &crate::AletheiaDB,
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

    // Issue 4: Pagination
    // Max limit for generic search is 1000 to prevents DoS
    let (limit_val, offset_val) = match validate_pagination(limit, offset, 1000) {
        Ok(vals) => vals,
        Err(e) => return HttpResponse::BadRequest().json(ApiResponse::error(e)),
    };

    if offset_val > 0 {
        builder = builder.skip(offset_val);
    }

    match builder.limit(limit_val).execute(db) {
        Ok(results) => {
            let mut nodes = Vec::new();
            for row in results.flatten() {
                if let crate::query::executor::EntityResult::Node(node) = row.entity {
                    match node_to_json(&node) {
                        Ok(json) => nodes.push(json),
                        Err(e) => {
                            return HttpResponse::InternalServerError()
                                .json(ApiResponse::error(e.to_string()));
                        }
                    }
                }
            }
            HttpResponse::Ok().json(ApiResponse::success(json!(nodes)))
        }
        Err(e) => HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string())),
    }
}

fn handle_find_neighbors(
    db: &crate::AletheiaDB,
    node_id: u64,
    limit: Option<usize>,
    offset: Option<usize>,
) -> HttpResponse {
    match NodeId::new(node_id) {
        Ok(nid) => {
            // Safety limits to prevent DoS
            let (limit_val, offset_val) = match validate_pagination(limit, offset, 1000) {
                Ok(vals) => vals,
                Err(e) => return HttpResponse::BadRequest().json(ApiResponse::error(e)),
            };

            // Deduplication
            let mut seen_ids = HashSet::new();
            let mut neighbors = Vec::with_capacity(limit_val);

            // Use zero-allocation iterators
            // Outgoing: edge -> target node
            let outgoing_iter = db
                .get_outgoing_edges_iter(nid)
                .map(|edge_id| db.get_edge_target(edge_id).ok());

            // Incoming: edge -> source node
            let incoming_iter = db
                .get_incoming_edges_iter(nid)
                .map(|edge_id| db.get_edge_source(edge_id).ok());

            // Chain iterators -> filter valid -> filter duplicates -> skip -> take
            let combined_iter = outgoing_iter
                .chain(incoming_iter)
                .flatten() // remove None (failed lookups)
                .filter(|&neighbor_id| seen_ids.insert(neighbor_id)) // deduplicate
                .skip(offset_val)
                .take(limit_val);

            for neighbor_id in combined_iter {
                match db.get_node(neighbor_id) {
                    Ok(node) => match node_to_json(&node) {
                        Ok(json) => neighbors.push(json),
                        Err(e) => {
                            return HttpResponse::InternalServerError()
                                .json(ApiResponse::error(e.to_string()));
                        }
                    },
                    Err(e) => {
                        // Node ID found in edge but not in node index? Should be impossible unless corrupted
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

/// Helper to convert a Node to JSON
fn node_to_json(node: &crate::core::Node) -> Result<serde_json::Value, String> {
    let props_json =
        property_map_to_json(&node.properties).map_err(|e| format!("Serialization error: {}", e))?;
    Ok(json!({
        "id": node.id.as_u64(),
        "label": interned_to_string(node.label),
        "properties": props_json
    }))
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

        // VULNERABLE: Returns 200 OK because the check was bypassed
        // SECURE: Should return 400 Bad Request

        assert!(
            resp.status().is_client_error(),
            "Should reject overflow attempt"
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

    #[test]
    fn test_validate_pagination_overflow() {
        // limit + offset = usize::MAX + 1 = 0 (wrapped) < 10000.
        // Saturating add makes it usize::MAX > 10000.
        let result = validate_pagination(Some(1), Some(usize::MAX), 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Pagination limit exceeded"));
    }

    #[test]
    fn test_validate_pagination_defaults() {
        let (limit, offset) = validate_pagination(None, None, 1000).unwrap();
        assert_eq!(limit, 100);
        assert_eq!(offset, 0);
    }

    #[test]
    fn test_validate_pagination_limits() {
        // Request > max_limit
        let (limit, _) = validate_pagination(Some(2000), None, 1000).unwrap();
        assert_eq!(limit, 1000); // Clamped

        // Request < max_limit
        let (limit, _) = validate_pagination(Some(50), None, 1000).unwrap();
        assert_eq!(limit, 50);
    }

    #[test]
    fn test_validate_pagination_deep_paging() {
        // limit + offset = 10001 > 10000
        let result = validate_pagination(Some(1), Some(10_000), 1000);
        assert!(result.is_err());
    }
