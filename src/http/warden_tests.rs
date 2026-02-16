use crate::core::PropertyMapBuilder;
use crate::http::handlers::handle_query;
use crate::http::state::AppState;
use actix_web::{App, test, web};
use serde_json::json;

#[actix_rt::test]
async fn test_find_node_limit_capping() {
    let db = std::sync::Arc::new(crate::AletheiaDB::new().unwrap());
    let state = web::Data::new(AppState::new(db.clone()));

    // Populate 1500 nodes
    for i in 0..1500 {
        let props = PropertyMapBuilder::new().insert("index", i as i64).build();
        db.create_node("TestItem", props).unwrap();
    }

    let app = test::init_service(
        App::new()
            .app_data(state)
            .route("/query", web::post().to(handle_query)),
    )
    .await;

    // Request 1500 nodes (should be capped at 1000)
    let payload = json!({
        "operation": "find_node",
        "label": "TestItem",
        "limit": 1500
    });

    let req = test::TestRequest::post()
        .uri("/query")
        .set_json(&payload)
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let body = test::read_body(resp).await;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Check data array length
    let data = json["data"]
        .as_array()
        .expect("Response should contain data array");

    // Expectation: Capped at 1000
    assert!(
        data.len() <= 1000,
        "Limit should be capped at 1000, got {}",
        data.len()
    );
}
