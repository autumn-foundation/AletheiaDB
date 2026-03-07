use actix_web::http::StatusCode;
use actix_web::{App, test};
use aletheiadb::db::AletheiaDB;
use aletheiadb::http::{AppState, configure_app};

#[actix_rt::test]
async fn test_json_payload_limit() {
    let db = AletheiaDB::new().unwrap();
    let app_state = actix_web::web::Data::new(AppState::new(std::sync::Arc::new(db)));

    let app = test::init_service(
        App::new()
            .app_data(app_state.clone())
            .configure(configure_app),
    )
    .await;

    // Create a JSON payload larger than 2MB
    let large_string = "x".repeat(3 * 1024 * 1024); // 3MB string
    let payload = format!(
        r#"{{"operation": "find_node", "label": "{}"}}"#,
        large_string
    );

    let req = test::TestRequest::post()
        .uri("/query")
        .insert_header(("Content-Type", "application/json"))
        .set_payload(payload)
        .to_request();

    let resp = test::call_service(&app, req).await;

    // Should return 413 Payload Too Large
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
