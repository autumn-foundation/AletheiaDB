//! Integration tests for HTTP server (Issue #465)
//!
//! This test module verifies the HTTP server functionality:
//! - Health endpoint returns correct response
//! - Server starts on configurable port
//! - CORS headers are properly set
//! - Graceful shutdown handling
//!
//! Run with: cargo test --test http_server --features http-server

#![cfg(feature = "http-server")]

use actix_web::{App, test};
use gallifreydb::http::{ServerConfig, configure_app, create_app, health_check};
use serde_json::Value;

/// Test that health endpoint returns 200 OK with {"status": "healthy"}
#[actix_rt::test]
async fn test_health_endpoint_returns_healthy_status() {
    // Create test app with our configuration
    let app = test::init_service(App::new().configure(configure_app)).await;

    // Make request to health endpoint
    let req = test::TestRequest::get().uri("/status").to_request();
    let resp = test::call_service(&app, req).await;

    // Assert response status is 200 OK
    assert!(
        resp.status().is_success(),
        "Health endpoint should return 200 OK"
    );
    assert_eq!(resp.status().as_u16(), 200);

    // Parse response body
    let body = test::read_body(resp).await;
    let json: Value = serde_json::from_slice(&body).expect("Response should be valid JSON");

    // Assert response body contains status: healthy
    assert_eq!(
        json.get("status").and_then(|v| v.as_str()),
        Some("healthy"),
        "Health endpoint should return {{\"status\": \"healthy\"}}"
    );
}

/// Test that health endpoint returns correct Content-Type header
#[actix_rt::test]
async fn test_health_endpoint_returns_json_content_type() {
    let app = test::init_service(App::new().configure(configure_app)).await;

    let req = test::TestRequest::get().uri("/status").to_request();
    let resp = test::call_service(&app, req).await;

    let content_type = resp
        .headers()
        .get("content-type")
        .expect("Should have Content-Type header")
        .to_str()
        .expect("Content-Type should be valid string");

    assert!(
        content_type.contains("application/json"),
        "Content-Type should be application/json, got: {}",
        content_type
    );
}

/// Test that CORS is enabled and allows requests
#[actix_rt::test]
async fn test_cors_headers_present() {
    // Use create_app() which includes CORS middleware
    let app = test::init_service(create_app()).await;

    // Send OPTIONS preflight request
    let req = test::TestRequest::default()
        .method(actix_web::http::Method::OPTIONS)
        .uri("/status")
        .insert_header(("Origin", "http://example.com"))
        .insert_header(("Access-Control-Request-Method", "GET"))
        .to_request();

    let resp = test::call_service(&app, req).await;

    // CORS should return appropriate headers
    let has_cors = resp.headers().contains_key("access-control-allow-origin")
        || resp.headers().contains_key("access-control-allow-methods");

    assert!(has_cors, "CORS headers should be present in response");
}

/// Test that server config has default port
#[actix_rt::test]
async fn test_server_config_default_port() {
    let config = ServerConfig::default();

    assert_eq!(config.port(), 8080, "Default port should be 8080");
}

/// Test that server config accepts custom port
#[actix_rt::test]
async fn test_server_config_custom_port() {
    let config = ServerConfig::new(3000);

    assert_eq!(config.port(), 3000, "Custom port should be respected");
}

/// Test that server config validates port range
#[actix_rt::test]
async fn test_server_config_port_validation() {
    // Port 0 should be allowed (OS assigns random available port)
    let config = ServerConfig::new(0);
    assert_eq!(config.port(), 0);

    // Standard ports should work
    let config = ServerConfig::new(80);
    assert_eq!(config.port(), 80);

    let config = ServerConfig::new(443);
    assert_eq!(config.port(), 443);

    // High ports should work
    let config = ServerConfig::new(65535);
    assert_eq!(config.port(), 65535);
}

/// Test that server config can be built with builder pattern
#[actix_rt::test]
async fn test_server_config_builder() {
    let config = ServerConfig::builder().port(9000).host("127.0.0.1").build();

    assert_eq!(config.port(), 9000);
    assert_eq!(config.host(), "127.0.0.1");
}

/// Test default host is 0.0.0.0 (all interfaces)
#[actix_rt::test]
async fn test_server_config_default_host() {
    let config = ServerConfig::default();

    assert_eq!(config.host(), "0.0.0.0", "Default host should be 0.0.0.0");
}

/// Test that the health check handler returns proper response
#[actix_rt::test]
async fn test_health_check_handler_directly() {
    let resp = health_check().await;

    // The handler should return an HttpResponse
    assert_eq!(resp.status().as_u16(), 200);
}

/// Test request logging is configured (verifies middleware doesn't break requests)
#[actix_rt::test]
async fn test_request_logging_middleware_configured() {
    let app = test::init_service(App::new().configure(configure_app)).await;

    // Make multiple requests to ensure logging middleware doesn't break anything
    for _ in 0..3 {
        let req = test::TestRequest::get().uri("/status").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }
}

/// Test unknown routes return 404
#[actix_rt::test]
async fn test_unknown_route_returns_404() {
    let app = test::init_service(App::new().configure(configure_app)).await;

    let req = test::TestRequest::get()
        .uri("/nonexistent-route")
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(
        resp.status().as_u16(),
        404,
        "Unknown routes should return 404"
    );
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::time::timeout;

    /// Test that server can be gracefully shut down
    #[actix_rt::test]
    async fn test_graceful_shutdown_signal() {
        use gallifreydb::http::create_server;

        let config = ServerConfig::new(0); // Use port 0 for random available port
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = shutdown_flag.clone();

        // Create server with shutdown handle
        let (server, shutdown_handle) = create_server(config).await.expect("Should create server");

        // Spawn server in background
        let server_handle = tokio::spawn(async move {
            server.await.ok();
            shutdown_flag_clone.store(true, Ordering::SeqCst);
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Trigger graceful shutdown
        shutdown_handle.shutdown().await;

        // Wait for server to shut down with timeout
        let result = timeout(Duration::from_secs(5), server_handle).await;
        assert!(result.is_ok(), "Server should shut down within timeout");

        assert!(
            shutdown_flag.load(Ordering::SeqCst),
            "Shutdown flag should be set after graceful shutdown"
        );
    }
}

// ============================================================================
// AppState Tests (Issue #466)
// ============================================================================

#[cfg(test)]
mod state_tests {
    use gallifreydb::GallifreyDB;
    use gallifreydb::PropertyMapBuilder;
    use gallifreydb::http::AppState;
    use std::sync::Arc;

    /// Test that AppState can be created with an Arc<GallifreyDB>
    #[test]
    fn test_app_state_creation() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state = AppState::new(db.clone());

        // Verify we can access the database through the state
        assert_eq!(state.db().node_count(), 0);
    }

    /// Test that AppState implements Clone for actix-web sharing
    #[test]
    fn test_app_state_is_clone() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state = AppState::new(db.clone());

        // Clone should work
        let state2 = state.clone();

        // Both should reference the same database
        let node_id = state
            .db()
            .create_node("Test", PropertyMapBuilder::new().build())
            .unwrap();
        assert_eq!(state2.db().node_count(), 1);
        // get_node returns Result<Node>, not Option
        let node = state2.db().get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
    }

    /// Test multiple concurrent reads succeed without blocking
    #[actix_rt::test]
    async fn test_app_state_concurrent_reads() {
        let db = Arc::new(GallifreyDB::new().unwrap());

        // Pre-populate with some nodes
        for i in 0..100 {
            db.create_node(
                "Test",
                PropertyMapBuilder::new().insert("id", i as i64).build(),
            )
            .unwrap();
        }

        let state = AppState::new(db);

        // Spawn 10 concurrent read tasks
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let state = state.clone();
                tokio::spawn(async move {
                    for _ in 0..100 {
                        let count = state.db().node_count();
                        assert_eq!(count, 100);
                    }
                })
            })
            .collect();

        // All tasks should complete successfully
        for handle in handles {
            handle.await.expect("Task should not panic");
        }
    }

    /// Test multiple concurrent writes succeed without data races
    #[actix_rt::test]
    async fn test_app_state_concurrent_writes() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state = AppState::new(db);

        // Spawn 10 concurrent write tasks, each creating 10 nodes
        let handles: Vec<_> = (0..10)
            .map(|task_id| {
                let state = state.clone();
                tokio::spawn(async move {
                    for iter in 0..10 {
                        state
                            .db()
                            .create_node(
                                "Test",
                                PropertyMapBuilder::new()
                                    .insert("task", task_id as i64)
                                    .insert("iter", iter as i64)
                                    .build(),
                            )
                            .unwrap();
                    }
                })
            })
            .collect();

        // All tasks should complete successfully
        for handle in handles {
            handle.await.expect("Task should not panic");
        }

        // Verify all 100 nodes were created (10 tasks × 10 nodes each)
        assert_eq!(state.db().node_count(), 100);
    }

    /// Test mixed concurrent reads and writes don't block each other
    #[actix_rt::test]
    async fn test_app_state_mixed_concurrent_operations() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state = AppState::new(db);

        let mut handles = vec![];

        // Spawn 5 writer tasks
        for task_id in 0..5 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                for iter in 0..20 {
                    state
                        .db()
                        .create_node(
                            "Writer",
                            PropertyMapBuilder::new()
                                .insert("task", task_id as i64)
                                .insert("iter", iter as i64)
                                .build(),
                        )
                        .unwrap();
                }
            }));
        }

        // Spawn 5 reader tasks
        for _ in 0..5 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..50 {
                    // Reads should succeed even while writes are happening
                    let _count = state.db().node_count();
                    // Count will vary as writes happen, so we don't assert exact value
                }
            }));
        }

        // All tasks should complete successfully
        for handle in handles {
            handle.await.expect("Task should not panic");
        }

        // Verify all writes succeeded (5 tasks × 20 nodes each)
        assert_eq!(state.db().node_count(), 100);
    }

    /// Test that heavy concurrent load doesn't cause deadlocks
    #[actix_rt::test]
    async fn test_no_deadlock_under_load() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state = AppState::new(db);

        // Spawn many tasks doing mixed operations
        let handles: Vec<_> = (0..20)
            .map(|task_id| {
                let state = state.clone();
                tokio::spawn(async move {
                    for iter in 0..10 {
                        // Mix of operations
                        state
                            .db()
                            .create_node(
                                "Load",
                                PropertyMapBuilder::new()
                                    .insert("task", task_id as i64)
                                    .insert("iter", iter as i64)
                                    .build(),
                            )
                            .unwrap();

                        let _count = state.db().node_count();
                    }
                })
            })
            .collect();

        // Use a timeout to detect deadlocks
        let timeout_result = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
            for handle in handles {
                handle.await.expect("Task should not panic");
            }
        })
        .await;

        assert!(
            timeout_result.is_ok(),
            "Tasks should complete without deadlock"
        );

        // Verify expected number of nodes created (20 tasks × 10 nodes each)
        assert_eq!(state.db().node_count(), 200);
    }

    /// Test that From<Arc<GallifreyDB>> is implemented for convenience
    #[test]
    fn test_app_state_from_arc() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state: AppState = db.into();

        // Should be usable immediately
        assert_eq!(state.db().node_count(), 0);
    }
}
