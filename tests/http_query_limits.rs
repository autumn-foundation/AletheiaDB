//! Integration tests for per-query resource limits on the HTTP `/query`
//! surface (Issue #3368).
//!
//! Covers the three enforceable dimensions — wall-clock timeout, result-row
//! cap, and result-byte cap — plus their server default / per-call override /
//! operator ceiling composition, the structured `{code, retriable, details}`
//! error contract (429 / 413 / 422), the disabled-config escape hatch,
//! concurrency isolation, and auth-vs-limits ordering (auth rejects first).
//!
//! The wall-clock timeout is additionally unit-tested for determinism in
//! `src/http/handlers.rs` (a real slow query can't be forced deterministically
//! over the wire); these integration tests exercise the row/byte caps and the
//! override/ceiling logic, which are deterministic end-to-end.
//!
//! Run with: `cargo test --test http_query_limits --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::http::{
    AppState, AuthState, QueryLimitsConfig, RowOverflowPolicy, ServerConfig, build_test_router,
    build_test_router_with_auth,
};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build an anonymous-mode client with the given per-query limits.
fn client_with_limits(limits: QueryLimitsConfig) -> (TestClient, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .query_limits(limits)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    (TestApp::from_router(router), db)
}

/// Build a required-auth client with the given limits, returning the shared
/// key store so the test can mint a valid credential.
fn auth_client_with_limits(
    limits: QueryLimitsConfig,
) -> (TestClient, Arc<AuthStore>, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    let store = Arc::new(AuthStore::new());
    let auth = AuthState::new(store.clone(), AuthMode::Required);
    let config = ServerConfig::builder().query_limits(limits).build();
    let router = build_test_router_with_auth(state, auth, &config).expect("build router");
    (TestApp::from_router(router), store, db)
}

async fn post_query(client: &TestClient, body: &Value) -> (u16, Value) {
    let resp = client.post("/query").json(body).send().await;
    let status = resp.status.as_u16();
    let json = serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null);
    (status, json)
}

async fn post_query_with_key(client: &TestClient, key: &str, body: &Value) -> (u16, Value) {
    let resp = client
        .post("/query")
        .header("Authorization", &format!("Bearer {key}"))
        .json(body)
        .send()
        .await;
    let status = resp.status.as_u16();
    let json = serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null);
    (status, json)
}

fn seed_people(db: &AletheiaDB, count: usize) {
    for i in 0..count {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("idx", i as i64).build(),
        )
        .expect("seed node");
    }
}

fn find_people() -> Value {
    json!({ "operation": "find_node", "label": "Person" })
}

/// Limits with a small row cap; every other dimension unlimited.
fn row_cap(max_rows: usize, policy: RowOverflowPolicy) -> QueryLimitsConfig {
    QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: max_rows,
        max_result_rows: 0,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: policy,
    }
}

// ---------------------------------------------------------------------------
// Result-row cap
// ---------------------------------------------------------------------------

/// Truncate policy: an over-cap result is capped and flagged `truncated: true`.
#[tokio::test]
async fn row_cap_truncate_caps_rows_and_flags_truncated() {
    let (client, db) = client_with_limits(row_cap(2, RowOverflowPolicy::Truncate));
    seed_people(&db, 5);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 200, "truncation is a success, not an error: {body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["truncated"], true, "must flag truncation");
    assert_eq!(
        body["data"].as_array().map(Vec::len),
        Some(2),
        "result must be capped to the row limit"
    );
}

/// A result at or under the cap is not flagged truncated.
#[tokio::test]
async fn row_cap_not_tripped_leaves_response_unflagged() {
    let (client, db) = client_with_limits(row_cap(10, RowOverflowPolicy::Truncate));
    seed_people(&db, 3);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    assert!(
        body.get("truncated").is_none(),
        "truncated must be omitted when nothing was dropped: {body}"
    );
    assert_eq!(body["data"].as_array().map(Vec::len), Some(3));
}

/// Reject policy: an over-cap result is a structured 413.
#[tokio::test]
async fn row_cap_reject_returns_413_with_details() {
    let (client, db) = client_with_limits(row_cap(2, RowOverflowPolicy::Reject));
    seed_people(&db, 5);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(
        status, 413,
        "reject policy must error, not truncate: {body}"
    );
    assert_eq!(body["success"], false);
    assert_eq!(body["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(body["retriable"], false);
    assert_eq!(body["details"]["dimension"], "result_rows");
    assert_eq!(body["details"]["limit"], 2);
    assert_eq!(body["details"]["consumed"], 5);
}

// ---------------------------------------------------------------------------
// Result-byte cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn byte_cap_exceeded_returns_413_with_details() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 0,
        max_result_rows: 0,
        default_max_response_bytes: 16, // tiny — any real result blows past it
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, db) = client_with_limits(limits);
    seed_people(&db, 3);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 413, "oversized response must be rejected: {body}");
    assert_eq!(body["success"], false);
    assert_eq!(body["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(body["retriable"], false);
    assert_eq!(body["details"]["dimension"], "result_bytes");
    assert_eq!(body["details"]["limit"], 16);
    assert!(
        body["details"]["consumed"].as_u64().unwrap() > 16,
        "consumed must report the actual serialized size"
    );
}

// ---------------------------------------------------------------------------
// Per-call override / ceiling composition
// ---------------------------------------------------------------------------

/// An override within the ceiling is honored (here: a tighter row cap than the
/// server default triggers truncation at the lower bound).
#[tokio::test]
async fn override_within_ceiling_is_honored_tighter_than_default() {
    // Default cap 100, ceiling 100. Caller self-limits to 1.
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 100,
        max_result_rows: 100,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, db) = client_with_limits(limits);
    seed_people(&db, 5);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": 1 }
        }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["truncated"], true);
    assert_eq!(body["data"].as_array().map(Vec::len), Some(1));
}

/// An override above the operator ceiling is rejected 422 INVALID_ARGUMENT
/// before any DB work.
#[tokio::test]
async fn override_above_ceiling_returns_422() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 10,
        max_result_rows: 100, // ceiling
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, _db) = client_with_limits(limits);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": 1000 } // > ceiling 100
        }),
    )
    .await;
    assert_eq!(
        status, 422,
        "over-ceiling override must be rejected: {body}"
    );
    assert_eq!(body["success"], false);
    assert_eq!(body["code"], "INVALID_ARGUMENT");
    assert_eq!(body["retriable"], false);
    assert_eq!(body["details"]["dimension"], "result_rows");
    assert_eq!(body["details"]["requested"], 1000);
    assert_eq!(body["details"]["ceiling"], 100);
}

/// A timeout override above the ceiling is likewise rejected 422 (dimension =
/// wall_clock_timeout), demonstrating per-dimension ceiling enforcement.
#[tokio::test]
async fn timeout_override_above_ceiling_returns_422() {
    let (client, _db) = client_with_limits(QueryLimitsConfig::default());

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "timeout_ms": 999_999_999 }
        }),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(body["code"], "INVALID_ARGUMENT");
    assert_eq!(body["details"]["dimension"], "wall_clock_timeout");
}

// ---------------------------------------------------------------------------
// Disabled config — no enforcement
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_limits_do_not_enforce_anything() {
    let (client, db) = client_with_limits(QueryLimitsConfig::disabled());
    seed_people(&db, 50);

    // A large result passes untouched, no truncation.
    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 200);
    assert!(body.get("truncated").is_none());
    assert_eq!(body["data"].as_array().map(Vec::len), Some(50));

    // Even an absurd per-call override is ignored (not rejected) when disabled.
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": 999_999_999, "timeout_ms": 0 }
        }),
    )
    .await;
    assert_eq!(status, 200, "disabled config ignores overrides: {body}");
    assert_eq!(body["data"].as_array().map(Vec::len), Some(50));
}

// ---------------------------------------------------------------------------
// Concurrency — limits are per-request, no shared-state poisoning
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_queries_under_limits_all_succeed() {
    let (client, db) = client_with_limits(row_cap(1000, RowOverflowPolicy::Truncate));
    seed_people(&db, 10);
    let client = Arc::new(client);

    let mut handles = Vec::new();
    for _ in 0..16 {
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            let (status, body) = post_query(&client, &find_people()).await;
            assert_eq!(status, 200, "concurrent query failed: {body}");
            assert_eq!(body["success"], true);
            // Well under the cap → never truncated.
            assert!(body.get("truncated").is_none());
        }));
    }
    for h in handles {
        h.await.expect("task panicked");
    }
}

// ---------------------------------------------------------------------------
// Auth composes with limits: auth rejects FIRST
// ---------------------------------------------------------------------------

/// Identical limit behavior applies under authenticated required-mode as under
/// anonymous mode: an over-ceiling override is a 422 once authenticated.
#[tokio::test]
async fn authenticated_request_gets_same_limit_behavior() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 10,
        max_result_rows: 100,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, store, _db) = auth_client_with_limits(limits);
    let (_p, key) = store.create_key("writer", Role::Writer).expect("mint key");

    let (status, body) = post_query_with_key(
        &client,
        &key,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": 1000 }
        }),
    )
    .await;
    assert_eq!(
        status, 422,
        "authenticated over-ceiling override → 422: {body}"
    );
    assert_eq!(body["code"], "INVALID_ARGUMENT");
}

/// An unauthenticated request that ALSO breaches a limit is rejected 401 —
/// authentication runs before any limit logic, so the caller never sees a
/// 422/413. (Required mode, no credential presented.)
#[tokio::test]
async fn unauthenticated_over_limit_request_is_401_not_422() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 10,
        max_result_rows: 100,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, _store, _db) = auth_client_with_limits(limits);

    // No Authorization header + an over-ceiling override.
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": 1000 }
        }),
    )
    .await;
    assert_eq!(status, 401, "auth must reject before limit logic: {body}");
    assert_eq!(body["code"], "UNAUTHENTICATED");
}

// ---------------------------------------------------------------------------
// Backward compatibility — a body with no "limits" key behaves as before
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_without_limits_key_uses_server_defaults() {
    let (client, db) = client_with_limits(QueryLimitsConfig::default());
    seed_people(&db, 3);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    assert!(body.get("truncated").is_none());
    assert_eq!(body["data"].as_array().map(Vec::len), Some(3));
}

/// A `"limits": null` field is deserialized as absent (`Option::None`) and
/// therefore behaves exactly as no `"limits"` key at all — server defaults.
#[tokio::test]
async fn null_limits_behaves_as_absent() {
    let (client, db) = client_with_limits(QueryLimitsConfig::default());
    seed_people(&db, 3);

    let (status, body) = post_query(
        &client,
        &json!({ "operation": "find_node", "label": "Person", "limits": null }),
    )
    .await;
    assert_eq!(status, 200, "null limits must parse as absent: {body}");
    assert_eq!(body["success"], true);
    assert!(body.get("truncated").is_none());
    assert_eq!(body["data"].as_array().map(Vec::len), Some(3));
}

/// A malformed `limits` field (out-of-range or wrong-typed) is a clean client
/// error (4xx) from the JSON extractor — never a 500 (server fault).
#[tokio::test]
async fn malformed_limits_is_a_clean_4xx_not_500() {
    let (client, _db) = client_with_limits(QueryLimitsConfig::default());

    // `timeout_ms` is a u64; a negative value cannot deserialize.
    let (status, _body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "timeout_ms": -1 }
        }),
    )
    .await;
    assert!(
        (400..500).contains(&status),
        "negative timeout_ms must be a 4xx, got {status}"
    );

    // Wrong type (string where a number is expected) is likewise a clean 4xx.
    let (status, _body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": "not-a-number" }
        }),
    )
    .await;
    assert!(
        (400..500).contains(&status),
        "wrong-typed override must be a 4xx, got {status}"
    );
}

// ---------------------------------------------------------------------------
// Write operations are exempt from the result row/byte caps (Issue #3368
// review MUST-FIX 1) — a committed write is never truncated or 413'd.
// ---------------------------------------------------------------------------

/// Under a Reject row policy with a tight per-call `max_result_rows`, a bulk
/// WRITE that produces more acknowledgement rows than the cap must NOT 413:
/// the write is exempt, and every node must actually persist.
#[tokio::test]
async fn write_op_is_exempt_from_row_reject_and_persists() {
    // Reject policy, cap 1, no ceiling (so the per-call override is honored).
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 100,
        max_result_rows: 0,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Reject,
    };
    let (client, db) = client_with_limits(limits);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_create_nodes",
            "nodes": [
                { "label": "Person" },
                { "label": "Person" },
                { "label": "Person" },
            ],
            "limits": { "max_result_rows": 1 } // would 413 a read of 3 rows
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a write must be exempt from the row Reject cap: {body}"
    );
    assert_eq!(body["success"], true);
    assert!(
        body.get("truncated").is_none(),
        "a write ack is never flagged truncated: {body}"
    );
    assert_eq!(
        body["data"].as_array().map(Vec::len),
        Some(3),
        "the full write acknowledgement is returned"
    );
    assert_eq!(db.node_count(), 3, "all three nodes must persist");
}

/// Under a Truncate policy with a tight per-call row cap, a bulk WRITE is not
/// truncated and is not flagged `truncated` — writes bypass the row cap.
#[tokio::test]
async fn write_op_is_not_truncated_under_truncate_policy() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 1, // tiny default cap
        max_result_rows: 0,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, db) = client_with_limits(limits);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_create_nodes",
            "nodes": [
                { "label": "Person" },
                { "label": "Person" },
                { "label": "Person" },
            ]
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(
        body.get("truncated").is_none(),
        "write ack must not be flagged truncated: {body}"
    );
    assert_eq!(body["data"].as_array().map(Vec::len), Some(3));
    assert_eq!(db.node_count(), 3);
}

// ---------------------------------------------------------------------------
// Concurrency isolation — distinct per-call overrides yield distinct outcomes
// on the SAME server, proving no shared-state bleed between requests.
// ---------------------------------------------------------------------------

/// Four concurrent `/query` requests against one server, each with a different
/// per-call override, must each get its own outcome: a default truncation, a
/// byte-cap 413, an over-ceiling 422, and an unlimited-within-ceiling 200.
/// (The row overflow *policy* is server-wide, so the 413 case uses the byte
/// dimension — still a genuine per-request 413.)
#[tokio::test]
async fn concurrent_requests_with_distinct_overrides_are_isolated() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 2, // default cap → truncates the 5-row read
        max_result_rows: 10,        // ceiling for row overrides
        default_max_response_bytes: 0, // unlimited by default
        max_response_bytes: 0,      // no byte ceiling → any byte override ok
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, db) = client_with_limits(limits);
    seed_people(&db, 5);
    let client = Arc::new(client);

    // A: no override → default cap 2 → truncated.
    let a = {
        let client = client.clone();
        tokio::spawn(async move { post_query(&client, &find_people()).await })
    };
    // B: tiny byte override → the 5-row response blows past 8 bytes → 413.
    let b = {
        let client = client.clone();
        tokio::spawn(async move {
            post_query(
                &client,
                &json!({
                    "operation": "find_node",
                    "label": "Person",
                    "limits": { "max_response_bytes": 8 }
                }),
            )
            .await
        })
    };
    // C: row override above the ceiling → 422.
    let c = {
        let client = client.clone();
        tokio::spawn(async move {
            post_query(
                &client,
                &json!({
                    "operation": "find_node",
                    "label": "Person",
                    "limits": { "max_result_rows": 1000 }
                }),
            )
            .await
        })
    };
    // D: row override at the ceiling (10 > 5 seeded) → full result, untruncated.
    let d = {
        let client = client.clone();
        tokio::spawn(async move {
            post_query(
                &client,
                &json!({
                    "operation": "find_node",
                    "label": "Person",
                    "limits": { "max_result_rows": 10 }
                }),
            )
            .await
        })
    };

    let (a, b, c, d) = (
        a.await.unwrap(),
        b.await.unwrap(),
        c.await.unwrap(),
        d.await.unwrap(),
    );

    // A: truncated to 2.
    assert_eq!(a.0, 200, "A: {}", a.1);
    assert_eq!(a.1["truncated"], true);
    assert_eq!(a.1["data"].as_array().map(Vec::len), Some(2));

    // B: byte-cap 413.
    assert_eq!(b.0, 413, "B: {}", b.1);
    assert_eq!(b.1["details"]["dimension"], "result_bytes");

    // C: over-ceiling 422.
    assert_eq!(c.0, 422, "C: {}", c.1);
    assert_eq!(c.1["details"]["dimension"], "result_rows");

    // D: full, untruncated.
    assert_eq!(d.0, 200, "D: {}", d.1);
    assert!(d.1.get("truncated").is_none());
    assert_eq!(d.1["data"].as_array().map(Vec::len), Some(5));
}

// ---------------------------------------------------------------------------
// Auth parity — an authenticated-but-unauthorized caller is rejected 403
// BEFORE limit validation, even carrying an over-ceiling override.
// ---------------------------------------------------------------------------

/// A Reader (no write permission) attempting a WRITE operation while ALSO
/// carrying an over-ceiling limit override is rejected 403 PERMISSION_DENIED —
/// authorization runs before limit validation, so the caller never sees a 422.
#[tokio::test]
async fn authorized_check_precedes_limit_validation() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 10,
        max_result_rows: 100, // finite ceiling the override breaches
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, store, _db) = auth_client_with_limits(limits);
    let (_p, key) = store.create_key("reader", Role::Reader).expect("mint key");

    let (status, body) = post_query_with_key(
        &client,
        &key,
        &json!({
            "operation": "create_node",
            "label": "Person",
            "limits": { "max_result_rows": 1000 } // over ceiling 100
        }),
    )
    .await;
    assert_eq!(
        status, 403,
        "role denial must precede limit validation: {body}"
    );
    assert_eq!(body["code"], "PERMISSION_DENIED");
}
