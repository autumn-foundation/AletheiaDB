//! HTTP + MCP parity for the namespace management tools (Issue #3349 PR3b):
//! `create_namespace` / `list_namespaces` / `describe_namespace`.
//!
//! These prove the three tools are routed on **both** the HTTP surface and the
//! MCP-over-HTTP (`/mcp`) surface, carry the correct RBAC classes
//! (`create_namespace` = write, the other two = read), and forward to the exact
//! main-crate MCP methods so their #3234 structured errors (CONFLICT /
//! INVALID_ARGUMENT / NOT_FOUND) surface unchanged.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";

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

fn client() -> TestClient {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    build_server_client(db, make_store(), AuthMode::Required)
}

async fn get(client: &TestClient, uri: &str, token: &str) -> (u16, Value) {
    let resp = client
        .get(uri)
        .header("authorization", &format!("Bearer {token}"))
        .send()
        .await;
    let status = resp.status.as_u16();
    (
        status,
        serde_json::from_str(&resp.text()).expect("JSON body"),
    )
}

async fn post(client: &TestClient, uri: &str, token: &str, body: &Value) -> (u16, Value) {
    let resp = client
        .post(uri)
        .header("authorization", &format!("Bearer {token}"))
        .json(body)
        .send()
        .await;
    let status = resp.status.as_u16();
    (
        status,
        serde_json::from_str(&resp.text()).expect("JSON body"),
    )
}

/// One `/mcp` tools/call round-trip; returns the parsed tool payload.
async fn mcp_call(client: &TestClient, token: &str, name: &str, args: Value) -> Value {
    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {token}"))
        .json(&rpc)
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("JSON body");
    // The tool result is the text content of the first content block.
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool text content for {name}: {body}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("tool payload for {name} not JSON ({e}): {text}"))
}

#[tokio::test]
async fn http_create_list_describe_round_trip() {
    let client = client();

    // create_namespace (write) via POST /namespaces.
    let (status, created) = post(
        &client,
        "/namespaces",
        ADMIN_TOKEN,
        &json!({ "name": "agent:planner", "description": "planner scope" }),
    )
    .await;
    assert_eq!(status, 200, "create must be 200: {created}");
    assert_eq!(created["name"], "agent:planner");
    assert_eq!(created["description"], "planner scope");
    assert!(created.get("created_at").is_some(), "created_at: {created}");

    // list_namespaces (read) via GET /namespaces — default first, then created.
    let (status, listed) = get(&client, "/namespaces", ADMIN_TOKEN).await;
    assert_eq!(status, 200);
    let namespaces = listed["namespaces"].as_array().expect("array");
    assert_eq!(namespaces[0]["name"], "default");
    assert!(
        namespaces.iter().any(|n| n["name"] == "agent:planner"),
        "created namespace listed: {listed}"
    );

    // describe_namespace (read, POST body) via POST /namespaces/describe.
    let (status, described) = post(
        &client,
        "/namespaces/describe",
        ADMIN_TOKEN,
        &json!({ "name": "agent:planner" }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(described["name"], "agent:planner");
    assert!(described.get("node_count").is_some(), "counts: {described}");
}

#[tokio::test]
async fn http_duplicate_is_conflict_and_unknown_is_not_found() {
    let client = client();
    post(
        &client,
        "/namespaces",
        ADMIN_TOKEN,
        &json!({ "name": "agent:a" }),
    )
    .await;

    // Duplicate → CONFLICT.
    let (_status, dup) = post(
        &client,
        "/namespaces",
        ADMIN_TOKEN,
        &json!({ "name": "agent:a" }),
    )
    .await;
    assert_eq!(
        dup.pointer("/error/code").and_then(|c| c.as_str()),
        Some("CONFLICT"),
        "duplicate must be CONFLICT: {dup}"
    );

    // Unknown describe → NOT_FOUND with details.namespace.
    let (_status, missing) = post(
        &client,
        "/namespaces/describe",
        ADMIN_TOKEN,
        &json!({ "name": "agent:missing" }),
    )
    .await;
    assert_eq!(
        missing.pointer("/error/code").and_then(|c| c.as_str()),
        Some("NOT_FOUND"),
        "unknown describe must be NOT_FOUND: {missing}"
    );
    assert_eq!(
        missing
            .pointer("/error/details/namespace")
            .and_then(|n| n.as_str()),
        Some("agent:missing"),
        "NOT_FOUND carries the namespace: {missing}"
    );
}

#[tokio::test]
async fn reader_denied_create_but_allowed_list() {
    let client = client();

    // Reader cannot create (write-class) → 403 PERMISSION_DENIED.
    let (status, denied) = post(
        &client,
        "/namespaces",
        READER_TOKEN,
        &json!({ "name": "agent:z" }),
    )
    .await;
    assert_eq!(status, 403, "reader create must be 403: {denied}");
    assert_eq!(
        denied.pointer("/error/code").and_then(|c| c.as_str()),
        Some("PERMISSION_DENIED"),
        "reader create is PERMISSION_DENIED: {denied}"
    );

    // Reader can list (read-class).
    let (status, listed) = get(&client, "/namespaces", READER_TOKEN).await;
    assert_eq!(status, 200, "reader list must be 200: {listed}");
    assert!(listed["namespaces"].is_array());
}

#[tokio::test]
async fn mcp_tools_call_round_trips_the_three_tools() {
    let client = client();

    // A POST-body autumn tool projects to MCP under a `body` wrapper (same as
    // create_edge / create_node); the GET tools take flat arguments.
    let created = mcp_call(
        &client,
        ADMIN_TOKEN,
        "create_namespace",
        json!({ "body": { "name": "agent:mcp", "description": "via mcp" } }),
    )
    .await;
    assert_eq!(created["name"], "agent:mcp");

    let listed = mcp_call(&client, ADMIN_TOKEN, "list_namespaces", json!({})).await;
    assert!(
        listed["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "agent:mcp"),
        "MCP list shows the created namespace: {listed}"
    );

    let described = mcp_call(
        &client,
        ADMIN_TOKEN,
        "describe_namespace",
        json!({ "body": { "name": "agent:mcp" } }),
    )
    .await;
    assert_eq!(described["name"], "agent:mcp");
    assert_eq!(described["description"], "via mcp");
}
