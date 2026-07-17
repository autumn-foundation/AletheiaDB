//! Parity + behavior tests for the TRAVERSAL + TEMPORAL cluster (Issue #3524,
//! PR4): `traverse`, `get_node_history`, `get_edge_history`, `get_node_at_time`,
//! `get_edge_at_time`, `list_changes`, `get_node_at_valid_time`,
//! `get_node_at_transaction_time`, `diff_node_versions`,
//! `get_edge_at_valid_time`, `get_edge_at_transaction_time`,
//! `diff_edge_versions`.
//!
//! Each test drives the new crate's autumn [`TestClient`] and asserts the
//! response equals the **legacy MCP surface** over the same `Arc<AletheiaDB>` —
//! byte-for-byte (parsed-JSON equality, `trace_id` + wall-clock-volatile
//! bi-temporal timestamps normalized) — in both anonymous and required auth
//! modes. The legacy reference is the typed per-tool method where one exists
//! (`traverse`/`get_node_history`/`get_edge_history`/`get_node_at_time`/
//! `get_edge_at_time`/`list_changes`) and `dispatch_tool_json` for the six tools
//! that have no public typed method. Plus: `traverse` behavior-pinned assertions
//! (#3225), #3353 budget shaping for the two budgetable reads (`traverse`,
//! `get_node_history`), #3360 snapshot-anchored cursor paging for `traverse`,
//! #3232 temporal block presence, and #3220 vector elision on the traversal
//! path.

use std::collections::BTreeSet;
use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Role};
use aletheiadb::mcp::{
    AletheiaMcpServer, GetEdgeAtTimeRequest, GetEdgeHistoryRequest, GetNodeAtTimeRequest,
    GetNodeHistoryRequest, ListChangesRequest, UpdateEdgeRequest, UpdateNodeRequest,
};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const EMBEDDING: &[f32] = &[0.10, 0.20, 0.30, 0.40];

/// A long note so a single traversal result / a single node history comfortably
/// exceeds a modest byte budget, forcing the #3353 ladder to shape it down.
const LONG_NOTE: &str = "a deliberately long note property, repeated so the full \
     serialized response comfortably exceeds a moderate byte budget and the #3353 \
     ladder must shape it down: lorem ipsum dolor sit amet consectetur adipiscing \
     elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua ut enim \
     ad minim veniam quis nostrud exercitation ullamco laboris nisi ut aliquip ex";

/// A far-future valid time that resolves every committed fact.
const FUTURE_VT: &str = "2999-01-01T00:00:00Z";

struct Fixture {
    db: Arc<AletheiaDB>,
    store: Arc<AuthStore>,
    alice: u64,
    bob: u64,
    edge: u64,
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

/// A shared DB with Alice --KNOWS--> Bob (both bearing an embedding), each entity
/// carrying two versions so history / diff / at-time reads have real depth.
fn fixture() -> Fixture {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30_i64)
                .insert("note", LONG_NOTE)
                .insert_vector("embedding", EMBEDDING)
                .build(),
        )
        .expect("create alice")
        .as_u64();
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25_i64)
                .insert("note", LONG_NOTE)
                .insert_vector("embedding", EMBEDDING)
                .build(),
        )
        .expect("create bob")
        .as_u64();
    let edge = db
        .create_edge(
            aletheiadb::core::NodeId::new(alice).unwrap(),
            aletheiadb::core::NodeId::new(bob).unwrap(),
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        )
        .expect("create edge")
        .as_u64();

    // Second version of each entity so history/diff have two versions.
    let server = mcp_server(&db);
    let _ = server.update_node(UpdateNodeRequest {
        namespace: None,
        node_id: alice,
        properties: [
            ("name".to_string(), json!("Alice")),
            ("age".to_string(), json!(31)),
            ("note".to_string(), json!(LONG_NOTE)),
        ]
        .into_iter()
        .collect(),
        valid_time: None,
        provenance: None,
        derived_from: None,
    });
    let _ = server.update_edge(UpdateEdgeRequest {
        namespace: None,
        edge_id: edge,
        properties: [("since".to_string(), json!(2021))].into_iter().collect(),
        valid_time: None,
        provenance: None,
        derived_from: None,
    });

    Fixture {
        db,
        store: make_store(),
        alice,
        bob,
        edge,
    }
}

/// Recursively drop the per-request `trace_id`, the wall-clock-volatile
/// bi-temporal timestamps, and the echoed volatile `transaction_time` so an
/// autumn response and a legacy reference are comparable.
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
                            | "snapshot_valid_time"
                            | "snapshot_transaction_time"
                            | "cursor"
                            | "cursor_ttl_seconds"
                    )
                })
                .map(|(k, val)| (k, normalize(val)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        other => other,
    }
}

fn len_of(v: &Value) -> usize {
    serde_json::to_string(v).expect("serialize").len()
}

async fn get(client: &TestClient, uri: &str, bearer: Option<&str>) -> (u16, Value) {
    let mut req = client.get(uri);
    if let Some(token) = bearer {
        req = req.header("authorization", &format!("Bearer {token}"));
    }
    let resp = req.send().await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).expect("new body is JSON");
    (status, body)
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

/// Fetch the two oldest `version_id`s of a node from its history (autumn).
async fn node_versions(client: &TestClient, id: u64, bearer: Option<&str>) -> (u64, u64) {
    let (status, body) = get(client, &format!("/nodes/{id}/history"), bearer).await;
    assert_eq!(status, 200, "history: {body}");
    let versions = body["versions"].as_array().expect("versions array");
    assert!(versions.len() >= 2, "need two versions: {body}");
    (
        versions[0]["version_id"].as_u64().expect("v0 id"),
        versions[1]["version_id"].as_u64().expect("v1 id"),
    )
}

/// Fetch the two oldest `version_id`s of an edge from its history (autumn).
async fn edge_versions(client: &TestClient, id: u64, bearer: Option<&str>) -> (u64, u64) {
    let (status, body) = get(client, &format!("/edges/{id}/history"), bearer).await;
    assert_eq!(status, 200, "history: {body}");
    let versions = body["versions"].as_array().expect("versions array");
    assert!(versions.len() >= 2, "need two versions: {body}");
    (
        versions[0]["version_id"].as_u64().expect("v0 id"),
        versions[1]["version_id"].as_u64().expect("v1 id"),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// Byte-parity for all twelve tools vs the legacy MCP surface, in both auth modes.
// ════════════════════════════════════════════════════════════════════════════

/// Drive every one of the twelve tools through the autumn surface and assert the
/// response equals the legacy MCP reference. `mode`/`token` parameterize the
/// auth mode so the same coverage runs anonymous and required.
async fn assert_all_twelve_parity(mode: AuthMode, token: Option<&'static str>) {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), mode);
    let server = mcp_server(&fx.db);
    let (nv0, nv1) = node_versions(&client, fx.alice, token).await;
    let (ev0, ev1) = edge_versions(&client, fx.edge, token).await;

    // 1. traverse (GET) — legacy typed method.
    let (s, autumn) = get(
        &client,
        &format!("/traverse?start_node_id={}&edge_label=KNOWS", fx.alice),
        token,
    )
    .await;
    assert_eq!(s, 200, "traverse: {autumn}");
    // `TraverseRequest` is `#[non_exhaustive]` (cannot be built out-of-crate), so
    // the legacy reference is the equivalent public `dispatch_tool_json` path —
    // which is exactly what the autumn handler forwards to.
    let legacy: Value = serde_json::from_str(&server.dispatch_tool_json(
        "traverse",
        json!({ "start_node_id": fx.alice, "edge_label": "KNOWS" }),
    ))
    .expect("legacy traverse json");
    assert_eq!(normalize(autumn), normalize(legacy), "traverse parity");

    // 2. get_node_history (GET) — legacy typed method.
    let (_s, autumn) = get(&client, &format!("/nodes/{}/history", fx.alice), token).await;
    let legacy: Value =
        serde_json::from_str(&server.get_node_history(GetNodeHistoryRequest { node_id: fx.alice }))
            .expect("legacy node history json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_node_history parity"
    );

    // 3. get_edge_history (GET) — legacy typed method.
    let (_s, autumn) = get(&client, &format!("/edges/{}/history", fx.edge), token).await;
    let legacy: Value =
        serde_json::from_str(&server.get_edge_history(GetEdgeHistoryRequest { edge_id: fx.edge }))
            .expect("legacy edge history json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_edge_history parity"
    );

    // 4. get_node_at_time (POST) — legacy typed method.
    let (_s, autumn) = post(
        &client,
        "/nodes/at_time",
        &json!({ "node_id": fx.alice, "valid_time": FUTURE_VT }),
        token,
    )
    .await;
    let legacy: Value = serde_json::from_str(&server.get_node_at_time(GetNodeAtTimeRequest {
        node_id: fx.alice,
        valid_time: FUTURE_VT.to_string(),
        transaction_time: None,
    }))
    .expect("legacy node at_time json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_node_at_time parity"
    );

    // 5. get_edge_at_time (POST) — legacy typed method.
    let (_s, autumn) = post(
        &client,
        "/edges/at_time",
        &json!({ "edge_id": fx.edge, "valid_time": FUTURE_VT }),
        token,
    )
    .await;
    let legacy: Value = serde_json::from_str(&server.get_edge_at_time(GetEdgeAtTimeRequest {
        edge_id: fx.edge,
        valid_time: FUTURE_VT.to_string(),
        transaction_time: None,
    }))
    .expect("legacy edge at_time json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_edge_at_time parity"
    );

    // 6. list_changes (POST) — legacy typed method.
    let changes_body = json!({
        "tx_from": "1970-01-01T00:00:00Z",
        "tx_to": FUTURE_VT,
    });
    let (_s, autumn) = post(&client, "/changes", &changes_body, token).await;
    let legacy: Value = serde_json::from_str(&server.list_changes(ListChangesRequest {
        tx_from: "1970-01-01T00:00:00Z".to_string(),
        tx_to: FUTURE_VT.to_string(),
        valid_from: None,
        valid_to: None,
        label: None,
        limit: None,
        cursor: None,
    }))
    .expect("legacy list_changes json");
    assert_eq!(normalize(autumn), normalize(legacy), "list_changes parity");

    // 7. get_node_at_valid_time (POST) — dispatch reference (no typed method).
    let body = json!({ "node_id": fx.alice, "valid_time": FUTURE_VT });
    let (_s, autumn) = post(&client, "/nodes/at_valid_time", &body, token).await;
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("get_node_at_valid_time", body.clone()))
            .expect("legacy node at_valid_time json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_node_at_valid_time parity"
    );

    // 8. get_node_at_transaction_time (POST) — dispatch reference.
    let body = json!({ "node_id": fx.alice, "transaction_time": FUTURE_VT });
    let (_s, autumn) = post(&client, "/nodes/at_transaction_time", &body, token).await;
    let legacy: Value = serde_json::from_str(
        &server.dispatch_tool_json("get_node_at_transaction_time", body.clone()),
    )
    .expect("legacy node at_transaction_time json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_node_at_transaction_time parity"
    );

    // 9. diff_node_versions (POST) — dispatch reference.
    let body = json!({ "node_id": fx.alice, "from_version": nv0, "to_version": nv1 });
    let (_s, autumn) = post(&client, "/nodes/diff", &body, token).await;
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("diff_node_versions", body.clone()))
            .expect("legacy diff_node json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "diff_node_versions parity"
    );

    // 10. get_edge_at_valid_time (POST) — dispatch reference.
    let body = json!({ "edge_id": fx.edge, "valid_time": FUTURE_VT });
    let (_s, autumn) = post(&client, "/edges/at_valid_time", &body, token).await;
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("get_edge_at_valid_time", body.clone()))
            .expect("legacy edge at_valid_time json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_edge_at_valid_time parity"
    );

    // 11. get_edge_at_transaction_time (POST) — dispatch reference.
    let body = json!({ "edge_id": fx.edge, "transaction_time": FUTURE_VT });
    let (_s, autumn) = post(&client, "/edges/at_transaction_time", &body, token).await;
    let legacy: Value = serde_json::from_str(
        &server.dispatch_tool_json("get_edge_at_transaction_time", body.clone()),
    )
    .expect("legacy edge at_transaction_time json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "get_edge_at_transaction_time parity"
    );

    // 12. diff_edge_versions (POST) — dispatch reference.
    let body = json!({ "edge_id": fx.edge, "from_version": ev0, "to_version": ev1 });
    let (_s, autumn) = post(&client, "/edges/diff", &body, token).await;
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("diff_edge_versions", body.clone()))
            .expect("legacy diff_edge json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "diff_edge_versions parity"
    );
}

#[tokio::test]
async fn all_twelve_byte_parity_required_mode() {
    assert_all_twelve_parity(AuthMode::Required, Some(READER_TOKEN)).await;
}

#[tokio::test]
async fn all_twelve_byte_parity_anonymous_mode() {
    assert_all_twelve_parity(AuthMode::Anonymous, None).await;
}

// ════════════════════════════════════════════════════════════════════════════
// traverse behavior-pinned assertions (#3225, #3232, #3220).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn traverse_behavior_pinned() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);

    let (status, body) = get(
        &client,
        &format!("/traverse?start_node_id={}&edge_label=KNOWS", fx.alice),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200, "traverse: {body}");

    let results = body["results"].as_array().expect("results array");
    // Alice --KNOWS--> Bob at depth 1: exactly Bob.
    let reached: BTreeSet<u64> = results
        .iter()
        .map(|r| r["node"]["id"].as_u64().expect("node id"))
        .collect();
    assert_eq!(reached, BTreeSet::from([fx.bob]), "traverse reaches Bob");
    assert_eq!(body["count"], 1);

    let bob = &results[0]["node"];
    // #3232: each traversal node carries the temporal block.
    assert!(
        bob["temporal"].is_object(),
        "traversal node has temporal block: {bob}"
    );
    // #3220: the embedding is elided by default on the traversal read path.
    assert_eq!(
        bob["properties"]["embedding"]["elided"], true,
        "embedding elided by default: {bob}"
    );

    // include_vectors=true returns the raw array.
    let (_s, raw) = get(
        &client,
        &format!(
            "/traverse?start_node_id={}&edge_label=KNOWS&include_vectors=true",
            fx.alice
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert!(
        raw["results"][0]["node"]["properties"]["embedding"].is_array(),
        "include_vectors=true returns the raw array: {raw}"
    );
}

#[tokio::test]
async fn traverse_unauthenticated_401() {
    let fx = fixture();
    let client = build_server_client(fx.db, fx.store, AuthMode::Required);
    let resp = client
        .get(&format!(
            "/traverse?start_node_id={}&edge_label=KNOWS",
            fx.alice
        ))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
}

// ════════════════════════════════════════════════════════════════════════════
// #3353 token budget on the two budgetable reads (traverse, get_node_history).
// The budget rides the RAW arguments and is applied in `dispatch_tool`; the
// handlers forward through `dispatch_tool_json`, so the same public entry is the
// legacy reference.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn traverse_token_budget_shapes_response_and_matches_legacy() {
    // Alice --KNOWS--> three targets, each with a bulky note, so the full
    // traversal comfortably exceeds a modest byte budget and the #3353 ladder
    // must shape it down.
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let big_note = LONG_NOTE.repeat(3);
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .expect("alice")
        .as_u64();
    for name in ["Bob", "Carol", "Dave"] {
        let t = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("n", name)
                    .insert("note", big_note.as_str())
                    .build(),
            )
            .expect("target");
        db.create_edge(
            aletheiadb::core::NodeId::new(alice).unwrap(),
            t,
            "KNOWS",
            PropertyMapBuilder::new().build(),
        )
        .expect("edge");
    }
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);
    let server = mcp_server(&db);

    // Full (no budget): no `budget` block, exceeds the cap.
    let base = format!("/traverse?start_node_id={alice}&edge_label=KNOWS");
    let (status, full) = get(&client, &base, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(full.get("budget").is_none(), "no budget block: {full}");
    let cap: u64 = 2000;
    assert!(
        len_of(&full) as u64 > cap,
        "full response should exceed the cap: {}",
        len_of(&full)
    );

    // Legacy dispatch is the reference; its emitted string is the exact bytes the
    // #3353 contract bounds.
    let legacy_text = server.dispatch_tool_json(
        "traverse",
        json!({ "start_node_id": alice, "edge_label": "KNOWS", "max_response_bytes": cap }),
    );
    assert!(
        legacy_text.len() as u64 <= cap,
        "emitted string must fit the byte budget: {} > {cap}",
        legacy_text.len()
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert!(
        legacy["budget"].is_object(),
        "budget block discloses the applied rung: {legacy}"
    );

    // Autumn with the same budget must match the legacy dispatch AND be shaped.
    let (status, budgeted) = get(
        &client,
        &format!("{base}&max_response_bytes={cap}"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(budgeted["budget"].is_object(), "shaped: {budgeted}");
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped response must be smaller than full: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn traverse must equal the legacy dispatch for the same budget"
    );
}

#[tokio::test]
async fn get_node_history_token_budget_shapes_response_and_matches_legacy() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);

    let base = format!("/nodes/{}/history", fx.alice);
    let (status, full) = get(&client, &base, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(full.get("budget").is_none(), "no budget block: {full}");
    let cap: u64 = 900;
    assert!(
        len_of(&full) as u64 > cap,
        "full response should exceed the cap: {}",
        len_of(&full)
    );

    let legacy_text = server.dispatch_tool_json(
        "get_node_history",
        json!({ "node_id": fx.alice, "max_response_bytes": cap }),
    );
    assert!(
        legacy_text.len() as u64 <= cap,
        "emitted string must fit the byte budget: {} > {cap}",
        legacy_text.len()
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert!(legacy["budget"].is_object(), "budget block: {legacy}");

    let (status, budgeted) = get(
        &client,
        &format!("{base}?max_response_bytes={cap}"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        budgeted["budget"].is_object(),
        "budget MUST be honored (shaped), not dropped: {budgeted}"
    );
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped response must be smaller: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn get_node_history must equal the legacy dispatch"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// #3360 snapshot-anchored cursor paging on traverse. `use_cursor:true` opens a
// snapshot-anchored scan returning an opaque cursor + first page; passing the
// cursor back (alone) continues it, duplicate-free and gap-free, until the union
// equals the full result. The cursor secret lives on the process-lifetime shared
// server in `ServerState`, so a token issued on one request resumes on the next.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn traverse_cursor_paging_snapshot_anchored() {
    // A star: Alice --KNOWS--> {Bob, Carol, Dave}. Per-page size of 1 forces
    // continuation across three snapshot-anchored pages.
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .expect("alice")
        .as_u64();
    let mut expected: BTreeSet<u64> = BTreeSet::new();
    for name in ["Bob", "Carol", "Dave"] {
        let t = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", name).build(),
            )
            .expect("target");
        db.create_edge(
            aletheiadb::core::NodeId::new(alice).unwrap(),
            t,
            "KNOWS",
            PropertyMapBuilder::new().build(),
        )
        .expect("edge");
        expected.insert(t.as_u64());
    }
    let client = build_server_client(db, make_store(), AuthMode::Required);

    let (status, page1) = get(
        &client,
        &format!("/traverse?start_node_id={alice}&edge_label=KNOWS&use_cursor=true&limit=1"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200, "page1: {page1}");
    assert_eq!(
        page1["paging"], "cursor",
        "cursor paging disclosed: {page1}"
    );
    assert!(
        page1["snapshot_valid_time"].is_string(),
        "snapshot disclosed: {page1}"
    );
    assert!(
        page1["cursor"].as_str().is_some(),
        "first page returns a continuation token: {page1}"
    );

    let mut seen: Vec<u64> = Vec::new();
    let mut page = page1.clone();
    let mut prev_snapshot = page1["snapshot_valid_time"].clone();
    let mut guard = 0;
    loop {
        for r in page["results"].as_array().expect("results array") {
            seen.push(r["node"]["id"].as_u64().expect("node id"));
        }
        let Some(token) = page["cursor"].as_str() else {
            break;
        };
        // Continuation: the token carries all discriminating state; passed alone.
        let token = token.to_string();
        let (status, next) = get(
            &client,
            &format!("/traverse?cursor={token}"),
            Some(READER_TOKEN),
        )
        .await;
        assert_eq!(status, 200, "continuation page: {next}");
        assert_eq!(
            next["snapshot_valid_time"], prev_snapshot,
            "continuation stays on the pinned snapshot: {next}"
        );
        prev_snapshot = next["snapshot_valid_time"].clone();
        page = next;
        guard += 1;
        assert!(guard < 10, "cursor scan did not terminate");
    }

    let seen_set: BTreeSet<u64> = seen.iter().copied().collect();
    assert_eq!(seen.len(), seen_set.len(), "no duplicates across pages");
    assert_eq!(
        seen_set, expected,
        "union of pages equals the full target set"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// #3232 temporal block presence on the point-in-time single-entity reads.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn temporal_block_present_on_point_in_time_reads() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);

    // get_node_at_time returns the node under a `node` key with a temporal block.
    let (_s, node_at) = post(
        &client,
        "/nodes/at_time",
        &json!({ "node_id": fx.alice, "valid_time": FUTURE_VT }),
        Some(READER_TOKEN),
    )
    .await;
    assert!(
        node_at["node"]["temporal"].is_object(),
        "get_node_at_time carries a temporal block: {node_at}"
    );

    let (_s, edge_at) = post(
        &client,
        "/edges/at_time",
        &json!({ "edge_id": fx.edge, "valid_time": FUTURE_VT }),
        Some(READER_TOKEN),
    )
    .await;
    assert!(
        edge_at["edge"]["temporal"].is_object(),
        "get_edge_at_time carries a temporal block: {edge_at}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Conformance: the two dispatch-routed budgetable reads are pinned in the shared
// DISPATCH_ROUTED_READ_TOOLS table with Read class (extends the coverage the
// `dispatch_pinned_names_match_routed_class` test in server_parity.rs asserts).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dispatch_routed_table_includes_traverse_and_get_node_history() {
    use aletheia_server::edge_tools::DISPATCH_ROUTED_READ_TOOLS;
    for name in ["traverse", "get_node_history"] {
        let entry = DISPATCH_ROUTED_READ_TOOLS
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} must be pinned in DISPATCH_ROUTED_READ_TOOLS"));
        assert_eq!(entry.1, AccessClass::Read, "{name} pinned as Read");
    }
}

// ════════════════════════════════════════════════════════════════════════════
// OpenAPI schema fidelity: a local-body dispatch tool must expose a named,
// per-operation request schema in /openapi.json — never the generic `Value`.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn openapi_diff_node_versions_has_typed_request_schema() {
    let fx = fixture();
    let client = build_server_client(fx.db, fx.store, AuthMode::Required);
    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let spec: Value = serde_json::from_str(&resp.text()).expect("openapi json");

    let schema = &spec["paths"]["/nodes/diff"]["post"]["requestBody"]["content"]["application/json"]
        ["schema"];
    let sref = schema["$ref"].as_str().unwrap_or_else(|| {
        panic!(
            "/nodes/diff requestBody schema must be a $ref: {}",
            spec["paths"]["/nodes/diff"]
        )
    });
    assert!(
        sref.ends_with("/DiffNodeVersionsBody"),
        "diff request body must ref a typed per-operation schema, got {sref}"
    );
    assert_ne!(
        sref, "#/components/schemas/Value",
        "must not degrade to the shared generic Value component"
    );
    // traverse (GET) documents its start_node_id query parameter.
    let op = &spec["paths"]["/traverse"]["get"];
    assert!(!op.is_null(), "GET /traverse documented: {}", spec["paths"]);
}
