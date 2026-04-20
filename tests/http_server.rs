//! Integration tests for the HTTP server (Issue #465).
//!
//! Exercises the HTTP surface against a fully-wired `axum::Router` built via
//! `aletheiadb::http::build_test_router` and fired at through
//! `autumn_web::test::TestApp::from_router`.
//!
//! Run with: `cargo test --test http_server --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use autumn_web::test::TestApp;
use axum::http::StatusCode;
use serde_json::Value;

fn test_client() -> autumn_web::test::TestClient {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let state = AppState::new(db);
    let config = ServerConfig::default();
    let router = build_test_router(state, &config).expect("router builds");
    TestApp::from_router(router)
}

/// Health endpoint returns 200 + `{"status":"healthy"}`.
#[tokio::test]
async fn health_endpoint_returns_healthy_status() {
    let client = test_client();
    let resp = client.get("/status").send().await;

    assert_eq!(resp.status, StatusCode::OK);

    let body: Value = serde_json::from_slice(&resp.body).expect("response is JSON");
    assert_eq!(body.get("status").and_then(Value::as_str), Some("healthy"));
}

/// Health endpoint sets `content-type: application/json`.
#[tokio::test]
async fn health_endpoint_returns_json_content_type() {
    let client = test_client();
    let resp = client.get("/status").send().await;

    let content_type = resp
        .headers
        .iter()
        .find_map(|(k, v)| (k.eq_ignore_ascii_case("content-type")).then_some(v.as_str()))
        .expect("Content-Type header present");
    assert!(
        content_type.contains("application/json"),
        "Content-Type should be application/json, got: {content_type}"
    );
}

/// CORS preflight response carries the expected allow-origin/allow-methods
/// headers when configured with the default (restrictive) policy.
#[tokio::test]
async fn cors_headers_present() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let state = AppState::new(db);
    let config = ServerConfig::builder()
        .cors(aletheiadb::http::CorsConfig::permissive())
        .build();
    let router = build_test_router(state, &config).unwrap();
    let client = TestApp::from_router(router);

    let resp = client
        .get("/status")
        .header("Origin", "http://example.com")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await;

    let has_cors = resp.headers.iter().any(|(k, _)| {
        k.eq_ignore_ascii_case("access-control-allow-origin")
            || k.eq_ignore_ascii_case("access-control-allow-methods")
    });
    assert!(has_cors, "CORS headers should be present in response");
}

#[tokio::test]
async fn server_config_default_port() {
    assert_eq!(ServerConfig::default().port(), 8080);
}

#[tokio::test]
async fn server_config_custom_port() {
    assert_eq!(ServerConfig::new(3000).port(), 3000);
}

#[tokio::test]
async fn server_config_port_range() {
    for port in [0u16, 80, 443, 65535] {
        assert_eq!(ServerConfig::new(port).port(), port);
    }
}

#[tokio::test]
async fn server_config_builder() {
    let config = ServerConfig::builder().port(9000).host("127.0.0.1").build();
    assert_eq!(config.port(), 9000);
    assert_eq!(config.host(), "127.0.0.1");
}

#[tokio::test]
async fn server_config_default_host() {
    assert_eq!(ServerConfig::default().host(), "0.0.0.0");
}

/// The health handler, exercised through the HTTP stack, returns a JSON body
/// whose `status` field is `"healthy"`. (Replaces the actix-era
/// direct-handler-invocation test — autumn handlers return `IntoResponse`
/// types, not `HttpResponse`, so the direct call no longer has a status.)
#[tokio::test]
async fn health_endpoint_body_is_healthy() {
    let client = test_client();
    let resp = client.get("/status").send().await;
    assert_eq!(resp.status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["status"], "healthy");
}

/// Multiple consecutive requests pass through the middleware chain without
/// breaking (regression for middleware-induced failures).
#[tokio::test]
async fn middleware_chain_survives_repeated_requests() {
    let client = test_client();
    for _ in 0..3 {
        let resp = client.get("/status").send().await;
        assert_eq!(resp.status, StatusCode::OK);
    }
}

#[tokio::test]
async fn unknown_route_returns_404() {
    let client = test_client();
    let resp = client.get("/nonexistent-route").send().await;
    assert_eq!(resp.status, StatusCode::NOT_FOUND);
}

// ============================================================================
// AppState concurrency (Issue #466) — framework-agnostic, exercises interior
// mutability of AletheiaDB under shared-state load.
// ============================================================================

#[cfg(test)]
mod state_tests {
    use aletheiadb::AletheiaDB;
    use aletheiadb::PropertyMapBuilder;
    use aletheiadb::http::AppState;
    use std::sync::Arc;

    #[test]
    fn app_state_creation() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);
        assert_eq!(state.db().node_count(), 0);
    }

    #[test]
    fn app_state_is_clone() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);
        let state2 = state.clone();

        let node_id = state
            .db()
            .create_node("Test", PropertyMapBuilder::new().build())
            .unwrap();
        assert_eq!(state2.db().node_count(), 1);
        let node = state2.db().get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
    }

    #[tokio::test]
    async fn app_state_concurrent_reads() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        for i in 0..100i64 {
            db.create_node("Test", PropertyMapBuilder::new().insert("id", i).build())
                .unwrap();
        }
        let state = AppState::new(db);

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let state = state.clone();
                tokio::spawn(async move {
                    for _ in 0..100 {
                        assert_eq!(state.db().node_count(), 100);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.await.expect("task should not panic");
        }
    }

    #[tokio::test]
    async fn app_state_concurrent_writes() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);

        let handles: Vec<_> = (0..10)
            .map(|task_id| {
                let state = state.clone();
                tokio::spawn(async move {
                    for iter in 0..10i64 {
                        state
                            .db()
                            .create_node(
                                "Test",
                                PropertyMapBuilder::new()
                                    .insert("task", task_id as i64)
                                    .insert("iter", iter)
                                    .build(),
                            )
                            .unwrap();
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.await.expect("task should not panic");
        }

        assert_eq!(state.db().node_count(), 100);
    }

    #[tokio::test]
    async fn app_state_mixed_concurrent_operations() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);

        let mut handles = Vec::new();

        for task_id in 0..5 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                for iter in 0..20i64 {
                    state
                        .db()
                        .create_node(
                            "Writer",
                            PropertyMapBuilder::new()
                                .insert("task", task_id as i64)
                                .insert("iter", iter)
                                .build(),
                        )
                        .unwrap();
                }
            }));
        }

        for _ in 0..5 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..50 {
                    let _ = state.db().node_count();
                }
            }));
        }

        for handle in handles {
            handle.await.expect("task should not panic");
        }

        assert_eq!(state.db().node_count(), 100);
    }

    #[tokio::test]
    async fn no_deadlock_under_load() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);

        let handles: Vec<_> = (0..20)
            .map(|task_id| {
                let state = state.clone();
                tokio::spawn(async move {
                    for iter in 0..10i64 {
                        state
                            .db()
                            .create_node(
                                "Load",
                                PropertyMapBuilder::new()
                                    .insert("task", task_id as i64)
                                    .insert("iter", iter)
                                    .build(),
                            )
                            .unwrap();
                        let _ = state.db().node_count();
                    }
                })
            })
            .collect();

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            for handle in handles {
                handle.await.expect("task should not panic");
            }
        })
        .await;

        assert!(result.is_ok(), "tasks should complete without deadlock");
        assert_eq!(state.db().node_count(), 200);
    }

    #[test]
    fn app_state_from_arc() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state: AppState = db.into();
        assert_eq!(state.db().node_count(), 0);
    }
}
