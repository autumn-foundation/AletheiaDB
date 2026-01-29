#[cfg(feature = "http-server")]
mod tests {
    use actix_web::{App, test, web};
    use gallifreydb::GallifreyDB;
    use gallifreydb::http::{AppState, configure_app};
    use serde_json::json;
    use std::sync::Arc;

    #[actix_rt::test]
    async fn test_query_endpoint() {
        // Setup DB and App
        let db = Arc::new(GallifreyDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

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

        // Assert success: endpoint should return 200 OK
        assert_eq!(resp.status().as_u16(), 200, "Create node request failed");

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);

        // Extract node ID from response
        let node_id = body["data"][0]["id"].as_u64().expect("Should have node ID");

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

        // 4. Find Neighbors (create another node and edge first)
        let create_bob = json!({
            "operation": "create_node",
            "label": "Person",
            "properties": { "name": "Bob" }
        });

        // Helper to execute request
        // We can't easily capture app in a reusable closure due to async move and FnOnce/FnMut issues with futures
        // So we just inline the calls or define a macro, or just copy paste for now since it's only twice.

        // Create Bob
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&create_bob)
            .to_request();
        let resp = test::call_service(&app, req).await;

        let bob_body: serde_json::Value = test::read_body_json(resp).await;
        let bob_id = bob_body["data"][0]["id"].as_u64().unwrap();

        // Create edge (we need an endpoint for this too if we want to test find_neighbors properly via API)
        // Since create_edge wasn't explicitly requested in the "basic JSON query endpoint" description
        // but is needed for find_neighbors, I'll use the DB directly for setup or check if create_edge is supported.
        // The issue description lists: find_node, get_node, create_node, find_neighbors.
        // It DOES NOT list create_edge. So I'll use DB directly to set up the edge.

        db.create_edge(
            gallifreydb::core::NodeId::new(node_id).unwrap(),
            gallifreydb::core::NodeId::new(bob_id).unwrap(),
            "KNOWS",
            gallifreydb::core::PropertyMap::new(),
        )
        .unwrap();

        let neighbors_req = json!({
            "operation": "find_neighbors",
            "node_id": node_id
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&neighbors_req)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 200, "Find neighbors request failed");

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["success"], true);
        let neighbors = body["data"].as_array().expect("Data should be array");
        assert!(neighbors.iter().any(|n| n["id"].as_u64() == Some(bob_id)));
    }
}
