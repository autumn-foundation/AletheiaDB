//! HTTP request handlers.

use crate::core::NodeId;
use crate::http::converters::{interned_to_string, json_to_property_map, property_map_to_json};
use crate::http::state::AppState;
use crate::query::QueryBuilder;
use crate::query::ir::{Predicate, PredicateValue};
use actix_web::{HttpResponse, web};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

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

/// Query endpoint handler.
pub async fn handle_query(
    state: web::Data<AppState>,
    req: web::Json<QueryRequest>,
) -> HttpResponse {
    let db = state.db();

    match req.into_inner() {
        QueryRequest::CreateNode { label, properties } => {
            let props = properties
                .map(|p| json_to_property_map(&p))
                .unwrap_or_default();

            match db.create_node(&label, props) {
                Ok(node_id) => {
                    // Fetch the created node to return full details
                    match db.get_node(node_id) {
                        Ok(node) => {
                            let node_json = json!({
                                "id": node.id.as_u64(),
                                "label": interned_to_string(node.label),
                                "properties": property_map_to_json(&node.properties)
                            });
                            HttpResponse::Ok().json(ApiResponse::success(node_json))
                        }
                        Err(e) => HttpResponse::InternalServerError()
                            .json(ApiResponse::error(e.to_string())),
                    }
                }
                Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
            }
        }
        QueryRequest::GetNode { node_id } => {
            match NodeId::new(node_id) {
                Ok(nid) => match db.get_node(nid) {
                    Ok(node) => {
                        let node_json = json!({
                            "id": node.id.as_u64(),
                            "label": interned_to_string(node.label),
                            "properties": property_map_to_json(&node.properties)
                        });
                        HttpResponse::Ok().json(ApiResponse::success(node_json))
                    }
                    Err(e) => HttpResponse::NotFound().json(ApiResponse::error(e.to_string())),
                },
                Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
            }
        }
        QueryRequest::FindNode { label, properties } => {
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

            match builder.limit(100).execute(db) {
                Ok(results) => {
                    let mut nodes = Vec::new();
                    for row_result in results {
                        match row_result {
                            Ok(row) => {
                                if let crate::query::executor::EntityResult::Node(node) = row.entity {
                                    nodes.push(json!({
                                        "id": node.id.as_u64(),
                                        "label": interned_to_string(node.label),
                                        "properties": property_map_to_json(&node.properties)
                                    }));
                                }
                            }
                            Err(e) => {
                                return HttpResponse::InternalServerError()
                                    .json(ApiResponse::error(e.to_string()));
                            }
                        }
                    }
                    HttpResponse::Ok().json(ApiResponse::success(json!(nodes)))
                }
                Err(e) => {
                    HttpResponse::InternalServerError().json(ApiResponse::error(e.to_string()))
                }
            }
        }
        QueryRequest::FindNeighbors { node_id } => {
            match NodeId::new(node_id) {
                Ok(nid) => {
                    // Get outgoing and incoming edges to find connected nodes
                    // For simplicity, we just return the connected nodes
                    let mut neighbors = Vec::new();

                    // Outgoing
                    let outgoing = db.get_outgoing_edges(nid);
                    for edge_id in outgoing {
                        if let Ok(target) = db.get_edge_target(edge_id) {
                            if let Ok(node) = db.get_node(target) {
                                neighbors.push(json!({
                                    "id": node.id.as_u64(),
                                    "label": interned_to_string(node.label),
                                    "properties": property_map_to_json(&node.properties)
                                }));
                            }
                        }
                    }

                    // Incoming
                    let incoming = db.get_incoming_edges(nid);
                    for edge_id in incoming {
                        if let Ok(source) = db.get_edge_source(edge_id) {
                            if let Ok(node) = db.get_node(source) {
                                neighbors.push(json!({
                                    "id": node.id.as_u64(),
                                    "label": interned_to_string(node.label),
                                    "properties": property_map_to_json(&node.properties)
                                }));
                            }
                        }
                    }

                    // Deduplicate? For now just return list
                    HttpResponse::Ok().json(ApiResponse::success(json!(neighbors)))
                }
                Err(e) => HttpResponse::BadRequest().json(ApiResponse::error(e.to_string())),
            }
        }
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
