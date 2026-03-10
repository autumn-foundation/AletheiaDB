use actix_web::{App, test};
use aletheiadb::db::AletheiaDB;
use aletheiadb::http::AppState;
use aletheiadb::http::configure_app;
use std::sync::Arc;

#[actix_rt::test]
async fn test_havoc_json_bomb() {
    let db = AletheiaDB::new().unwrap();
    let app_state = actix_web::web::Data::new(AppState::new(Arc::new(db)));
    let app = test::init_service(App::new().app_data(app_state).configure(configure_app)).await;

    // Send a large JSON string to test max limit. But actix-web's default json limit is usually 2MB or so, so maybe it's not unlimited. Let's send 10MB.
    let huge_string = "A".repeat(10 * 1024 * 1024);
    let payload = format!(
        r#"{{"operation": "find_node", "node_id": 1, "junk": "{}"}}"#,
        huge_string
    );

    let req = test::TestRequest::post()
        .uri("/query")
        .insert_header(("Content-Type", "application/json"))
        .set_payload(payload)
        .to_request();

    let resp: actix_web::dev::ServiceResponse = test::call_service(&app, req).await;
    println!("Status: {}", resp.status());
    // Since the system has no explicit JsonConfig, the limit defaults to whatever actix-web uses (usually 2MB).
    // If it's 413, that means actix-web protected it. If it's 400 because of JSON parsing failure, it still means memory was allocated.
    assert!(resp.status().is_client_error(), "Status: {}", resp.status());
}
