//! Route + behavior tests for the SEMANTIC-SEARCH analysis cluster (Issue #2907):
//! `semantic_path`, `concept_analogy`, `concept_mean`, `find_duplicate_candidates`,
//! `semantic_horizon`, and `context_aspects` — the six new HTTP projection
//! handlers added by PR #3640.
//!
//! Each handler forwards its typed all-optional request body through the shared
//! [`AletheiaMcpServer::dispatch_tool_json`] under the B4 resource-limits wiring,
//! exactly like `find_similar`. These tests drive the new crate's autumn
//! [`TestClient`] end-to-end so the handler forwarding + request-struct
//! deserialization + auth all execute, and assert the response is byte-identical
//! to the legacy MCP surface over the same `Arc<AletheiaDB>` (parsed-JSON
//! equality, `trace_id` + wall-clock-volatile bi-temporal timestamps normalized).
//!
//! The tool bodies are gated on the main crate's `semantic-search` feature
//! (Design A); this crate never enables that feature (there is no forwarding
//! feature, and neither the default nor the Code Coverage CI build passes it), so
//! the shared server returns a structured `FAILED_PRECONDITION`
//! (`required_feature: "semantic-search"`) which the handler forwards verbatim.
//! That is exactly the point of these tests: the *forwarding* path (build args
//! map → `dispatch_slow_read` → return the tool result JSON) executes regardless
//! of the feature, and parity with the legacy dispatch reference holds in every
//! build.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::mcp::{AletheiaMcpServer, EnableVectorIndexRequest};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";

struct Fixture {
    db: Arc<AletheiaDB>,
    store: Arc<AuthStore>,
    alice: u64,
    bob: u64,
    carol: u64,
}

fn make_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("insert admin key");
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("insert reader key");
    store
}

fn mcp_server(db: &Arc<AletheiaDB>) -> AletheiaMcpServer {
    AletheiaMcpServer::new(db.clone())
}

/// A shared DB with a `cosine` vector index on `embedding` (dim 4) and a small
/// graph: Alice --KNOWS--> {Bob, Carol}, each bearing a distinct embedding, so
/// the semantic tools have real node ids + a live index to reference.
fn fixture() -> Fixture {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let setup = mcp_server(&db);
    let enabled = setup.enable_vector_index(EnableVectorIndexRequest {
        property_name: "embedding".to_string(),
        dimensions: 4,
        distance_metric: Some("cosine".to_string()),
    });
    assert!(
        serde_json::from_str::<Value>(&enabled).expect("json")["success"] == json!(true),
        "vector index must enable: {enabled}"
    );

    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[0.10_f32, 0.20, 0.30, 0.40])
                .build(),
        )
        .expect("create alice")
        .as_u64();

    let mut ids = Vec::new();
    let mut idx = 1.0_f32;
    for name in ["Bob", "Carol"] {
        let target = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", name)
                    .insert_vector(
                        "embedding",
                        &[0.10 * idx, 0.20 * idx, 0.30 * idx, 0.40 * idx],
                    )
                    .build(),
            )
            .expect("create target");
        db.create_edge(
            aletheiadb::core::NodeId::new(alice).unwrap(),
            target,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        )
        .expect("create edge");
        ids.push(target.as_u64());
        idx += 1.0;
    }

    Fixture {
        db,
        store: make_store(),
        alice,
        bob: ids[0],
        carol: ids[1],
    }
}

/// Recursively drop the per-request `trace_id` and wall-clock-volatile
/// bi-temporal timestamps so an autumn response and a legacy reference compare.
fn normalize(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "trace_id"
                            | "valid_from"
                            | "valid_to"
                            | "transaction_from"
                            | "transaction_to"
                            | "transaction_time"
                            | "timestamp"
                    )
                })
                .map(|(k, val)| (k, normalize(val)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        other => other,
    }
}

async fn post(client: &TestClient, uri: &str, body: &Value, bearer: Option<&str>) -> (u16, Value) {
    let mut req = client.post(uri);
    if let Some(token) = bearer {
        req = req.header("authorization", &format!("Bearer {token}"));
    }
    let resp = req.json(body).send().await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).expect("new body is JSON");
    (status, body)
}

/// The six new `/semantic/*` routes paired with the legacy MCP tool name they
/// forward to and a valid request body that deserializes into the handler's
/// typed `*Body` struct (exercising every request-struct field of interest).
fn cases(fx: &Fixture) -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "/semantic/path",
            "semantic_path",
            json!({
                "start": fx.alice,
                "end": fx.carol,
                "property_name": "embedding",
                "max_depth": 3
            }),
        ),
        (
            "/semantic/analogy",
            "concept_analogy",
            json!({
                "a": fx.alice,
                "b": fx.bob,
                "c": fx.carol,
                "property_name": "embedding",
                "k": 5
            }),
        ),
        (
            "/semantic/mean",
            "concept_mean",
            json!({
                "nodes": [fx.alice, fx.bob, fx.carol],
                "property_name": "embedding",
                "k": 5
            }),
        ),
        (
            "/semantic/duplicates",
            "find_duplicate_candidates",
            json!({
                "node_id": fx.alice,
                "property_name": "embedding",
                "threshold": 0.9,
                "limit": 5
            }),
        ),
        (
            "/semantic/horizon",
            "semantic_horizon",
            json!({
                "seed": fx.alice,
                "property_name": "embedding",
                "threshold": 0.8,
                "max_depth": 2
            }),
        ),
        (
            "/semantic/aspects",
            "context_aspects",
            json!({
                "node_id": fx.alice,
                "property_name": "embedding",
                "k": 3,
                "include_vectors": true
            }),
        ),
    ]
}

// ════════════════════════════════════════════════════════════════════════════
// Byte-parity for all six semantic tools vs the legacy MCP dispatch surface,
// in both auth modes. The valid body deserializes into each handler's typed
// `*Body` struct, the handler rebuilds the raw args + forwards through
// `dispatch_tool_json`, and the returned tool-result JSON is byte-identical to
// the legacy reference over the same `Arc<AletheiaDB>`.
// ════════════════════════════════════════════════════════════════════════════

async fn assert_all_six_parity(mode: AuthMode, token: Option<&'static str>) {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), mode);
    let server = mcp_server(&fx.db);

    for (uri, tool, args) in cases(&fx) {
        let (status, autumn) = post(&client, uri, &args, token).await;
        assert_eq!(status, 200, "{uri}: {autumn}");
        let legacy: Value = serde_json::from_str(&server.dispatch_tool_json(tool, args.clone()))
            .unwrap_or_else(|e| panic!("legacy {tool} json: {e}"));
        assert_eq!(
            normalize(autumn.clone()),
            normalize(legacy),
            "{uri} parity vs legacy dispatch of {tool}"
        );
    }
}

#[tokio::test]
async fn all_six_byte_parity_required_mode() {
    assert_all_six_parity(AuthMode::Required, Some(READER_TOKEN)).await;
}

#[tokio::test]
async fn all_six_byte_parity_anonymous_mode() {
    assert_all_six_parity(AuthMode::Anonymous, None).await;
}

// ════════════════════════════════════════════════════════════════════════════
// Feature-off contract: this crate never compiles `semantic-search`, so every
// tool returns the structured #3234 `FAILED_PRECONDITION` envelope with
// `details.required_feature == "semantic-search"` (a well-formed structured
// response the handler forwards verbatim, HTTP 200). Confirms the handlers
// surface the actionable error rather than an "unknown tool".
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn all_six_return_structured_feature_unavailable_envelope() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);

    for (uri, tool, args) in cases(&fx) {
        let (status, body) = post(&client, uri, &args, Some(READER_TOKEN)).await;
        assert_eq!(status, 200, "{uri} returns 200 with in-band error: {body}");
        let err = &body["error"];
        assert!(
            err.is_object(),
            "{uri} must carry a structured error: {body}"
        );
        // #3234 uniform envelope: a stable `code` the caller can branch on.
        assert_eq!(
            err["code"], "FAILED_PRECONDITION",
            "{uri} feature-unavailable code: {body}"
        );
        assert_eq!(
            err["retriable"],
            json!(false),
            "{uri} non-retriable: {body}"
        );
        assert_eq!(
            err["details"]["required_feature"], "semantic-search",
            "{uri} names the missing feature: {body}"
        );
        assert_eq!(
            err["details"]["tool"], tool,
            "{uri} names the underlying tool: {body}"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Auth: all six tools are ReadClass. A keyless required-mode call is 401; a
// reader-role key reaches the handler (200, exercised by the parity tests).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn semantic_tools_require_credential_in_required_mode() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);

    for (uri, _tool, _args) in cases(&fx) {
        let resp = client.post(uri).json(&json!({})).send().await;
        assert_eq!(resp.status.as_u16(), 401, "{uri} unauth is 401");
        assert_eq!(
            resp.header("www-authenticate"),
            Some("Bearer"),
            "{uri} advertises Bearer"
        );
    }
}

#[tokio::test]
async fn semantic_tools_admin_key_also_reaches_handler() {
    // ReadClass admits admin as well as reader; confirm an admin key reaches the
    // handler (200 with the in-band feature-unavailable error) for every route.
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);

    for (uri, _tool, args) in cases(&fx) {
        let (status, body) = post(&client, uri, &args, Some(ADMIN_TOKEN)).await;
        assert_eq!(status, 200, "{uri} admin reaches handler: {body}");
        assert_eq!(
            body["error"]["code"], "FAILED_PRECONDITION",
            "{uri} admin sees the in-band error: {body}"
        );
    }
}
