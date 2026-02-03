//! HTTP request handlers.

use crate::GallifreyDB;
use crate::core::{Node, NodeId, PropertyMap};
use crate::http::converters::{interned_to_string, json_to_property_map, property_map_to_json};
use crate::http::state::AppState;
use crate::query::QueryBuilder;
use crate::query::ir::{Predicate, PredicateValue};
use actix_web::{HttpResponse, web};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum QueryRequest {
    FindNode {
        label: Option<String>,
        properties: Option<HashMap<String, serde_json::Value>>,
        limit: Option<usize>,
        offset: Option<usize>,
    },
    GetNode {
        node_id: u64,
    },
    CreateNode {
        label: String,
        properties: Option<HashMap<String, serde_json::Value>>,
    },
    FindNeighbors {
        node_id: u64,
    },
}

#[derive(Debug, Serialize)]
pub struct ApiResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ApiResponse {
    fn success(data: serde_json::Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

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

/// Helper to convert a Node to JSON.
fn node_to_json(node: &Node) -> Result<Value, HttpResponse> {
    let props_json = property_map_to_json(&node.properties)
        .map_err(|e| HttpResponse::InternalServerError().json(ApiResponse::error(e)))?;

    Ok(json!({
        "id": node.id.as_u64(),
        "label": interned_to_string(node.label),
        "properties": props_json
    }))
}

fn handle_create_node(
    db: &GallifreyDB,
    label: String,
    properties: Option<HashMap<String, Value>>,
) -> HttpResponse {
    let props = match properties {
        Some(p) => match json_to_property_map(&p) {
            Ok(map) => map,
            Err(e) => return HttpResponse::BadRequest().json(ApiResponse::error(e)),
        },
        None => PropertyMap::new(),
    };

    let node_id = match db.create_node(&label, props) {
        Ok(id) => id,
        Err(e) => return HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    };

    let node = match db.get_node(node_id) {
        Ok(n) => n,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string()));
        }
    };

    match node_to_json(&node) {
        Ok(json) => HttpResponse::Ok().json(ApiResponse::success(json)),
        Err(resp) => resp,
    }
}

fn handle_get_node(db: &GallifreyDB, node_id: u64) -> HttpResponse {
    let nid = match NodeId::new(node_id) {
        Ok(id) => id,
        Err(e) => return HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    };

    let node = match db.get_node(nid) {
        Ok(n) => n,
        // Issue 1: Return 404 for node not found
        Err(_) => {
            return HttpResponse::NotFound()
                .json(ApiResponse::error(format!("Node {} not found", node_id)));
        }
    };

    match node_to_json(&node) {
        Ok(json) => HttpResponse::Ok().json(ApiResponse::success(json)),
        Err(resp) => resp,
    }
}

fn handle_find_node(
    db: &GallifreyDB,
    label: Option<String>,
    properties: Option<HashMap<String, Value>>,
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
    if let Some(skip) = offset {
        builder = builder.skip(skip);
    }

    let limit_val = limit.unwrap_or(100);
    let results = match builder.limit(limit_val).execute(db) {
        Ok(res) => res,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string()));
        }
    };

    let mut nodes = Vec::new();
    for row in results.flatten() {
        if let crate::query::executor::EntityResult::Node(node) = row.entity {
            match node_to_json(&node) {
                Ok(json) => nodes.push(json),
                Err(resp) => return resp,
            }
        }
    }
    HttpResponse::Ok().json(ApiResponse::success(json!(nodes)))
}

fn handle_find_neighbors(db: &GallifreyDB, node_id: u64) -> HttpResponse {
    let nid = match NodeId::new(node_id) {
        Ok(id) => id,
        Err(e) => return HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
    };

    // Issue 2: Deduplication
    // Use a HashSet to track seen node IDs
    let mut seen_ids = HashSet::new();
    let mut neighbors = Vec::new();

    // Helper closure to process edges
    let mut process_edge = |node_result: crate::utils::Result<Node>| -> Result<(), HttpResponse> {
        if let Ok(node) = node_result {
            let id = node.id.as_u64();
            if seen_ids.insert(id) {
                match node_to_json(&node) {
                    Ok(json) => {
                        neighbors.push(json);
                    }
                    Err(resp) => return Err(resp),
                }
            }
        }
        Ok(())
    };

    // Outgoing
    for edge_id in db.get_outgoing_edges(nid) {
        let node_res = db
            .get_edge_target(edge_id)
            .and_then(|target| db.get_node(target));
        if let Err(resp) = process_edge(node_res) {
            return resp;
        }
    }

    // Incoming
    for edge_id in db.get_incoming_edges(nid) {
        let node_res = db
            .get_edge_source(edge_id)
            .and_then(|source| db.get_node(source));
        if let Err(resp) = process_edge(node_res) {
            return resp;
        }
    }

    HttpResponse::Ok().json(ApiResponse::success(json!(neighbors)))
}

/// Query endpoint handler.
pub async fn handle_query(
    state: web::Data<AppState>,
    req: web::Json<QueryRequest>,
) -> HttpResponse {
    let db = state.db();

    match req.into_inner() {
        QueryRequest::CreateNode { label, properties } => handle_create_node(db, label, properties),
        QueryRequest::GetNode { node_id } => handle_get_node(db, node_id),
        QueryRequest::FindNode {
            label,
            properties,
            limit,
            offset,
        } => handle_find_node(db, label, properties, limit, offset),
        QueryRequest::FindNeighbors { node_id } => handle_find_neighbors(db, node_id),
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
}
