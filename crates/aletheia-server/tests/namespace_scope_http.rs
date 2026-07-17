//! HTTP namespace-scope forwarding tests (Issue #3349 PR3a hardening, FIX1/FIX2).
//!
//! The HTTP read routes deserialize typed Query/body structs and forward a
//! `namespace` argument into `dispatch_tool_json` — the same seam the MCP layer
//! parses via `parse_opt_scope` / `reject_unsupported_scope`. These tests assert
//! that an HTTP read therefore scopes **identically** to its MCP twin: a scoped
//! HTTP read isolates other namespaces, an omitted read is `default`-only
//! (isolated-by-default — never silently every-namespace), an unknown namespace
//! is a structured `NOT_FOUND` (not a silent unscoped result), and the
//! non-scoping tools reject a narrowing scope with `INVALID_ARGUMENT`.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";

fn make_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("insert admin key");
    store
}

/// A DB with one `default` node (Dave) and one `agent:planner` node (Eve).
/// Returns `(db, dave_id, eve_id)`.
fn fixture() -> (Arc<AletheiaDB>, u64, u64) {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let dave = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Dave").build(),
        )
        .expect("create default node")
        .as_u64();
    let eve = db
        .create_node_in_namespace(
            "Person",
            PropertyMapBuilder::new().insert("name", "Eve").build(),
            "agent:planner",
        )
        .expect("create namespaced node")
        .as_u64();
    (db, dave, eve)
}

fn client(db: Arc<AletheiaDB>) -> TestClient {
    build_server_client(db, make_store(), AuthMode::Required)
}

async fn get(client: &TestClient, uri: &str) -> Value {
    let resp = client
        .get(uri)
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .send()
        .await;
    serde_json::from_str(&resp.text()).expect("JSON body")
}

async fn post(client: &TestClient, uri: &str, body: &Value) -> Value {
    let resp = client
        .post(uri)
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .json(body)
        .send()
        .await;
    serde_json::from_str(&resp.text()).expect("JSON body")
}

#[tokio::test]
async fn http_get_node_scopes_identically_to_mcp() {
    let (db, _dave, eve) = fixture();
    let client = client(db);

    // In-scope: Eve is returned with her namespace.
    let scoped = get(&client, &format!("/nodes/{eve}?namespace=agent:planner")).await;
    assert_eq!(
        scoped.get("namespace").and_then(|n| n.as_str()),
        Some("agent:planner"),
        "scoped HTTP get_node must return the in-scope node: {scoped}"
    );

    // Omitted (default-only, FIX2): Eve (agent:planner) is NOT_FOUND — HTTP does
    // NOT silently return the unscoped node.
    let omitted = get(&client, &format!("/nodes/{eve}")).await;
    assert_eq!(
        omitted.pointer("/error/code").and_then(|c| c.as_str()),
        Some("NOT_FOUND"),
        "omitted HTTP get_node must be default-only (never silently unscoped): {omitted}"
    );

    // Unknown namespace: structured NOT_FOUND with details.namespace.
    let unknown = get(&client, &format!("/nodes/{eve}?namespace=agent:nope")).await;
    assert_eq!(
        unknown.pointer("/error/code").and_then(|c| c.as_str()),
        Some("NOT_FOUND"),
        "unknown namespace must be a structured error, not silent unscoped: {unknown}"
    );
    assert_eq!(
        unknown
            .pointer("/error/details/namespace")
            .and_then(|c| c.as_str()),
        Some("agent:nope"),
        "NOT_FOUND must carry details.namespace: {unknown}"
    );
}

#[tokio::test]
async fn http_list_nodes_scope_isolates_and_defaults() {
    let (db, _dave, _eve) = fixture();
    let client = client(db);

    // Scoped to agent:planner: only Eve.
    let planner = get(&client, "/nodes?label=Person&namespace=agent:planner").await;
    let ptext = planner.to_string();
    assert!(
        ptext.contains("Eve") && !ptext.contains("Dave"),
        "scoped HTTP list_nodes must isolate: {planner}"
    );

    // Omitted (default-only): only Dave, never Eve.
    let omitted = get(&client, "/nodes?label=Person").await;
    let otext = omitted.to_string();
    assert!(
        otext.contains("Dave") && !otext.contains("Eve"),
        "omitted HTTP list_nodes must be default-only: {omitted}"
    );

    // "all": both.
    let all = get(&client, "/nodes?label=Person&namespace=all").await;
    let atext = all.to_string();
    assert!(
        atext.contains("Dave") && atext.contains("Eve"),
        "namespace=all must see both: {all}"
    );
}

#[tokio::test]
async fn http_list_edges_rejects_narrowing_scope() {
    let (db, _dave, _eve) = fixture();
    let client = client(db);
    // list_edges does not scope in v1: a narrowing scope is a structured error,
    // never a silent unscoped result.
    let out = get(&client, "/edges?namespace=agent:planner").await;
    assert_eq!(
        out.pointer("/error/code").and_then(|c| c.as_str()),
        Some("INVALID_ARGUMENT"),
        "HTTP list_edges must reject a narrowing scope: {out}"
    );
    // "all" is accepted (no-op).
    let all = get(&client, "/edges?namespace=all").await;
    assert!(
        all.pointer("/error").is_none(),
        "HTTP list_edges with namespace=all must not error: {all}"
    );
}

#[tokio::test]
async fn http_find_nodes_at_time_scope_isolates() {
    let (db, _dave, _eve) = fixture();
    let client = client(db);
    let body = json!({
        "label": "Person",
        "valid_time": "2999-01-01T00:00:00Z",
        "namespace": "agent:planner"
    });
    let scoped = post(&client, "/nodes/find_at_time", &body).await;
    let stext = scoped.to_string();
    assert!(
        stext.contains("Eve") && !stext.contains("Dave"),
        "scoped HTTP find_nodes_at_time must isolate: {scoped}"
    );

    // Omitted (default-only): only Dave.
    let omitted_body = json!({ "label": "Person", "valid_time": "2999-01-01T00:00:00Z" });
    let omitted = post(&client, "/nodes/find_at_time", &omitted_body).await;
    let otext = omitted.to_string();
    assert!(
        otext.contains("Dave") && !otext.contains("Eve"),
        "omitted HTTP find_nodes_at_time must be default-only: {omitted}"
    );
}
