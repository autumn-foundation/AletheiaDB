#![cfg(feature = "http-server")]

use actix_web::{App, test, web};
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::http::AppState;
use aletheiadb::http::handlers::handle_query;
use serde_json::json;

#[actix_rt::test]
async fn test_warden_execute_query_result_limit() {
    let db = std::sync::Arc::new(aletheiadb::AletheiaDB::new().unwrap());

    // Insert 1001 nodes (using a smaller limit for test speed, assuming handler will use 1000)
    // Wait, the plan says 10,000. Let's stick to 10,000 for robustness, unless it's too slow.
    // 10,000 nodes insertion is fast in-memory.
    // However, I will implement a constant in `src/http/handlers.rs` which might be 1000 or 10000.
    // Let's assume 1000 for the test to be faster, and I'll set the constant to 1000 first,
    // or maybe I'll make it 10000 and test with 10001.
    // Let's go with 10001. It should be fine.

    // Pre-populate DB
    // Batch this? No batch API exposed easily here. Loop is fine.
    // Warden: Insert enough nodes to exceed the 10,000 limit.
    for i in 0..10005 {
        let props = PropertyMapBuilder::new().insert("id", i as i64).build();
        db.create_node("TestNode", props).unwrap();
    }

    let state = web::Data::new(AppState::new(db));
    let app = test::init_service(
        App::new()
            .app_data(state)
            .route("/query", web::post().to(handle_query)),
    )
    .await;

    // Execute query that matches all nodes
    let payload = json!({
        "operation": "execute_query",
        "query": "MATCH (n:TestNode) RETURN n"
    });

    let req = test::TestRequest::post()
        .uri("/query")
        .set_json(&payload)
        .to_request();

    let resp = test::call_service(&app, req).await;

    // CURRENT BEHAVIOR: Returns 200 OK with > 1000 results.
    // DESIRED BEHAVIOR: Returns 400 Bad Request (or 500) with error about limit.

    // If I implement a limit of 1000, this test should fail if it returns 200.
    // But initially, it will return 200.
    // So to prove vulnerability, I assert failure.

    if resp.status().is_success() {
        let body = test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let data = json["data"].as_array().unwrap();

        // Vulnerability confirmed if we get all 10005 results
        if data.len() >= 10005 {
            panic!(
                "VULNERABILITY CONFIRMED: Query returned {} results, exceeding expected limit",
                data.len()
            );
        }
    } else {
        // If it fails, check error message
        let body = test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let error = json["error"].as_str().unwrap();
        println!("Got error as expected: {}", error);
    }
}
