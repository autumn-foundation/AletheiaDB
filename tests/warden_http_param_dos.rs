use axum::{body::Body, http::Request};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

use aletheiadb::AletheiaDB;
use aletheiadb::http::AppState;
use aletheiadb::http::ServerConfig;
use aletheiadb::http::build_test_router;

#[tokio::test]
async fn test_havoc_execute_query_with_large_param_array_dos() {
    let db = Arc::new(AletheiaDB::new().unwrap());

    let state = AppState::new(db);
    let config = ServerConfig::default();
    let app = build_test_router(state, &config).unwrap();

    let mut large_array: Vec<serde_json::Value> = vec![];
    for _ in 0..10_000_001 {
        // Exceeding MAX_ARRAY_ELEMENTS = 10_000_000
        large_array.push(json!("hello"));
    }

    let payload = json!({
        "operation": "execute_query",
        "query": "CREATE (n:Person)",
        "parameters": {
            "large": large_array
        }
    });

    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();

    let response = app.clone().oneshot(req).await;
    let resp = response.unwrap();
    assert!(
        resp.status().is_client_error(),
        "Should reject large parameter array"
    );
}

#[tokio::test]
async fn test_havoc_execute_query_with_large_param_map_dos() {
    let db = Arc::new(AletheiaDB::new().unwrap());

    let state = AppState::new(db);
    let config = ServerConfig::default();
    let app = build_test_router(state, &config).unwrap();

    let mut large_map = serde_json::Map::new();
    for i in 0..100_001 {
        // Testing 100,001 which is over MAX_PROPERTY_MAP_CAPACITY
        large_map.insert(format!("key{}", i), json!(1.0));
    }

    let payload = json!({
        "operation": "execute_query",
        "query": "CREATE (n:Person)",
        "parameters": large_map
    });

    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();

    let response = app.clone().oneshot(req).await;
    let resp = response.unwrap();
    assert!(
        resp.status().is_client_error(),
        "Should reject large parameter map"
    );
}

#[tokio::test]
async fn test_havoc_execute_query_with_large_param_nested_array_dos() {
    let db = Arc::new(AletheiaDB::new().unwrap());

    let state = AppState::new(db);
    let config = ServerConfig::default();
    let app = build_test_router(state, &config).unwrap();

    let mut nested = json!(1);
    for _ in 0..101 {
        // Over MAX_JSON_RECURSION_DEPTH of 100
        nested = json!([nested]);
    }

    let payload = json!({
        "operation": "execute_query",
        "query": "CREATE (n:Person)",
        "parameters": {
            "large": nested
        }
    });

    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();

    let response = app.clone().oneshot(req).await;
    let resp = response.unwrap();
    assert!(
        resp.status().is_client_error(),
        "Should reject large recursive parameter map"
    );
}
