//! Seam-independent acceptance tests for the Lane B3 rate-limit core (autumn
//! migration §8 / Issue #3561 AC: default-off per-IP `tower-governor` layer,
//! 429 on over-limit).
//!
//! Proves two things the proving ground exists to prove:
//!   1. **Default-off parity** — a conservative `SecurityConfig` yields no
//!      layer (`governor_layer(..) == None`), so a router built without it
//!      never 429s (today's behavior, unchanged).
//!   2. **The real `GovernorLayer` mounts and runs** on an axum 0.8 / tower 0.5
//!      `Router` via `.layer()` and enforces the configured rate — superseding
//!      ADR-0055's "custom middleware impractical under autumn 0.4" finding.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use aletheia_server::security::rate_limit::governor_layer;
use aletheia_server::security::{RateLimitSettings, SecurityConfig};
use aletheiadb::auth::{AuthMode, AuthStore};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use tower::ServiceExt; // oneshot

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A conservative-default `SecurityConfig` (store-first `new` in the re-homed
/// crate).
fn config() -> SecurityConfig {
    SecurityConfig::new(Arc::new(AuthStore::new()), AuthMode::Required)
}

/// A `GET /` request carrying a `ConnectInfo` peer IP (what
/// `PeerIpKeyExtractor` keys on). Driving via `oneshot` bypasses the server's
/// `into_make_service_with_connect_info`, so the extension is inserted by hand.
fn req_from(ip: [u8; 4]) -> Request<Body> {
    let mut r = Request::builder()
        .uri("/")
        .body(Body::empty())
        .expect("valid request");
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 9999);
    r.extensions_mut().insert(ConnectInfo(addr));
    r
}

fn router_with_layer(cfg: &SecurityConfig) -> Router {
    let rl = governor_layer(cfg).expect("rate limiting is enabled for this config");
    Router::new()
        .route("/", get(|| async { "ok" }))
        .layer(rl.layer)
}

// ---------------------------------------------------------------------------
// (1) Default-OFF parity — always-on, never flaky
// ---------------------------------------------------------------------------

#[test]
fn default_config_yields_no_layer() {
    let cfg = config();
    assert!(cfg.rate_limit.is_none(), "rate limiting is off by default");
    assert!(
        governor_layer(&cfg).is_none(),
        "no layer is built unless the operator opts in"
    );
}

#[tokio::test]
async fn router_without_layer_never_rate_limits() {
    // A router built the default way (no governor layer) serves every request
    // 200, no matter how many arrive back-to-back from one IP.
    let app = Router::new().route("/", get(|| async { "ok" }));
    for _ in 0..25 {
        let resp = app
            .clone()
            .oneshot(req_from([127, 0, 0, 1]))
            .await
            .expect("router responds");
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

// ---------------------------------------------------------------------------
// (2) Enabled: the real GovernorLayer mounts and enforces the rate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enabled_under_limit_request_succeeds() {
    let mut cfg = config();
    cfg.rate_limit = Some(RateLimitSettings::new(1, 1));
    let app = router_with_layer(&cfg);

    // The first (only) request from an IP is within the burst budget → 200.
    let resp = app
        .oneshot(req_from([10, 0, 0, 1]))
        .await
        .expect("layer passes the request through");
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Over-limit → 429. Uses burst=1 + an immediate second request from the same
/// IP. If the quanta clock proves flaky under the sandbox scheduler, `#[ignore]`
/// this one — the default-off test above remains the always-on guarantee.
#[tokio::test]
async fn enabled_over_limit_request_is_rejected_429() {
    let mut cfg = config();
    cfg.rate_limit = Some(RateLimitSettings::new(1, 1));
    let app = router_with_layer(&cfg);

    // Burst budget of 1: the first request consumes the single cell.
    let first = app
        .clone()
        .oneshot(req_from([10, 0, 0, 2]))
        .await
        .expect("first request served");
    assert_eq!(first.status(), StatusCode::OK);

    // The immediate second request from the same IP (no time for the 1/s
    // replenishment) is throttled.
    let second = app
        .clone()
        .oneshot(req_from([10, 0, 0, 2]))
        .await
        .expect("second request served");
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "over-limit request must be 429"
    );
}

/// F1 regression (rate-mapping inversion): with `rps=5, burst=5` the layer must
/// admit a burst of 5, reject the 6th immediate request (429), and — crucially —
/// **replenish** a cell after roughly one period (1/rps = 200ms) so a later
/// request passes again. The historical bug mapped `rps` onto the replenishment
/// *period in seconds* (`per_second(rps)`), so `rps=5` meant "one cell every 5s":
/// the burst/429 half coincidentally held, but no cell replenished within 220ms,
/// so the post-sleep request stayed 429. This test pins the replenishment half
/// the inversion breaks.
#[tokio::test]
async fn rps_5_allows_burst_then_429s() {
    let mut cfg = config();
    cfg.rate_limit = Some(RateLimitSettings::new(5, 5));
    let app = router_with_layer(&cfg);
    let ip = [10, 0, 0, 5];

    // Burst budget of 5: the first five immediate requests all pass.
    for i in 0..5 {
        let resp = app
            .clone()
            .oneshot(req_from(ip))
            .await
            .expect("burst request served");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "request {i} within the burst of 5 must pass"
        );
    }

    // The 6th immediate request (no time to replenish) is throttled.
    let sixth = app
        .clone()
        .oneshot(req_from(ip))
        .await
        .expect("sixth request served");
    assert_eq!(
        sixth.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "6th immediate request exhausts the burst → 429"
    );

    // After ~1.1 periods (1/rps = 200ms) exactly one cell has replenished, so
    // one more request passes. Under the inverted mapping (period = 5s) nothing
    // replenishes in this window and this stays 429 — the RED assertion.
    tokio::time::sleep(Duration::from_millis(220)).await;
    let after_replenish = app
        .clone()
        .oneshot(req_from(ip))
        .await
        .expect("post-replenish request served");
    assert_eq!(
        after_replenish.status(),
        StatusCode::OK,
        "a cell replenishes within ~1 period (1/rps), so this must pass"
    );
}

/// F2 regression (unbounded per-IP keyed state): the limiter accrues one keyed
/// entry per distinct source IP and — without a GC handle — never reclaims them,
/// leaking memory under attacker-controlled source IPs. `RateLimit::gc()` (the
/// exposed `retain_recent()` handle) must evict keys once they are idle past the
/// retention window. Drives several distinct IPs through the SHARED layer,
/// asserts the keyed state grows, then asserts `gc()` after the window shrinks
/// it back to empty (and is idempotent). A period of 20ms (`rps=50`) keeps the
/// retention window tiny so the test is fast and non-flaky.
#[tokio::test]
async fn retain_recent_evicts_stale_keys() {
    let mut cfg = config();
    // rps=50 → 20ms replenishment period → ~tens-of-ms retention window.
    cfg.rate_limit = Some(RateLimitSettings::new(50, 1));
    let rl = governor_layer(&cfg).expect("rate limiting enabled");

    // Mount a CLONE of the layer so `rl` stays owned for gc()/live_keys(); the
    // clone shares the same keyed limiter Arc, so GC on `rl` reaches this layer.
    let app = Router::new()
        .route("/", get(|| async { "ok" }))
        .layer(rl.layer.clone());

    // Empty before any traffic.
    assert_eq!(rl.live_keys(), 0, "no keys before traffic");

    // Register five distinct source IPs (one request each, all within burst=1).
    let ips = [
        [10, 2, 0, 1],
        [10, 2, 0, 2],
        [10, 2, 0, 3],
        [10, 2, 0, 4],
        [10, 2, 0, 5],
    ];
    for ip in ips {
        let resp = app.clone().oneshot(req_from(ip)).await.expect("served");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // Keyed state grew: one live key per distinct IP.
    assert_eq!(rl.live_keys(), 5, "one keyed entry accrues per distinct IP");

    // GC before the retention window is a safe no-op (keys still fresh).
    rl.gc();
    assert_eq!(rl.live_keys(), 5, "fresh keys survive an early gc()");

    // After the retention window the keys are stale; gc() reclaims them.
    tokio::time::sleep(Duration::from_millis(300)).await;
    rl.gc();
    assert_eq!(rl.live_keys(), 0, "gc() evicts stale per-IP keys");

    // Idempotent: a second gc() with no intervening traffic stays at zero.
    rl.gc();
    assert_eq!(rl.live_keys(), 0, "gc() is idempotent");
}

// ---------------------------------------------------------------------------
// (3) Document the raw 429 shape governor emits (wrapped into the retriable
// envelope at B4).
// ---------------------------------------------------------------------------

/// The raw `GovernorLayer` 429 carries a `retry-after` header (seconds until a
/// cell frees) and a plain-text body. B4 will wrap this into the #3234
/// `{code: "UNAVAILABLE", retriable: true, ...}` envelope; this test pins the
/// raw shape we are wrapping.
#[tokio::test]
async fn documents_raw_429_headers_and_body() {
    let mut cfg = config();
    cfg.rate_limit = Some(RateLimitSettings::new(1, 1));
    let app = router_with_layer(&cfg);

    let _first = app.clone().oneshot(req_from([10, 0, 0, 3])).await.unwrap();
    let throttled = app
        .clone()
        .oneshot(req_from([10, 0, 0, 3]))
        .await
        .expect("throttled response");
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

    // Raw governor 429 advertises a `retry-after` header.
    assert!(
        throttled.headers().contains_key("retry-after"),
        "governor 429 carries retry-after; headers = {:?}",
        throttled.headers()
    );

    let body = axum::body::to_bytes(throttled.into_body(), usize::MAX)
        .await
        .expect("read body");
    // A non-empty human-readable body (the text we replace with the envelope).
    assert!(!body.is_empty(), "governor 429 has an explanatory body");
}
