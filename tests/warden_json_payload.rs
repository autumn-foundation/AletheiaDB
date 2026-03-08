#![cfg(feature = "http-server")]

use actix_web::dev::ServiceResponse;
use actix_web::{App, test, web};
use aletheiadb::http::AppState;
use aletheiadb::http::configure_app;

#[actix_rt::test]
async fn test_large_json_payload() {
    let db = std::sync::Arc::new(aletheiadb::AletheiaDB::new().unwrap());
    let state = web::Data::new(AppState::new(db));

    // Initialize the app with configure_app where our limits are set
    let app = test::init_service(App::new().app_data(state).configure(configure_app)).await;

    // A standard massive payload (3MB), larger than the 2MB limit
    let huge_string = "a".repeat(3 * 1024 * 1024);
    let payload = format!(
        r#"{{"operation": "execute_query", "query": "{}"}}"#,
        huge_string
    );

    let req = test::TestRequest::post()
        .uri("/query")
        .insert_header(("Content-Type", "application/json"))
        .set_payload(payload)
        .to_request();

    let resp: ServiceResponse = test::call_service(&app, req).await;

    // The request should be rejected as Payload Too Large (413)
    assert_eq!(resp.status().as_u16(), 413, "Should be Payload Too Large");
}
