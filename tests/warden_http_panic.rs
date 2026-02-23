#[cfg(feature = "http-server")]
mod tests {
    use actix_web::{App, test, web};
    use aletheiadb::AletheiaDB;
    use aletheiadb::http::{AppState, configure_app};
    use serde_json::json;
    use std::sync::Arc;

    #[actix_rt::test]
    async fn test_invalid_json_body() {
        // Setup DB and App
        let db = Arc::new(AletheiaDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

        // 1. Invalid JSON Syntax
        let req = test::TestRequest::post()
            .uri("/query")
            .set_payload("{ \"operation\": \"create_node\", \"label\": \"Person\", }") // Trailing comma
            .insert_header(("Content-Type", "application/json"))
            .to_request();

        let resp = test::call_service(&app, req).await;
        // Should return 400 Bad Request (handled by actix Json extractor)
        assert_eq!(
            resp.status().as_u16(),
            400,
            "Should handle invalid JSON syntax"
        );

        // 2. Valid JSON but missing required fields for the specific variant
        // "FindNode" requires nothing (all optional), but "CreateNode" requires "label".
        let req_body = json!({
            "operation": "create_node",
            // Missing "label"
            "properties": {}
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&req_body)
            .to_request();

        let resp = test::call_service(&app, req).await;
        // Should return 400 Bad Request (handled by serde untagged enum or variant validation)
        assert_eq!(
            resp.status().as_u16(),
            400,
            "Should handle missing required fields"
        );
    }

    #[actix_rt::test]
    async fn test_wrong_type_fields() {
        let db = Arc::new(AletheiaDB::new().expect("Failed to create DB"));
        let state = AppState::new(db.clone());

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_app),
        )
        .await;

        // 3. Wrong type for field
        let req_body = json!({
            "operation": "create_node",
            "label": 12345, // Should be string
            "properties": {}
        });

        let req = test::TestRequest::post()
            .uri("/query")
            .set_json(&req_body)
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(
            resp.status().as_u16(),
            400,
            "Should handle wrong type fields"
        );
    }
}
