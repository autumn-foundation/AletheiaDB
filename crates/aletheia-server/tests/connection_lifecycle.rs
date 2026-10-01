//! Acceptance tests for the inbound connection / MCP-session lifecycle budgets
//! (autumn migration §8 / Issue #3561): the app-wide HTTP concurrency limit
//! (AC1), the default-off per-IP `tower-governor` rate-limit layer (AC2), the
//! `DefaultBodyLimit` request-body cap (AC3), the per-query wall-clock-timeout →
//! HTTP status mapping (AC4), and the MCP-over-HTTP session concurrency budget
//! (AC5) — all with **backpressure** (concurrency budgets) or throttling
//! (rate limit), default-off for the concurrency + rate budgets (parity),
//! always-on for the body cap.
//!
//! Each is proven two ways where practical:
//!   * **Seam-level** — the `tower::limit::GlobalConcurrencyLimitLayer` / axum
//!     `DefaultBodyLimit` mounted on a bare `Router`, so the enforcement
//!     semantics (peak-concurrency ceiling, 413) are observed directly and
//!     deterministically.
//!   * **App-level** — the assembled server (`build_server_client*`) wired via
//!     `apply_security`, proving the config actually mounts the layer.
//!
//! AC2 / AC4 wiring landed in PR #3660; the tests here are the regression guard
//! that the layer is actually **mounted** (AC2, driven through the assembled
//! router with a `ConnectInfo` peer so the per-IP key extracts and throttles)
//! and that a per-query timeout maps to the shared #3234 `RESOURCE_EXHAUSTED`
//! `429` envelope (AC4, exercised at the `run_with_timeout` seam where the
//! mapping lives — the synchronous handler bodies cannot be interrupted mid-poll
//! by `tokio::time::timeout`, so a route-level 429 timeout is not reproducible).
//!
//! TDD note: the concurrency enforcement tests are written to be
//! scheduler-robust — a controlled gate holds handlers open so peak concurrency
//! is observed under a real ceiling, never raced against wall-clock timing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aletheia_server::SecurityConfig;
use aletheia_server::security::RateLimitSettings;
use aletheia_server::security::concurrency::{app_concurrency_layer, mcp_session_layer};
use aletheia_server::security::rate_limit::governor_layer;
use aletheia_server::security::resource_limits::run_with_timeout;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
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

// ════════════════════════════════════════════════════════════════════════════
// AC2 — per-IP rate-limit layer (default-off, throttling → 429), MOUNTED
// ════════════════════════════════════════════════════════════════════════════

/// Default parity: a conservative config opts out of rate limiting, so
/// `governor_layer` builds no layer (today's behavior, no `429`s). `Some` with a
/// zero sustained rate is also rejected (an invalid quota → no layer).
#[test]
fn ac2_default_config_yields_no_rate_limit_layer() {
    let cfg = config();
    assert!(cfg.rate_limit.is_none(), "rate limiting is off by default");
    assert!(
        governor_layer(&cfg).is_none(),
        "no rate-limit layer unless the operator opts in"
    );
    // A zero sustained rate is an invalid quota → no layer (never a wedge).
    let zero_rate = cfg
        .clone()
        .with_rate_limit(Some(RateLimitSettings::new(0, 1)));
    assert!(
        governor_layer(&zero_rate).is_none(),
        "a zero rps rate builds no layer"
    );
}

/// Enabled + MOUNTED: an assembled server built with rate limiting on (rps=1,
/// burst=1) actually throttles a rapid burst from one peer IP through the
/// **mounted** router. The first request is admitted; the immediately-following
/// requests exhaust the single-cell bucket and come back `429` with the shared
/// #3234 envelope (`code: RESOURCE_EXHAUSTED`, `retriable: true`).
///
/// Crucially this drives a real request through `apply_security`'s assembled
/// router (via [`into_router`](autumn_web::test::TestClient::into_router)) — it
/// would FAIL if `governor_layer` were constructed but not mounted (every
/// request would pass). A `ConnectInfo` peer is injected so the
/// `PeerIpKeyExtractor` can key the limiter (the in-process test transport sets
/// none otherwise).
#[tokio::test]
async fn ac2_enabled_rate_limit_is_mounted_and_throttles_429() {
    let cfg = SecurityConfig::new(admin_store(), AuthMode::Required)
        .with_rate_limit(Some(RateLimitSettings::new(1, 1)));
    let router =
        aletheia_server::build_server_client_with_config(db_with_node(), cfg).into_router();

    let statuses = burst_statuses(&router, "10.1.2.3:5555", 6).await;
    let throttled = statuses
        .iter()
        .find(|(status, _)| *status == StatusCode::TOO_MANY_REQUESTS)
        .expect("at least one request in the burst is throttled 429 by the mounted layer");

    // The throttle body is the unified #3234 envelope.
    let body: serde_json::Value = serde_json::from_slice(&throttled.1).expect("429 body is JSON");
    assert_eq!(
        body["error"]["code"], "RESOURCE_EXHAUSTED",
        "throttle uses the shared RESOURCE_EXHAUSTED code"
    );
    assert_eq!(
        body["error"]["retriable"], true,
        "rate-limit throttling is transient/retriable"
    );
}

/// Default parity, end-to-end: the SAME rapid burst through an assembled server
/// with rate limiting **off** (the default) never yields a `429` — proving the
/// layer is mounted only when the operator opts in (no throttling by default).
#[tokio::test]
async fn ac2_default_config_never_throttles() {
    let cfg = SecurityConfig::new(admin_store(), AuthMode::Required);
    assert!(cfg.rate_limit.is_none(), "default config is rate-limit-off");
    let router =
        aletheia_server::build_server_client_with_config(db_with_node(), cfg).into_router();

    let statuses = burst_statuses(&router, "10.1.2.3:5555", 6).await;
    assert!(
        statuses
            .iter()
            .all(|(status, _)| *status != StatusCode::TOO_MANY_REQUESTS),
        "no request is throttled when rate limiting is off by default (got {:?})",
        statuses.iter().map(|(s, _)| s.as_u16()).collect::<Vec<_>>()
    );
}

/// Drive `n` sequential `GET /status` requests through `router`, all from the
/// same `peer` IP (so they key the same rate-limit bucket), returning each
/// `(status, body-bytes)`. The rate-limit layer is app-wide and outermost, so it
/// runs before auth — a throttled request is `429` regardless of credentials.
async fn burst_statuses(router: &Router, peer: &str, n: usize) -> Vec<(StatusCode, Vec<u8>)> {
    let addr: SocketAddr = peer.parse().expect("peer socket address parses");
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut req = Request::builder()
            .method("GET")
            .uri("/status")
            .body(Body::empty())
            .expect("request");
        // Set the peer the `PeerIpKeyExtractor` reads (no in-process ConnectInfo
        // otherwise). Same IP each time → same keyed bucket.
        req.extensions_mut().insert(ConnectInfo(addr));
        let resp = router.clone().oneshot(req).await.expect("router responded");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body")
            .to_vec();
        out.push((status, bytes));
    }
    out
}

// ════════════════════════════════════════════════════════════════════════════
// AC4 — per-query wall-clock timeout → HTTP status (429 + #3234 envelope)
// ════════════════════════════════════════════════════════════════════════════

/// A read-path query that overruns its wall-clock deadline resolves to the
/// shared `AletheiaHttpError`, which renders **HTTP 429** with the unified #3234
/// envelope: `code: RESOURCE_EXHAUSTED`, `retriable: true` (a read timeout is
/// transient), and `details.dimension: "wall_clock_timeout"` carrying the
/// effective `limit_ms`.
///
/// This is the seam where every resource-limited read handler's
/// `run_with_timeout(limits.timeout, false, …)` maps a deadline breach to a wire
/// status; asserting it here guards the AC4 contract (429, not 503/408) against
/// regression. The 429 choice follows the established
/// `LimitDimension::WallClockTimeout → TOO_MANY_REQUESTS` mapping.
#[tokio::test]
async fn ac4_read_query_timeout_maps_to_429_envelope() {
    let timeout = Duration::from_millis(10);
    // `is_write = false` (read class): the deadline breach is retriable.
    let err: AletheiaHttpError = run_with_timeout(timeout, false, async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok::<(), AletheiaHttpError>(())
    })
    .await
    .expect_err("the future overruns the 10ms deadline");

    let resp = err.into_response();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a per-query wall-clock timeout maps to HTTP 429"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("429 body is JSON");
    assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(
        body["error"]["retriable"], true,
        "a read-class timeout is transient/retriable"
    );
    assert_eq!(
        body["error"]["details"]["dimension"], "wall_clock_timeout",
        "the timeout dimension is disclosed"
    );
    assert_eq!(
        body["error"]["details"]["limit_ms"], 10,
        "the effective timeout (ms) is echoed"
    );
}

/// A write-path timeout is classified **non-retriable** (a committed write must
/// not be duplicated) — still HTTP 429, but `retriable: false`.
#[tokio::test]
async fn ac4_write_query_timeout_is_non_retriable_429() {
    let err: AletheiaHttpError = run_with_timeout(Duration::from_millis(10), true, async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok::<(), AletheiaHttpError>(())
    })
    .await
    .expect_err("write future overruns the deadline");

    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("body JSON");
    assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(
        body["error"]["retriable"], false,
        "a write-class timeout is not retriable"
    );
}

/// No false positives: the generous default timeout (30s) and the `ZERO`
/// (unlimited) timeout both pass a fast future straight through — a per-query
/// timeout never penalizes a normal, quick request.
#[tokio::test]
async fn ac4_generous_and_zero_timeouts_pass_fast_requests_through() {
    // Config default is 30s (fast requests unaffected by default).
    assert_eq!(
        config().query_timeout,
        Duration::from_millis(30_000),
        "default per-query timeout is 30s"
    );

    let under_generous = run_with_timeout(Duration::from_millis(30_000), false, async { 7u32 })
        .await
        .expect("a fast future completes well within a generous timeout");
    assert_eq!(under_generous, 7);

    // ZERO = unlimited: the future runs inline with no deadline armed.
    let unlimited = run_with_timeout(Duration::ZERO, false, async { 9u32 })
        .await
        .expect("ZERO timeout is unlimited");
    assert_eq!(unlimited, 9);
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
