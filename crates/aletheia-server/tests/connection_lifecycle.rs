//! Acceptance tests for the inbound connection / MCP-session lifecycle budgets
//! (autumn migration §8 / Issue #3561): the app-wide HTTP concurrency limit
//! (AC1), the `DefaultBodyLimit` request-body cap (AC3), and the MCP-over-HTTP
//! session concurrency budget (AC5) — all with **backpressure**, default-off
//! for the concurrency budgets (parity), always-on for the body cap.
//!
//! Each is proven two ways where practical:
//!   * **Seam-level** — the `tower::limit::GlobalConcurrencyLimitLayer` / axum
//!     `DefaultBodyLimit` mounted on a bare `Router`, so the enforcement
//!     semantics (peak-concurrency ceiling, 413) are observed directly and
//!     deterministically.
//!   * **App-level** — the assembled server (`build_server_client*`) wired via
//!     `apply_security`, proving the config actually mounts the layer.
//!
//! TDD note: the concurrency enforcement tests are written to be
//! scheduler-robust — a controlled gate holds handlers open so peak concurrency
//! is observed under a real ceiling, never raced against wall-clock timing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aletheia_server::SecurityConfig;
use aletheia_server::security::concurrency::{app_concurrency_layer, mcp_session_layer};
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower::{Layer, Service, ServiceExt};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";

// ── fixtures ────────────────────────────────────────────────────────────────

fn config() -> SecurityConfig {
    SecurityConfig::new(Arc::new(AuthStore::new()), AuthMode::Anonymous)
}

fn admin_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("admin key");
    store
}

fn db_with_node() -> Arc<AletheiaDB> {
    let db = Arc::new(AletheiaDB::new().expect("db"));
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .expect("node");
    db
}

// ════════════════════════════════════════════════════════════════════════════
// AC1 — app-wide HTTP concurrency budget (default-off, backpressure)
// ════════════════════════════════════════════════════════════════════════════

/// Default parity: a conservative config builds no layer, so no request is ever
/// back-pressured (today's behavior, unchanged).
#[test]
fn ac1_default_config_yields_no_app_concurrency_layer() {
    let cfg = config();
    assert!(
        cfg.max_concurrent_requests.is_none(),
        "app concurrency budget is off by default"
    );
    assert!(
        app_concurrency_layer(&cfg).is_none(),
        "no layer unless the operator opts in"
    );
    // Some(0) is treated as off — a zero-permit budget would wedge everything.
    let zero = cfg.clone().with_max_concurrent_requests(Some(0));
    assert!(
        app_concurrency_layer(&zero).is_none(),
        "Some(0) is treated as no budget"
    );
}

/// Enabled: the global limit admits at most `cap` requests concurrently and
/// back-pressures the rest (parks them in `poll_ready`) until a permit frees —
/// then every request eventually completes (backpressure, NOT rejection).
#[tokio::test]
async fn ac1_enabled_caps_concurrency_and_backpressures() {
    let cfg = config().with_max_concurrent_requests(Some(2));
    let layer = app_concurrency_layer(&cfg).expect("layer built when enabled");
    assert_peak_concurrency_capped(layer, 2).await;
}

// ════════════════════════════════════════════════════════════════════════════
// AC5 — MCP-over-HTTP session budget (default-off, backpressure)
// ════════════════════════════════════════════════════════════════════════════

/// Default parity: no `/mcp` session budget layer unless opted in.
#[test]
fn ac5_default_config_yields_no_mcp_session_layer() {
    let cfg = config();
    assert!(
        cfg.max_mcp_sessions.is_none(),
        "mcp session budget off by default"
    );
    assert!(
        mcp_session_layer(&cfg).is_none(),
        "no layer unless opted in"
    );
    let zero = cfg.clone().with_max_mcp_sessions(Some(0));
    assert!(
        mcp_session_layer(&zero).is_none(),
        "Some(0) is treated as no budget"
    );
}

/// Enabled: the MCP session budget uses the same global-concurrency primitive,
/// so it caps concurrent MCP requests and back-pressures the overflow.
#[tokio::test]
async fn ac5_enabled_caps_mcp_sessions_and_backpressures() {
    let cfg = config().with_max_mcp_sessions(Some(2));
    let layer = mcp_session_layer(&cfg).expect("layer built when enabled");
    assert_peak_concurrency_capped(layer, 2).await;
}

/// The MCP session budget is enabled end-to-end without breaking dispatch: an
/// assembled server with `max_mcp_sessions = Some(1)` still serves a `/mcp`
/// `tools/list` `200` (the budget bounds concurrency, it does not reject).
#[tokio::test]
async fn ac5_app_with_mcp_budget_still_serves_mcp() {
    let cfg = SecurityConfig::new(admin_store(), AuthMode::Required).with_max_mcp_sessions(Some(1));
    let client = aletheia_server::build_server_client_with_config(db_with_node(), cfg);
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        200,
        "budget-enabled /mcp still dispatches"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// AC3 — DefaultBodyLimit request-body cap (always-on, 413)
// ════════════════════════════════════════════════════════════════════════════

/// The config default matches the legacy HTTP surface (2 MiB, #3108).
#[test]
fn ac3_default_body_cap_is_2_mib() {
    assert_eq!(
        config().max_request_body_bytes,
        2 * 1024 * 1024,
        "default body cap matches legacy DEFAULT_MAX_REQUEST_BODY_BYTES"
    );
}

/// Seam-level: a `DefaultBodyLimit::max(N)` layer rejects an `N+1`-byte body with
/// `413 Payload Too Large` and passes an `N`-byte body (inclusive boundary).
#[tokio::test]
async fn ac3_body_over_limit_is_413_under_is_ok() {
    const LIMIT: usize = 64;
    let app = Router::new()
        .route(
            "/echo",
            post(|body: axum::body::Bytes| async move { format!("got {}", body.len()) }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(LIMIT));

    // Exactly LIMIT bytes: within budget → 200.
    let ok = app
        .clone()
        .oneshot(post_bytes("/echo", vec![b'x'; LIMIT]))
        .await
        .expect("served");
    assert_eq!(ok.status(), StatusCode::OK, "N-byte body is within budget");

    // LIMIT + 1 bytes: over budget → 413.
    let too_big = app
        .clone()
        .oneshot(post_bytes("/echo", vec![b'x'; LIMIT + 1]))
        .await
        .expect("served");
    assert_eq!(
        too_big.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "over-limit body is rejected 413"
    );
}

/// App-level: an assembled server built with a tiny `max_request_body_bytes`
/// rejects an over-limit POST body with `413`, proving `apply_security` wires
/// `DefaultBodyLimit` onto the real surface.
#[tokio::test]
async fn ac3_app_rejects_oversize_body_413() {
    let cfg =
        SecurityConfig::new(admin_store(), AuthMode::Required).with_max_request_body_bytes(32);
    let client = aletheia_server::build_server_client_with_config(db_with_node(), cfg);

    // A create_node body well over 32 bytes.
    let big = serde_json::json!({
        "label": "Person",
        "properties": { "name": "a-name-that-makes-this-body-exceed-thirty-two-bytes" }
    });
    let resp = client
        .post("/nodes")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .json(&big)
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        413,
        "oversize body rejected before handler logic"
    );
}

// ── shared concurrency-enforcement harness ───────────────────────────────────

/// Drive `cap + 1` concurrent requests through a `GlobalConcurrencyLimitLayer`
/// wrapping a **gated** handler and assert the observed peak concurrency never
/// exceeds `cap` (the overflow request is back-pressured), then release the gate
/// and assert every request completes `200`.
///
/// Robust-by-construction (not timing-raced): each admitted handler increments a
/// live counter, records the running peak, then blocks on a controlled
/// semaphore. Only after the live count settles at `cap` (the overflow parked in
/// the layer's `poll_ready`) is the gate opened.
async fn assert_peak_concurrency_capped(layer: GlobalConcurrencyLimitLayer, cap: usize) {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let handler = {
        let gate = Arc::clone(&gate);
        let live = Arc::clone(&live);
        let peak = Arc::clone(&peak);
        tower::service_fn(move |_req: Request<Body>| {
            let gate = Arc::clone(&gate);
            let live = Arc::clone(&live);
            let peak = Arc::clone(&peak);
            async move {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Hold the handler (and thus the concurrency permit) open until
                // the gate is released.
                let _permit = gate.acquire().await.expect("gate open");
                live.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(axum::response::Response::new(Body::from("ok")))
            }
        })
    };

    // One shared service instance → clones share the global permit semaphore.
    let svc = layer.layer(handler);

    let total = cap + 1;
    let mut tasks: Vec<tokio::task::JoinHandle<axum::response::Response<Body>>> = Vec::new();
    for _ in 0..total {
        let mut svc = svc.clone();
        tasks.push(tokio::spawn(async move {
            svc.ready()
                .await
                .expect("ready")
                .call(empty_post())
                .await
                .expect("service responded")
        }));
    }

    // Wait until exactly `cap` handlers are admitted and live (the overflow is
    // parked in poll_ready). Bounded spin so a regression that admits > cap
    // fails fast rather than hanging.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if live.load(Ordering::SeqCst) == cap {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "handlers never settled at the cap (live = {})",
            live.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Give any (erroneously admitted) overflow a chance to slip through, then
    // assert the ceiling held.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        live.load(Ordering::SeqCst),
        cap,
        "no more than `cap` handlers run concurrently (overflow is back-pressured)"
    );
    assert_eq!(
        peak.load(Ordering::SeqCst),
        cap,
        "peak concurrency equals the cap, never exceeds it"
    );

    // Release the gate: all handlers (including the back-pressured overflow)
    // proceed and complete — backpressure delayed, never rejected.
    gate.add_permits(total);
    for t in tasks {
        let resp = t.await.expect("task");
        assert_eq!(resp.status(), StatusCode::OK, "every request completes 200");
    }
}

fn empty_post() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/")
        .body(Body::empty())
        .expect("request")
}

fn post_bytes(uri: &str, bytes: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::from(bytes))
        .expect("request")
}
