//! Security reproduction test for Unbounded Allocation DoS in FindNode.
//!
//! This test verifies that `FindNode` currently allows unbounded limits,
//! which is a DoS vector. After the fix, it should verify that limits are enforced.

#![cfg(feature = "http-server")]

use actix_web::{App, test, web};
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use gallifreydb::http::{AppState, configure_app};
use std::sync::Arc;
use serde_json::Value;

#[actix_rt::test]
async fn test_find_node_limit_clamping() {
    // 1. Setup DB with 1100 nodes
    let db = Arc::new(GallifreyDB::new().unwrap());

    for i in 0..1100 {
        db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("id", i as i64)
                .build()
        ).unwrap();
    }

    let state = AppState::new(db);

    // 2. Setup App using the public configuration function
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure_app)
    ).await;

    // 3. Send request with limit: 2000 (exceeding safety limit of 1000)
    let req_body = serde_json::json!({
        "operation": "find_node",
        "label": "Person",
        "limit": 2000
    });

    let req = test::TestRequest::post()
        .uri("/query")
        .set_json(&req_body)
        .to_request();

    // 4. Verify vulnerability protection: Returns 1000 nodes (clamped)
    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body: Value = test::read_body_json(resp).await;
    let data = body.get("data").expect("Should have data").as_array().expect("Data should be array");

    if data.len() == 1100 {
        panic!("⚠️ VULNERABILITY CONFIRMED: FindNode returned 1100 nodes (unbounded limit).");
    } else if data.len() == 1000 {
        println!("✅ SECURE: FindNode clamped results to 1000.");
    } else {
        panic!("Unexpected result count: {}", data.len());
    }
}

#[actix_rt::test]
async fn test_find_node_deep_pagination_prevention() {
    let db = Arc::new(GallifreyDB::new().unwrap());
    let state = AppState::new(db);
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure_app)
    ).await;

    // Request offset=9999, limit=100 -> Total 10099 > 10000 -> Bad Request
    let req_body = serde_json::json!({
        "operation": "find_node",
        "limit": 100,
        "offset": 9999
    });

    let req = test::TestRequest::post()
        .uri("/query")
        .set_json(&req_body)
        .to_request();

    let resp = test::call_service(&app, req).await;

    // Expect 400 Bad Request
    assert_eq!(resp.status().as_u16(), 400);

    let body: Value = test::read_body_json(resp).await;
    let error = body.get("error").expect("Should have error").as_str().expect("Error should be string");

    assert!(error.contains("Pagination limit exceeded"));
    println!("✅ SECURE: Deep pagination rejected.");
}
