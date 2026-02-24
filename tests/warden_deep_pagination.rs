#![cfg(feature = "http-server")]

use actix_web::{App, test, web};
use aletheiadb::AletheiaDB;
use aletheiadb::http::AppState;
use serde_json::json;
use std::sync::Arc;

#[actix_rt::test]
async fn test_warden_execute_query_deep_pagination_dos() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let state = web::Data::new(AppState::new(db));

    let app = test::init_service(App::new().app_data(state).route(
        "/query",
        web::post().to(aletheiadb::http::handlers::handle_query),
    ))
    .await;

    // A query with a massive SKIP that would cause the server to loop for a long time
    // if there were enough nodes (or just allocate if implemented naively).
    // Even with few nodes, iterating 100M times in LimitIterator consumes CPU.
    let payload = json!({
        "operation": "execute_query",
        "query": "MATCH (n) RETURN n SKIP 100000000 LIMIT 10"
    });

    let req = test::TestRequest::post()
        .uri("/query")
        .set_json(&payload)
        .to_request();

    let resp = test::call_service(&app, req).await;

    // CURRENT BEHAVIOR: Returns 200 OK (with empty result) but consumes resources
    // DESIRED BEHAVIOR: Should return 400 Bad Request

    if resp.status().is_success() {
        // We want this test to fail currently to demonstrate the issue
        panic!("VULNERABILITY CONFIRMED: Server accepted deep pagination query");
    } else {
        assert!(
            resp.status().is_client_error(),
            "Should reject deep pagination"
        );
        let body = test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let error = json["error"].as_str().unwrap();
        // Check for either the handler-level error or the planner-level error
        let passed = error.contains("Pagination limit exceeded")
            || (error.contains("Invalid query parameter")
                && error.contains("exceeds maximum allowed"));
        assert!(passed, "Unexpected error message: {}", error);
    }
}
