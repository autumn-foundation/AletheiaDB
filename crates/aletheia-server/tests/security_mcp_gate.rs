//! B4 finalize suite for the custom `/mcp` security gate ([`McpSecurityLayer`]).
//!
//! Covers the coordinator-approved **x-api-key normalization** + its two guards,
//! the **oversize-body → 413** fail-closed path (SHOULD-FIX #2), and the
//! **batch / odd-frame fail-closed** prechecks (#6). Every test is written
//! attack-first: the negative/precedence case precedes the positive.
//!
//! Two harnesses:
//!   * **Layer-level** (`Captor` inner service) — drives [`McpSecurityLayer`]
//!     directly so the *replayed* request headers are observable. This is the
//!     only way to assert the normalization contract ("the replayed handler sees
//!     ONLY the verified credential in `authorization`; the loser/unverified
//!     header does not survive") byte-for-byte.
//!   * **App-level** (`build_server_client`) — drives the assembled server for
//!     the end-to-end 413 / fail-closed / normalized-tools-call behavior.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use aletheia_server::build_server_client;
use aletheia_server::security::mcp_gate::McpSecurityLayer;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use axum::body::Body;
use axum::http::{HeaderMap, Request, Response, StatusCode};
use serde_json::{Value, json};
use tower::{Layer, Service, ServiceExt};

const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnop";

fn keyed_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("reader key");
    store
        .insert_bootstrap_key("metrics", Role::Metrics, METRICS_TOKEN)
        .expect("metrics key");
    store
}

fn db_with_node() -> (Arc<AletheiaDB>, NodeId) {
    let db = Arc::new(AletheiaDB::new().expect("db"));
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("node");
    (db, id)
}

// ── layer-level captor harness ────────────────────────────────────────────────

/// A minimal inner `tower::Service` that records the headers of the request the
/// gate forwards to it (the "replayed" request) and how many times it is called.
#[derive(Clone)]
struct Captor {
    seen: Arc<Mutex<Option<HeaderMap>>>,
    hits: Arc<AtomicUsize>,
}

impl Captor {
    fn new() -> Self {
        Self {
            seen: Arc::new(Mutex::new(None)),
            hits: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
    fn seen(&self) -> Option<HeaderMap> {
        self.seen.lock().expect("lock").clone()
    }
}

impl Service<Request<Body>> for Captor {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        self.hits.fetch_add(1, Ordering::SeqCst);
        *self.seen.lock().expect("lock") = Some(req.headers().clone());
        Box::pin(async { Ok(Response::new(Body::from("inner-ok"))) })
    }
}

/// Drive one request through `McpSecurityLayer` wrapping a `Captor`. Returns the
/// gate's response status plus the captor (to inspect what was replayed).
async fn drive(store: Arc<AuthStore>, req: Request<Body>) -> (StatusCode, Captor) {
    let captor = Captor::new();
    let svc = McpSecurityLayer::new(store, AuthMode::Required).layer(captor.clone());
    let resp = svc.oneshot(req).await.expect("gate responds");
    (resp.status(), captor)
}

fn tools_list_body() -> Body {
    Body::from(serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).unwrap())
}

// ════════════════════════════════════════════════════════════════════════════
// Guard 1 — dual-header precedence (legacy: Authorization/Bearer-first, NO
// fallthrough to x-api-key when Authorization is present) + loser-not-survive.
// ════════════════════════════════════════════════════════════════════════════

/// Attack (a): a **valid x-api-key** presented alongside a **garbage
/// Authorization** header. Legacy precedence: a present Authorization header is
/// authoritative and, being non-bearer, collapses the whole extraction to
/// `None` — the x-api-key is NEVER consulted. So the request is rejected 401 and
/// the inner (replay) service is never reached.
#[tokio::test]
async fn guard1_garbage_authorization_defeats_valid_x_api_key_401() {
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("authorization", "NotBearer somejunk")
        .header("x-api-key", READER_TOKEN)
        .body(tools_list_body())
        .unwrap();

    let (status, captor) = drive(keyed_store(), req).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "present-but-non-bearer Authorization wins and fails; x-api-key not consulted"
    );
    assert_eq!(
        captor.hits(),
        0,
        "rejected before the replay: inner not called"
    );
}

/// Attack (b): a **valid Bearer** alongside a **garbage x-api-key**. Bearer wins;
/// the garbage x-api-key is ignored AND must not survive into the replay. The
/// replayed request carries ONLY `authorization: Bearer <verified>`.
#[tokio::test]
async fn guard1_valid_bearer_wins_and_strips_garbage_x_api_key() {
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("authorization", format!("Bearer {READER_TOKEN}"))
        .header("x-api-key", "garbage-loser-value")
        .body(tools_list_body())
        .unwrap();

    let (status, captor) = drive(keyed_store(), req).await;
    assert_eq!(status, StatusCode::OK, "valid bearer authenticates");
    assert_eq!(captor.hits(), 1, "passed through to the replay");
    let seen = captor.seen().expect("headers captured");
    assert_eq!(
        seen.get("authorization").and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {READER_TOKEN}").as_str()),
        "replay carries the verified bearer credential"
    );
    assert!(
        seen.get("x-api-key").is_none(),
        "the losing/unverified x-api-key must NOT survive into the replay"
    );
}

/// Normalization core: an **x-api-key-only** request (no Authorization) is
/// verified, then REWRITTEN to `authorization: Bearer <verified>` before the
/// replay — giving x-api-key full parity on `tools/call` (only `authorization`
/// is in autumn's FORWARDED_HEADERS). The x-api-key header does not survive.
#[tokio::test]
async fn guard1_x_api_key_normalized_onto_authorization() {
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("x-api-key", READER_TOKEN)
        .body(tools_list_body())
        .unwrap();

    let (status, captor) = drive(keyed_store(), req).await;
    assert_eq!(status, StatusCode::OK);
    let seen = captor.seen().expect("headers captured");
    assert_eq!(
        seen.get("authorization").and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {READER_TOKEN}").as_str()),
        "x-api-key credential is normalized onto authorization for the replay"
    );
    assert!(
        seen.get("x-api-key").is_none(),
        "x-api-key is consumed by normalization, not forwarded verbatim"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Guard 2 — the authorization header / token is never logged.
// ════════════════════════════════════════════════════════════════════════════

/// Source guard: the gate must not contain any logging macro that could emit the
/// credential. Pairs with AC(d)'s response-body scrub (the token never appears in
/// any response body either).
#[test]
fn guard2_mcp_gate_never_logs_the_credential() {
    const SRC: &str = include_str!("../src/security/mcp_gate.rs");
    for pat in [
        "println!",
        "eprintln!",
        "dbg!",
        "tracing::",
        "log::",
        "print!",
    ] {
        assert!(
            !SRC.contains(pat),
            "mcp_gate.rs must not log ({pat}) — it handles the raw credential/authorization header"
        );
    }
}

/// Guard 2 (behavioral): a normalized, verified request's token never appears in
/// the gate's forwarded/response surface — asserted here against the replay
/// response (the inner service returns a fixed body; the token must not leak into
/// it, and the gate adds no echoing).
#[tokio::test]
async fn guard2_token_never_echoed_in_gate_output() {
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("x-api-key", READER_TOKEN)
        .body(tools_list_body())
        .unwrap();
    let captor = Captor::new();
    let svc = McpSecurityLayer::new(keyed_store(), AuthMode::Required).layer(captor.clone());
    let resp = svc.oneshot(req).await.expect("gate responds");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let raw = String::from_utf8_lossy(&bytes);
    assert!(
        !raw.contains(READER_TOKEN),
        "the verified token must never be echoed in the gate's response body"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// SHOULD-FIX #2 — oversize buffered body → 413 RESOURCE_EXHAUSTED envelope
// (not a swallowed error forwarding an EMPTY body).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn oversize_mcp_body_is_413_envelope() {
    let (db, node) = db_with_node();
    let client = build_server_client(db, keyed_store(), AuthMode::Required);

    // A >2 MiB body: authentication (header-only) succeeds, then the body buffer
    // exceeds MAX_MCP_REQUEST_BYTES and must fail-closed with 413.
    let pad = "x".repeat(3 * 1024 * 1024);
    let rpc = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "get_node", "arguments": { "id": node.as_u64(), "pad": pad } }
    });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).unwrap_or(Value::Null);
    assert_eq!(status, 413, "oversize body → 413: {body}");
    assert_eq!(body["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(body["success"], json!(false));
}

// ════════════════════════════════════════════════════════════════════════════
// #6 — fail-closed on unrecognized tool-executing frames (batch + odd frame).
// ════════════════════════════════════════════════════════════════════════════

/// A JSON-RPC **batch array** containing a `tools/call` cannot be per-tool
/// RBAC-prechecked at the envelope, so the gate refuses it fail-closed rather
/// than under-gating it. The tool never executes.
#[tokio::test]
async fn batch_with_tools_call_is_failclosed() {
    let (db, node) = db_with_node();
    let client = build_server_client(db, keyed_store(), AuthMode::Required);

    let batch = json!([
        { "jsonrpc": "2.0", "id": 1, "method": "tools/call",
          "params": { "name": "get_node", "arguments": { "id": node.as_u64() } } }
    ]);
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&batch)
        .send()
        .await;
    let status = resp.status.as_u16();
    let text = resp.text();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(
        status, 400,
        "batched tools/call is refused fail-closed: {text}"
    );
    assert_eq!(body["code"], "INVALID_ARGUMENT");
    assert!(
        !text.contains("\"name\":\"Alice\"") && !text.contains("Alice"),
        "the tool must NOT have executed (no node data leaked): {text}"
    );
}

/// An odd frame whose `method` is `tools/call` but whose `params.name` is
/// missing/non-string is a tool-executing frame the gate cannot identify, so it
/// is refused fail-closed (never passed through un-RBAC-checked).
#[tokio::test]
async fn tools_call_missing_name_is_failclosed() {
    let (db, _node) = db_with_node();
    let client = build_server_client(db, keyed_store(), AuthMode::Required);

    let rpc = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "arguments": {} }
    });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).unwrap_or(Value::Null);
    assert_eq!(
        status, 400,
        "malformed tools/call → fail-closed 400: {body}"
    );
    assert_eq!(body["code"], "INVALID_ARGUMENT");
}

// ════════════════════════════════════════════════════════════════════════════
// Normalization end-to-end — x-api-key now works through the tools/call replay.
// ════════════════════════════════════════════════════════════════════════════

/// With normalization landed, an **x-api-key-only** `tools/call` authenticates
/// AND dispatches (the replayed handler re-verifies the normalized
/// `authorization: Bearer`), closing the documented v1 boundary.
#[tokio::test]
async fn x_api_key_tools_call_authenticates_via_normalization() {
    let (db, node) = db_with_node();
    let client = build_server_client(db, keyed_store(), AuthMode::Required);

    let rpc = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "get_node", "arguments": { "id": node.as_u64() } }
    });
    let resp = client
        .post("/mcp")
        .header("x-api-key", READER_TOKEN)
        .json(&rpc)
        .send()
        .await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).unwrap_or(Value::Null);
    assert_eq!(status, 200, "x-api-key tools/call now dispatches: {body}");
    assert_eq!(body["result"]["isError"], json!(false), "tool ran: {body}");
}

/// Envelope-refused metrics→read over x-api-key still 403s at the gate (RBAC
/// precheck runs on the normalized credential's principal, unchanged).
#[tokio::test]
async fn x_api_key_metrics_read_tool_denied_403_with_details() {
    let (db, node) = db_with_node();
    let client = build_server_client(db, keyed_store(), AuthMode::Required);

    let rpc = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "get_node", "arguments": { "id": node.as_u64() } }
    });
    let resp = client
        .post("/mcp")
        .header("x-api-key", METRICS_TOKEN)
        .json(&rpc)
        .send()
        .await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).unwrap_or(Value::Null);
    assert_eq!(status, 403, "metrics denied read over x-api-key: {body}");
    assert_eq!(body["code"], "PERMISSION_DENIED");
    assert_eq!(body["details"]["required_class"], "read");
    assert_eq!(body["details"]["principal_role"], "metrics");
}
