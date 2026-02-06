#[cfg(feature = "http-server")]
mod tests {
    use actix_web::{App, test, web};
    use aletheiadb::AletheiaDB;
    use aletheiadb::http::{AppState, configure_app};
    use serde_json::json;
    use std::sync::Arc;

    #[actix_rt::test]
    async fn test_find_node_result_limit_enforcement() {
        // Setup in-memory DB
        let db = Arc::new(AletheiaDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

        // Populate DB with 1500 nodes (exceeding the standard 1000 limit)
        // We use a label "DosTarget" to easily scan them
        let count = 1500;
        for i in 0..count {
            db.create_node(
                "DosTarget",
                aletheiadb::core::PropertyMapBuilder::new()
                    .insert("idx", i as i64)
                    .build(),
            )
            .unwrap();
        }

        // Send FindNode request with limit > 1000
        let find_req = json!({
            "operation": "find_node",
            "label": "DosTarget",
            "limit": 2000,
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&find_req)
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200, "Find node request failed");

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);

        let nodes = body["data"].as_array().expect("Data should be array");

        // VULNERABILITY CHECK:
        // If vulnerabilities exists: returns 1500 (all nodes) because we asked for 2000
        // If fortified: returns 1000 (MAX_RESULT_LIMIT)
        let returned_count = nodes.len();
        println!("Returned count: {}", returned_count);

        // This assertion expects the SAFE behavior (1000).
        // It will FAIL initially (Red Phase).
        assert_eq!(returned_count, 1000, "Should cap results at 1000 to prevent DoS");
    }
}
