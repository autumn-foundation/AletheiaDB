//! Budget-shaping + cursor-paging conformance for the three dispatch-routed node
//! reads (`get_node`, `list_nodes`, `find_nodes_at_time`) — the NODE-READS
//! BUDGET/CURSOR RETROFIT (Issue #3524 follow-up).
//!
//! PR2 forwarded these three reads through the main crate's **typed** per-tool
//! methods (`AletheiaMcpServer::get_node`/`list_nodes`/`find_nodes_at_time`),
//! which bypass `dispatch_tool` — so the Issue #3353 token budget
//! (`max_response_tokens`/`max_response_bytes`/`priority_properties`) and the
//! Issue #3360 cursor paging (`use_cursor`/`cursor`) were **silently dropped**.
//! The retrofit routes them through `AletheiaMcpServer::dispatch_tool_json` with
//! a hardcoded literal tool name via the process-lifetime shared
//! `ServerState::mcp_server()` (so the cursor secret persists across requests),
//! exactly like the four budgetable edge reads (`tests/edge_parity.rs`).
//!
//! Each test drives the autumn [`TestClient`] and asserts the response equals
//! the **legacy dispatch** (`AletheiaMcpServer::dispatch_tool_json`) byte-for-byte
//! for the same budget, that omitting the budget/cursor reproduces the full
//! output, and — the **parity blind spot** these tests close — that a small
//! budget actually SHAPES the node-read response (a `budget` block appears and
//! the response shrinks) rather than being ignored as the typed forwarding did.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::mcp::AletheiaMcpServer;
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const EMBEDDING: &[f32] = &[0.10, 0.20, 0.30, 0.40];

/// A long property value so a single node / a small fan-out comfortably exceeds
/// a modest byte budget, forcing the #3353 ladder to shape the response down.
const LONG_NOTE: &str = "a deliberately long note property, repeated so the full \
     serialized response comfortably exceeds a moderate byte budget and the #3353 \
     ladder must shape it down: lorem ipsum dolor sit amet consectetur adipiscing \
     elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua ut enim \
     ad minim veniam quis nostrud exercitation ullamco laboris nisi ut aliquip ex";

fn make_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("insert reader key");
    store
}

fn fresh_db() -> Arc<AletheiaDB> {
    Arc::new(AletheiaDB::new().expect("create db"))
}

fn mcp_server(db: &Arc<AletheiaDB>) -> AletheiaMcpServer {
    AletheiaMcpServer::new(db.clone())
}

/// Recursively drop the per-request `trace_id`, the wall-clock-volatile
/// bi-temporal timestamps, and the `find_nodes_at_time`-echoed
/// `transaction_time` so an autumn response and a legacy dispatch are comparable.
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

/// Seed `count` `Person{idx, note:<long>}` nodes, returning their ids (ascending,
/// deterministic on a fresh db). The long `note` makes the full response bulky.
fn seed_people(db: &Arc<AletheiaDB>, count: usize) -> Vec<NodeId> {
    let mut ids = Vec::new();
    for i in 0..count {
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("idx", i as i64)
                    .insert("note", LONG_NOTE)
                    .build(),
            )
            .expect("seed person");
        ids.push(id);
    }
    ids
}

fn len_of(v: &Value) -> usize {
    serde_json::to_string(v).expect("serialize").len()
}

/// Seed a single `Person` whose full read comfortably exceeds the #3353
/// single-entity minimal-rung floor (~852 bytes), so a budget above that floor
/// but below the full size forces the ladder to shape (elide the bulky note).
fn seed_bulky_person(db: &Arc<AletheiaDB>) -> NodeId {
    let big_note = LONG_NOTE.repeat(6); // ~2.4 KiB, well above the floor
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("note", big_note.as_str())
            .insert_vector("embedding", EMBEDDING)
            .build(),
    )
    .expect("seed bulky person")
}

// ════════════════════════════════════════════════════════════════════════════
// #3353 token budget on the three node reads. The budget rides the RAW arguments
// and is applied in `AletheiaMcpServer::dispatch_tool` (the typed per-tool
// methods bypass it), so the handlers forward through `dispatch_tool_json`. The
// tests use that same public entry as the legacy reference.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_nodes_token_budget_shapes_response_and_matches_legacy() {
    let db = fresh_db();
    seed_people(&db, 6);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // `list_nodes` enumerates only under a `label` filter (an unlabeled list is a
    // stub `count:0` message), so the whole budget path is exercised with
    // `label=Person`. Full (no budget): larger than the cap, no `budget` block.
    let (status, full) = get(&client, "/nodes?label=Person", Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(full.get("budget").is_none(), "no budget block: {full}");
    assert_eq!(full["count"], 6);
    let cap: u64 = 2000;
    assert!(
        len_of(&full) as u64 > cap,
        "full response should exceed the cap: {}",
        len_of(&full)
    );

    // Legacy dispatch is the reference; its emitted string is the exact bytes the
    // #3353 contract bounds.
    let budget_args = json!({ "label": "Person", "max_response_bytes": cap });
    let legacy_text = mcp_server(&db).dispatch_tool_json("list_nodes", budget_args);
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

    // Autumn surface with the same budget must match the legacy dispatch.
    let (status, budgeted) = get(
        &client,
        &format!("/nodes?label=Person&max_response_bytes={cap}"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(budgeted["budget"].is_object(), "shaped: {budgeted}");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn list_nodes must equal the legacy dispatch for the same budget"
    );
}

#[tokio::test]
async fn get_node_token_budget_parity() {
    // The single-entity budgetable read forwards the budget: a byte budget above
    // the minimal-rung floor but below the full size yields the same shaped
    // result as the legacy dispatch.
    let db = fresh_db();
    let id = seed_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let cap: u64 = 2000;
    let (status, budgeted) = get(
        &client,
        &format!("/nodes/{}?max_response_bytes={cap}", id.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(budgeted["budget"].is_object(), "shaped: {budgeted}");

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_node",
        json!({ "node_id": id.as_u64(), "max_response_bytes": cap }),
    );
    assert!(
        legacy_text.len() as u64 <= cap,
        "emitted string must fit the byte budget: {} > {cap}",
        legacy_text.len()
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "get_node budgeted autumn response must equal the legacy dispatch"
    );
}

#[tokio::test]
async fn find_nodes_at_time_token_budget_shapes_response_and_matches_legacy() {
    let db = fresh_db();
    seed_people(&db, 6);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let vt = "2999-01-01T00:00:00Z"; // after creation → resolves all seeded nodes
    let base = json!({ "label": "Person", "valid_time": vt });

    // Full (no budget): no `budget` block, exceeds the cap.
    let (status, full) = post(&client, "/nodes/find_at_time", &base, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(full.get("budget").is_none(), "no budget block: {full}");
    assert_eq!(full["count"], 6);
    let cap: u64 = 2000;
    assert!(
        len_of(&full) as u64 > cap,
        "full response should exceed the cap: {}",
        len_of(&full)
    );

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "find_nodes_at_time",
        json!({ "label": "Person", "valid_time": vt, "max_response_bytes": cap }),
    );
    assert!(
        legacy_text.len() as u64 <= cap,
        "emitted string must fit the byte budget: {} > {cap}",
        legacy_text.len()
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert!(legacy["budget"].is_object(), "budget block: {legacy}");

    let mut budgeted_req = base.clone();
    budgeted_req["max_response_bytes"] = json!(cap);
    let (status, budgeted) = post(
        &client,
        "/nodes/find_at_time",
        &budgeted_req,
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(budgeted["budget"].is_object(), "shaped: {budgeted}");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn find_nodes_at_time must equal the legacy dispatch"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// PARITY BLIND SPOT: the node reads must HONOR a budget, not merely default.
//
// The original gap was invisible to a pure default-output parity test: typed
// forwarding produced byte-identical DEFAULT output, so parity passed while the
// budget was silently dropped. These tests assert the *shaped* behavior — a
// `budget` block appears AND the response shrinks under a small budget — which is
// exactly what typed forwarding could NOT do and what would have caught the gap.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_nodes_budget_is_honored_not_silently_dropped() {
    let db = fresh_db();
    seed_people(&db, 6);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (_s, full) = get(&client, "/nodes?label=Person", Some(READER_TOKEN)).await;
    let (_s, budgeted) = get(
        &client,
        "/nodes?label=Person&max_response_bytes=2000",
        Some(READER_TOKEN),
    )
    .await;

    // Pre-retrofit (typed forwarding) these two would be byte-identical with no
    // `budget` block — the silent-drop bug. Post-retrofit the budgeted response
    // is shaped: it carries a `budget` block and is strictly smaller.
    assert!(
        full.get("budget").is_none(),
        "unbudgeted response must not be shaped: {full}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "budget MUST be honored (shaped), not dropped: {budgeted}"
    );
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped response must be smaller than the full one: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
    assert!(
        len_of(&budgeted) <= 2000,
        "shaped response must fit the byte budget: {}",
        len_of(&budgeted)
    );
}

#[tokio::test]
async fn find_nodes_at_time_budget_is_honored_not_silently_dropped() {
    let db = fresh_db();
    seed_people(&db, 6);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let vt = "2999-01-01T00:00:00Z";
    let (_s, full) = post(
        &client,
        "/nodes/find_at_time",
        &json!({ "label": "Person", "valid_time": vt }),
        Some(READER_TOKEN),
    )
    .await;
    let (_s, budgeted) = post(
        &client,
        "/nodes/find_at_time",
        &json!({ "label": "Person", "valid_time": vt, "max_response_bytes": 2000 }),
        Some(READER_TOKEN),
    )
    .await;

    assert!(
        full.get("budget").is_none(),
        "unbudgeted not shaped: {full}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "budget MUST be honored, not dropped: {budgeted}"
    );
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped response must be smaller: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
}

#[tokio::test]
async fn get_node_budget_is_honored_not_silently_dropped() {
    let db = fresh_db();
    let id = seed_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (_s, full) = get(
        &client,
        &format!("/nodes/{}", id.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    let (_s, budgeted) = get(
        &client,
        &format!("/nodes/{}?max_response_bytes=2000", id.as_u64()),
        Some(READER_TOKEN),
    )
    .await;

    assert!(
        full.get("budget").is_none(),
        "unbudgeted not shaped: {full}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "budget MUST be honored on the single-entity read, not dropped: {budgeted}"
    );
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped single-entity response must be smaller: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
}

// ════════════════════════════════════════════════════════════════════════════
// #3360 cursor paging on `list_nodes` and `find_nodes_at_time`. `use_cursor:true`
// opens a snapshot-anchored scan returning an opaque cursor + first page; passing
// the cursor back (alone) continues it, duplicate-free and gap-free, until the
// union equals the full result. The cursor secret lives on the process-lifetime
// shared server in `ServerState`, so a token issued on one request resumes on
// the next — the scan is driven autumn-only (a separate `AletheiaMcpServer`
// instance signs with a different secret and could not decode the token).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_nodes_cursor_paging_snapshot_anchored() {
    let db = fresh_db();
    let ids = seed_people(&db, 3);
    let expected: std::collections::BTreeSet<u64> = ids.iter().map(|n| n.as_u64()).collect();
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // First page: cursor paging requires `label` (an enumerable candidate set);
    // per-page size of 1 forces continuation.
    let (status, page1) = get(
        &client,
        "/nodes?label=Person&use_cursor=true&limit=1",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
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
        for n in page["nodes"].as_array().expect("nodes array") {
            seen.push(n["id"].as_u64().expect("node id"));
        }
        let Some(token) = page["cursor"].as_str() else {
            break;
        };
        // Continuation: the token carries all discriminating state; it is passed
        // back alone. The base64url no-pad token is URL-safe.
        let token = token.to_string();
        let (status, next) = get(
            &client,
            &format!("/nodes?cursor={token}"),
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

    let seen_set: std::collections::BTreeSet<u64> = seen.iter().copied().collect();
    assert_eq!(seen.len(), seen_set.len(), "no duplicates across pages");
    assert_eq!(
        seen_set, expected,
        "union of pages equals the full node set"
    );
}

#[tokio::test]
async fn find_nodes_at_time_cursor_paging_snapshot_anchored() {
    let db = fresh_db();
    let ids = seed_people(&db, 3);
    let expected: std::collections::BTreeSet<u64> = ids.iter().map(|n| n.as_u64()).collect();
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let vt = "2999-01-01T00:00:00Z";
    let (status, page1) = post(
        &client,
        "/nodes/find_at_time",
        &json!({ "label": "Person", "valid_time": vt, "use_cursor": true, "limit": 1 }),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        page1["paging"], "cursor",
        "cursor paging disclosed: {page1}"
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
        for n in page["nodes"].as_array().expect("nodes array") {
            seen.push(n["id"].as_u64().expect("node id"));
        }
        let Some(token) = page["cursor"].as_str() else {
            break;
        };
        // #3360: the continuation body carries ONLY the cursor (no label /
        // valid_time) — which is exactly why the handler forwards a raw JSON body
        // rather than a typed request (those fields are otherwise required).
        let (status, next) = post(
            &client,
            "/nodes/find_at_time",
            &json!({ "cursor": token }),
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

    let seen_set: std::collections::BTreeSet<u64> = seen.iter().copied().collect();
    assert_eq!(seen.len(), seen_set.len(), "no duplicates across pages");
    assert_eq!(
        seen_set, expected,
        "union of pages equals the full node set"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Default (no budget/cursor) parity: the retrofit must not change today's output.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn node_reads_default_output_unchanged() {
    let db = fresh_db();
    seed_people(&db, 2);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // get_node
    let (_s, get_body) = get(&client, "/nodes/1", Some(READER_TOKEN)).await;
    let legacy_get: Value = serde_json::from_str(
        &mcp_server(&db).dispatch_tool_json("get_node", json!({ "node_id": 1 })),
    )
    .expect("legacy get_node json");
    assert_eq!(
        normalize(get_body),
        normalize(legacy_get),
        "default get_node output must be unchanged"
    );

    // list_nodes
    let (_s, list_body) = get(&client, "/nodes", Some(READER_TOKEN)).await;
    let legacy_list: Value =
        serde_json::from_str(&mcp_server(&db).dispatch_tool_json("list_nodes", json!({})))
            .expect("legacy list_nodes json");
    assert_eq!(
        normalize(list_body),
        normalize(legacy_list),
        "default list_nodes output must be unchanged"
    );

    // find_nodes_at_time
    let vt = "2999-01-01T00:00:00Z";
    let (_s, find_body) = post(
        &client,
        "/nodes/find_at_time",
        &json!({ "label": "Person", "valid_time": vt }),
        Some(READER_TOKEN),
    )
    .await;
    let legacy_find: Value = serde_json::from_str(&mcp_server(&db).dispatch_tool_json(
        "find_nodes_at_time",
        json!({ "label": "Person", "valid_time": vt }),
    ))
    .expect("legacy find json");
    assert_eq!(
        normalize(find_body),
        normalize(legacy_find),
        "default find_nodes_at_time output must be unchanged"
    );
}
