#[cfg(feature = "http-server")]
mod sentinel_tests {
    use actix_web::{App, http::StatusCode, test, web};
    use aletheiadb::AletheiaDB;
    use aletheiadb::http::{AppState, configure_app};
    use serde_json::json;
    use std::sync::Arc;

    #[actix_rt::test]
    async fn test_json_to_predicate_value() {
        let db = Arc::new(AletheiaDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

        let create_payload = json!({
            "operation": "create_node",
            "label": "TestNode",
            "properties": {
                "prop_null": null,
                "prop_bool": true,
                "prop_int": 42,
                "prop_float": 1.23_f64,
                "prop_string": "hello"
            }
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(create_payload)
            .to_request();
        let _ = test::call_service(&app, req).await;

        // Test array property to hit the None arm
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "properties": {
                    "prop_array": [1, 2, 3]
                }
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        // Arrays map to None, which causes `if let Some(pred_value) = json_to_predicate_value` to just ignore it.
        // It doesn't fail, it just drops the filter. So it returns 200 OK.
        assert_eq!(res.status(), StatusCode::OK);

        // Test int
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "properties": {
                    "prop_int": 42
                }
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Test float
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "properties": {
                    "prop_float": 1.23_f64
                }
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Test null
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "properties": {
                    "prop_null": null
                }
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Test bool
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "properties": {
                    "prop_bool": true
                }
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Test string
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "properties": {
                    "prop_string": "hello"
                }
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[actix_rt::test]
    async fn test_execute_query_parse_error() {
        let db = Arc::new(AletheiaDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "execute_query",
                "query": "MATCH # INVALID"
            }))
            .to_request();
        let res = test::call_service(&app, req).await;

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let body: serde_json::Value = test::read_body_json(res).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("parse")
                || body["error"]
                    .as_str()
                    .unwrap()
                    .to_lowercase()
                    .contains("syntax")
        );
    }

    #[actix_rt::test]
    async fn test_warden_find_node_deep_pagination() {
        let db = Arc::new(AletheiaDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

        // Limit + offset == 10000 (Exact boundary, should succeed)
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "offset": 9900,
                "limit": 100
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Limit + offset == 10001 (Overflow, should fail with BAD_REQUEST)
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_node",
                "label": "TestNode",
                "offset": 9901,
                "limit": 100
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_rt::test]
    async fn test_warden_find_neighbors_overflow() {
        let db = Arc::new(AletheiaDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

        // Ensure node 1 exists
        let create_req = json!({
            "operation": "create_node",
            "label": "Person",
            "properties": {}
        });
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(create_req)
            .to_request();
        let _ = test::call_service(&app, req).await;

        // Limit + offset == 10000 (Exact boundary, should succeed)
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_neighbors",
                "node_id": 1,
                "offset": 9900,
                "limit": 100
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Limit + offset == 10001 (Overflow, should fail)
        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(json!({
                "operation": "find_neighbors",
                "node_id": 1,
                "offset": 9901,
                "limit": 100
            }))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
