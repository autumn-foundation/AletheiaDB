//! Cross-surface enforcement + admin-gating of the per-principal changefeed
//! quota (Issue #3678, review round 1), driven against the LIVE assembled
//! autumn-web server via the in-process `TestClient`.
//!
//! These close the AC (a) "consistently across every surface" gaps the round-1
//! review flagged as wired-but-untested:
//!
//! - **B1 (security)**: the `database_stats` per-principal identity roster is
//!   ADMIN-gated — a `metrics`/`reader` credential sees the scalar aggregate but
//!   NOT other principals' ids/counts, admin sees the roster.
//! - **T1**: the SSE `GET /changes/stream` route enforces the per-principal
//!   quota (→ 429 `RESOURCE_EXHAUSTED`) — RED if reverted to the non-principal
//!   `subscribe_changes` (a 200 stream would open and hang instead of 429ing).
//! - **T2**: an SSE client disconnect RELEASES the principal slot (the
//!   SSE-worker-drop path, distinct from a plain `drop(sub)`).
//! - **T3**: the HTTP `POST /changes/await` route buckets by the caller's REAL
//!   authenticated principal, not the shared "anonymous" fallback.

use std::sync::Arc;
use std::time::Duration;

use aletheia_server::build_server_client;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::changefeed_subscription::ChangefeedConfig;
use aletheiadb::{AletheiaDB, ChangeFilter, PropertyMapBuilder};
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnop";
const WRITER_TOKEN: &str = "writer-token-abcdefghijklmnop";

// ── fixtures / helpers ───────────────────────────────────────────────────────

fn new_db() -> Arc<AletheiaDB> {
    Arc::new(AletheiaDB::new().expect("create db"))
}

fn keyed_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("admin key");
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("reader key");
    store
        .insert_bootstrap_key("metrics", Role::Metrics, METRICS_TOKEN)
        .expect("metrics key");
    store
        .insert_bootstrap_key("writer", Role::Writer, WRITER_TOKEN)
        .expect("writer key");
    store
}

/// The stable principal id a token resolves to (the changefeed quota bucket key).
fn principal_id(store: &AuthStore, token: &str) -> String {
    store.verify(token).expect("verify token").id
}

/// The live per-principal subscription count for `id` (0 if absent).
fn principal_count(db: &AletheiaDB, id: &str) -> usize {
    db.changefeed_principal_counts()
        .into_iter()
        .find(|(k, _)| k == id)
        .map(|(_, c)| c)
        .unwrap_or(0)
}

/// Locate the `changefeed` stats block regardless of whether the response is the
/// raw stats object or a `{success, data}` envelope.
fn changefeed_block(body: &Value) -> &Value {
    body.get("changefeed")
        .or_else(|| body.get("data").and_then(|d| d.get("changefeed")))
        .unwrap_or_else(|| panic!("no changefeed block in response: {body}"))
}

// ── B1: per-principal breakdown is admin-gated on `database_stats` ────────────

/// The `database_stats` changefeed per-principal identity roster is ADMIN-gated
/// (Issue #3678 B1). `database_stats` is `metrics`-class, so a metrics/reader
/// credential reaches the route and sees the SCALAR aggregate — but only an
/// admin sees `changefeed.per_principal` (other principals' ids + live counts).
/// Goes RED if the gate is removed (roster leaks to metrics/reader).
#[tokio::test]
async fn database_stats_per_principal_breakdown_is_admin_gated() {
    let db = new_db();
    let store = keyed_store();
    // Two live principal-scoped subscriptions so the roster is non-empty.
    let _alice = db
        .subscribe_changes_for_principal(Some("alice"), ChangeFilter::all())
        .expect("alice subscribes");
    let _bob = db
        .subscribe_changes_for_principal(Some("bob"), ChangeFilter::all())
        .expect("bob subscribes");

    let client = build_server_client(Arc::clone(&db), Arc::clone(&store), AuthMode::Required);

    // Admin: the per-principal roster IS present, with both principals.
    let resp = client
        .get("/database_stats")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200, "admin reads database_stats");
    let body: Value = resp.json();
    let cf = changefeed_block(&body);
    let names: Vec<&str> = cf["per_principal"]
        .as_array()
        .expect("admin sees the per_principal roster")
        .iter()
        .filter_map(|e| e["principal"].as_str())
        .collect();
    assert!(
        names.contains(&"alice") && names.contains(&"bob"),
        "admin sees both principals: {names:?}"
    );
    assert!(
        cf.get("active_subscriptions").is_some(),
        "admin sees the scalar aggregate too"
    );

    // Non-admin (metrics + reader): the scalar aggregate stays, the identity
    // roster is OMITTED entirely.
    for (label, token) in [("metrics", METRICS_TOKEN), ("reader", READER_TOKEN)] {
        let resp = client
            .get("/database_stats")
            .header("authorization", &format!("Bearer {token}"))
            .send()
            .await;
        assert_eq!(
            resp.status.as_u16(),
            200,
            "{label} may read metrics-tier database_stats"
        );
        let body: Value = resp.json();
        let cf = changefeed_block(&body);
        assert!(
            cf.get("per_principal").is_none(),
            "{label} must NOT see the per-principal identity roster: {cf}"
        );
        assert!(
            cf.get("active_subscriptions").is_some(),
            "{label} still sees the scalar aggregate"
        );
    }
}

// ── T1: SSE stream enforces the per-principal quota ──────────────────────────

/// The SSE `GET /changes/stream` route enforces the per-principal quota: a
/// principal already at its cap gets a prompt 429 `RESOURCE_EXHAUSTED` (the
/// subscribe is rejected before any stream opens). RED if the route is reverted
/// to the non-principal `subscribe_changes` — the bare path bypasses the quota,
/// so a 200 SSE stream would open and the buffering `send()` would hang until
/// the timeout fires (→ the `.expect` panics).
#[tokio::test]
async fn sse_stream_enforces_per_principal_quota_429() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    // Global cap high so ONLY the per-principal cap can bind; per-principal 1.
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(1),
    );
    // Fill the reader's bucket to its cap (held for the whole test).
    let _held = db
        .subscribe_changes_for_principal(Some(&reader_id), ChangeFilter::all())
        .expect("prefill reader bucket");

    let client = build_server_client(Arc::clone(&db), store, AuthMode::Required);

    let fut = client
        .get("/changes/stream")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send();
    let resp = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .expect("changes/stream at quota must respond promptly with 429, not open an SSE stream");
    assert_eq!(resp.status.as_u16(), 429, "per-principal quota → 429");
    let body: Value = resp.json();
    assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(body["error"]["retriable"], true);
    assert_eq!(
        body["error"]["details"]["principal"], reader_id,
        "the breach is reported against the caller's own principal"
    );
    assert_eq!(body["error"]["details"]["current"], 1);
    assert_eq!(body["error"]["details"]["limit"], 1);
}

// ── T2: SSE client disconnect releases the principal slot ────────────────────

/// An SSE client disconnect RELEASES the principal slot (Issue #3678). The
/// SSE-worker-drop path — the `spawn_blocking` worker dropping its held
/// `Subscription` once the client's receiver goes away — is distinct from a
/// plain `drop(sub)`, and this proves it frees the per-principal count.
#[tokio::test]
async fn sse_client_disconnect_releases_principal_slot() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(4),
    );
    let client = Arc::new(build_server_client(
        Arc::clone(&db),
        store,
        AuthMode::Required,
    ));

    // Open a live SSE stream as the reader (consumes 1 slot). `send()` buffers
    // the infinite SSE body and never returns, so drive it on a spawned task and
    // abort it later to simulate a client disconnect (dropping the in-process
    // oneshot future drops the SSE response body — the mpsc receiver — closing
    // the worker's sender).
    let stream_task = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            let _ = client
                .get("/changes/stream")
                .header("authorization", &format!("Bearer {READER_TOKEN}"))
                .send()
                .await;
        })
    };

    // Wait until the subscription is registered against the reader's bucket.
    let mut registered = false;
    for _ in 0..200 {
        if principal_count(&db, &reader_id) == 1 {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        registered,
        "SSE stream subscription should register the reader slot"
    );

    // Disconnect the client.
    stream_task.abort();

    // The worker frees the slot on drop; it notices the closed channel on the
    // next delivered record or its idle poll. Commit changes to wake it promptly,
    // then assert the reader's slot returns to zero (no leak on disconnect).
    let mut released = false;
    for i in 0..120i64 {
        if principal_count(&db, &reader_id) == 0 {
            released = true;
            break;
        }
        // A committed change wakes the worker (blocked in recv_timeout) so it
        // observes the closed channel and drops its subscription.
        let _ = db.create_node("Ping", PropertyMapBuilder::new().insert("n", i).build());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        released,
        "SSE client disconnect must release the reader's principal slot"
    );

    // The slot is genuinely free: the reader can re-subscribe.
    let _fresh = db
        .subscribe_changes_for_principal(Some(&reader_id), ChangeFilter::all())
        .expect("reader re-subscribes after the disconnected slot is released");
}

// ── T3: HTTP await buckets by the real principal, not "anonymous" ────────────

/// The HTTP `POST /changes/await` route buckets the quota by the caller's REAL
/// authenticated principal (Issue #3678), not the shared "anonymous" fallback.
/// RED if every HTTP caller were collapsed into one "anonymous" bucket: the
/// reader's (then-empty) anonymous bucket would not reject, and the reported
/// principal would read "anonymous" instead of the reader's id.
#[tokio::test]
async fn http_await_buckets_by_real_principal_not_anonymous() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(1),
    );
    // Fill ONLY the reader's bucket.
    let _held = db
        .subscribe_changes_for_principal(Some(&reader_id), ChangeFilter::all())
        .expect("prefill reader bucket");

    let client = build_server_client(Arc::clone(&db), store, AuthMode::Required);

    // Reader: bucket full → quota-rejected, reported against the reader's REAL id.
    let resp = client
        .post("/changes/await")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&json!({ "timeout_ms": 100 }))
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        200,
        "MCP-over-HTTP surfaces the error as a 200 envelope"
    );
    let body: Value = resp.json();
    assert_eq!(
        body["error"]["code"], "RESOURCE_EXHAUSTED",
        "reader's bucket is full: {body}"
    );
    assert_eq!(
        body["error"]["details"]["principal"], reader_id,
        "the quota is charged to the reader's real principal, not \"anonymous\""
    );
    assert_ne!(body["error"]["details"]["principal"], "anonymous");

    // A DIFFERENT authenticated principal (writer) with an empty bucket is NOT
    // blocked by the reader being full — it long-polls and times out cleanly,
    // proving the two callers occupy independent buckets.
    let resp = client
        .post("/changes/await")
        .header("authorization", &format!("Bearer {WRITER_TOKEN}"))
        .json(&json!({ "timeout_ms": 100 }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = resp.json();
    assert!(
        body.get("error").is_none(),
        "writer (empty bucket) must NOT be quota-rejected: {body}"
    );
    assert_eq!(
        body["timed_out"], true,
        "writer's long-poll times out cleanly (independent bucket)"
    );
}
