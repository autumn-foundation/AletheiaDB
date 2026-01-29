#[cfg(feature = "http-server")]
mod tests {
    use actix_web::{test, App, web};
    use gallifreydb::GallifreyDB;
    use gallifreydb::http::{AppState, configure_app};
    use std::sync::Arc;
    use serde_json::json;

    #[actix_rt::test]
    async fn test_query_endpoint() {
        // Setup DB and App
        let db = Arc::new(GallifreyDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app)
        ).await;

        // 1. Create Node
        let create_req = json!({
            "operation": "create_node",
            "label": "Person",
            "properties": {
                "name": "Alice",
                "age": 30
            }
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&create_req)
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200, "Create node request failed");

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);

        // Extract node ID from response (now a single object, not array)
        let node_id = body["data"]["id"].as_u64().expect("Should have node ID");

        // 2. Get Node
        let get_req = json!({
            "operation": "get_node",
            "node_id": node_id
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&get_req)
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200, "Get node request failed");

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        assert_eq!(body["data"]["id"].as_u64(), Some(node_id));
        assert_eq!(body["data"]["properties"]["name"], "Alice");

        // 3. Find Node
        let find_req = json!({
            "operation": "find_node",
            "label": "Person",
            "properties": {
                "name": "Alice"
            }
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
        assert!(nodes.iter().any(|n| n["id"].as_u64() == Some(node_id)));

        // 4. Find Neighbors
        let create_bob = json!({
            "operation": "create_node",
            "label": "Person",
            "properties": { "name": "Bob" }
        });

        // Helper to execute request - manually inline since we can't reuse closure easily
        let req = test::TestRequest::post().uri("/query").set_json(&create_bob).to_request();
        let resp = test::call_service(&app, req).await;
        let bob_body: serde_json::Value = test::read_body_json(resp).await;
        let bob_id = bob_body["data"]["id"].as_u64().unwrap();

        // Create edge directly
        db.create_edge(
            gallifreydb::core::NodeId::new(node_id).unwrap(),
            gallifreydb::core::NodeId::new(bob_id).unwrap(),
            "KNOWS",
            gallifreydb::core::PropertyMap::new()
        ).unwrap();

        let neighbors_req = json!({
            "operation": "find_neighbors",
            "node_id": node_id
        });

        let req = test::TestRequest::post().uri("/query").set_json(&neighbors_req).to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200, "Find neighbors request failed");

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        let neighbors = body["data"].as_array().expect("Data should be array");
        assert!(neighbors.iter().any(|n| n["id"].as_u64() == Some(bob_id)));
    }

    #[actix_rt::test]
    async fn test_error_cases() {
        let db = Arc::new(GallifreyDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());
        let app = test::init_service(
            App::new().app_data(web::Data::new(state)).configure(configure_app)
        ).await;

        // 1. Get non-existent node (404)
        let get_req = json!({
            "operation": "get_node",
            "node_id": 999999
        });
        let req = test::TestRequest::post().uri("/query").set_json(&get_req).to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 404, "Should return 404 for non-existent node");

        // 2. Unsupported property type (nested object) -> 400 Bad Request
        let create_req = json!({
            "operation": "create_node",
            "label": "Test",
            "properties": {
                "nested": { "foo": "bar" }
            }
        });
        let req = test::TestRequest::post().uri("/query").set_json(&create_req).to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400, "Should return 400 for nested object property");
    }

    #[actix_rt::test]
    async fn test_deduplication_and_pagination() {
        let db = Arc::new(GallifreyDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());
        let app = test::init_service(
            App::new().app_data(web::Data::new(state)).configure(configure_app)
        ).await;

        // Create nodes for pagination
        for i in 0..5 {
            db.create_node(
                "PaginationTest",
                gallifreydb::core::PropertyMapBuilder::new().insert("idx", i as i64).build()
            ).unwrap();
        }

        // Test Pagination
        let find_req = json!({
            "operation": "find_node",
            "label": "PaginationTest",
            "limit": 2,
            "offset": 1
        });
        let req = test::TestRequest::post().uri("/query").set_json(&find_req).to_request();
        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        let nodes = body["data"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "Should return 2 nodes");

        // Test Neighbor Deduplication
        let n1 = db.create_node("N1", gallifreydb::core::PropertyMap::new()).unwrap();
        let n2 = db.create_node("N2", gallifreydb::core::PropertyMap::new()).unwrap();

        // Create bidirectional edge (conceptually two edges in directed graph)
        db.create_edge(n1, n2, "KNOWS", gallifreydb::core::PropertyMap::new()).unwrap();
        db.create_edge(n2, n1, "KNOWS", gallifreydb::core::PropertyMap::new()).unwrap();

        let neighbors_req = json!({
            "operation": "find_neighbors",
            "node_id": n1.as_u64()
        });
        let req = test::TestRequest::post().uri("/query").set_json(&neighbors_req).to_request();
        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        let neighbors = body["data"].as_array().unwrap();

        // n2 should appear only once, despite bidirectional edges
        let n2_count = neighbors.iter().filter(|n| n["id"].as_u64() == Some(n2.as_u64())).count();
        assert_eq!(n2_count, 1, "Neighbor should appear exactly once");
    }
}
