#[cfg(feature = "http-server")]
mod tests {
    use gallifreydb::GallifreyDB;
    use gallifreydb::http::{AppState, configure_app};
    use actix_web::{test, web, App};
    use std::sync::Arc;
    use serde_json::{json, Value};

    #[actix_rt::test]
    async fn test_find_node_dos_protection() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state = AppState::new(db.clone());

        // Insert 1005 nodes
        for _ in 0..1005 {
            let props = gallifreydb::core::PropertyMap::new();
            // Add a property to ensure uniqueness if needed, but not required for simple count check
            let _ = db.create_node("TestNode", props).unwrap();
        }

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure_app)
        ).await;

        // Test 1: Huge limit DoS
        // Request limit 2000. Should be capped at 1000.
        let req_payload = json!({
            "operation": "find_node",
            "label": "TestNode",
            "limit": 2000
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&req_payload)
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let body: Value = test::read_body_json(resp).await;
        let data = body.get("data").unwrap().as_array().unwrap();

        assert_eq!(data.len(), 1000, "Limit should be capped at 1000, got {}", data.len());

        // Test 2: Deep pagination DoS
        // Request offset 9500 + limit 1000 = 10500 > 10000 (max_deep_pagination)
        let req_payload = json!({
            "operation": "find_node",
            "label": "TestNode",
            "limit": 1000,
            "offset": 9500
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&req_payload)
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 400, "Should reject deep pagination, got {}", resp.status().as_u16());

        // Test 3: Integer Overflow DoS
        // Request offset usize::MAX.
        // With saturating_add, usize::MAX + limit > MAX_DEEP_PAGINATION.
        // With wrapping add, (usize::MAX + 100) might be small.
        let req_payload = json!({
            "operation": "find_node",
            "label": "TestNode",
            "limit": 100,
            "offset": usize::MAX
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&req_payload)
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 400, "Should reject overflow pagination, got {}", resp.status().as_u16());
    }
}
