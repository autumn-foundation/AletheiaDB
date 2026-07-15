//! `priority_properties` on the budgetable **GET** reads must be a functional,
//! comma-separated query parameter (bug fix: `fix/priority-properties-get-param`).
//!
//! axum/autumn `Query` deserialization runs through `serde_urlencoded`, which
//! CANNOT deserialize a `Vec<T>` from any URL encoding — so a GET read declaring
//! `priority_properties: Option<Vec<String>>` and receiving
//! `?priority_properties=name,title` returned **HTTP 400**: the #3353
//! prioritization param was latent-dead on every GET surface (only the POST-body
//! reads — `find_nodes_at_time`/`find_similar`/`hybrid_query` — worked, because
//! they deserialize from a JSON body, not the query string).
//!
//! The fix keeps the field type `Option<Vec<String>>` (so the shared
//! `insert_budget` helper and the `src/mcp/budget.rs` contract are untouched) and
//! applies ONE shared custom `deserialize_with` that reads a single query String
//! and splits it on `,` (trimming, dropping empties). These tests assert the
//! param is now honored end-to-end on representative GET routes across the three
//! affected handler modules (`node_tools`, `traverse_temporal_tools`) plus the
//! `get_node` single-entity read, matching the **legacy dispatch**
//! (`AletheiaMcpServer::dispatch_tool_json` with a `priority_properties` array)
//! byte-for-byte and — the observable behavior — protecting the named properties
//! from the #3353 elision ladder while an unnamed bulky property is elided.

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

/// A long property value so a node with a few of these comfortably exceeds a
/// modest byte budget, forcing the #3353 ladder to shape the response down.
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

/// Recursively drop the per-request `trace_id` and the wall-clock-volatile
/// bi-temporal timestamps so an autumn response and a legacy dispatch compare.
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
    let body: Value = serde_json::from_str(&resp.text()).expect("body is JSON");
    (status, body)
}

/// Seed a single `Person` with THREE bulky string properties (`note`, `bio`,
/// `memo`) plus a small `name` and a vector, so that at an intermediate budget
/// the #3353 elision rung must drop the *unprotected* bulky props while the
/// `priority_properties`-named ones survive — making prioritization observable.
fn seed_three_bulky_person(db: &Arc<AletheiaDB>) -> NodeId {
    let big = LONG_NOTE.repeat(6); // ~2.4 KiB each; three of them ~7.3 KiB total
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("note", big.as_str())
            .insert("bio", big.as_str())
            .insert("memo", big.as_str())
            .insert_vector("embedding", EMBEDDING)
            .build(),
    )
    .expect("seed three-bulky person")
}

/// Locate the reconstructed `properties` object of a single-entity read body.
fn props(body: &Value) -> &Value {
    &body["properties"]
}

fn is_elided(v: &Value) -> bool {
    v.get("elided").and_then(Value::as_bool).unwrap_or(false)
}

// A budget that sits ABOVE the size of the response after eliding the single
// unprotected bulky prop (`memo` ~2.4 KiB → tiny descriptor) but BELOW the full
// ~7.3 KiB, so the ladder stops at the `elided_properties` rung with `note` and
// `bio` (protected) intact.
const CAP: u64 = 5500;

// ════════════════════════════════════════════════════════════════════════════
// (a) get_node: prioritized props survive, an unprotected bulky prop is elided,
//     and the GET response matches the legacy dispatch with a priority array.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_node_priority_properties_get_param_prioritizes_and_matches_legacy() {
    let db = fresh_db();
    let id = seed_three_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // Two protected props, comma-separated on the query string.
    let (status, budgeted) = get(
        &client,
        &format!(
            "/nodes/{}?max_response_bytes={CAP}&priority_properties=note,bio",
            id.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "GET with comma-separated priority_properties: {budgeted}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "response is shaped: {budgeted}"
    );

    // The two protected props survive in full; the unprotected bulky one elides.
    let p = props(&budgeted);
    assert!(
        p["note"].is_string(),
        "protected `note` must survive in full: {p}"
    );
    assert!(
        p["bio"].is_string(),
        "protected `bio` must survive in full: {p}"
    );
    assert!(
        is_elided(&p["memo"]),
        "unprotected bulky `memo` must be elided: {p}"
    );

    // Parity: the GET surface equals the legacy dispatch given the array form.
    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_node",
        json!({
            "node_id": id.as_u64(),
            "max_response_bytes": CAP,
            "priority_properties": ["note", "bio"],
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "GET priority_properties=note,bio must equal legacy priority_properties:[note,bio]"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (c) Malformed all-empty input `?priority_properties=,,` → 200, behaves as if
//     the param were absent (no 400, no protection).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_node_priority_properties_all_empty_behaves_like_absent() {
    let db = fresh_db();
    let id = seed_three_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, budgeted) = get(
        &client,
        &format!(
            "/nodes/{}?max_response_bytes={CAP}&priority_properties=,,",
            id.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "all-empty comma input must not 400: {budgeted}"
    );

    // Equivalent to sending NO priority param at all.
    let (status_absent, absent) = get(
        &client,
        &format!("/nodes/{}?max_response_bytes={CAP}", id.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status_absent, 200);
    assert_eq!(
        normalize(budgeted),
        normalize(absent.clone()),
        "`priority_properties=,,` must behave exactly like the param being absent"
    );

    // And equal to the legacy dispatch with NO priority_properties key.
    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_node",
        json!({ "node_id": id.as_u64(), "max_response_bytes": CAP }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(absent),
        normalize(legacy),
        "no-priority GET must equal legacy dispatch without the key"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (d) Whitespace around entries is trimmed; both named props are protected.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_node_priority_properties_whitespace_trimmed() {
    let db = fresh_db();
    let id = seed_three_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // `%20` is an encoded space: ` note ,bio` → trimmed to `note`,`bio`.
    let (status, budgeted) = get(
        &client,
        &format!(
            "/nodes/{}?max_response_bytes={CAP}&priority_properties=%20note%20,bio",
            id.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "whitespace-padded entries must not 400: {budgeted}"
    );

    let p = props(&budgeted);
    assert!(p["note"].is_string(), "trimmed `note` protected: {p}");
    assert!(p["bio"].is_string(), "trimmed `bio` protected: {p}");
    assert!(is_elided(&p["memo"]), "unprotected `memo` elided: {p}");

    // Parity with the trimmed array form.
    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_node",
        json!({
            "node_id": id.as_u64(),
            "max_response_bytes": CAP,
            "priority_properties": ["note", "bio"],
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "trimmed GET priority_properties must equal the clean array form"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (e1) list_nodes (node_tools::ListNodesQuery) honors the GET param crate-wide.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_nodes_priority_properties_get_param_parity() {
    let db = fresh_db();
    // A handful of bulky people so the list exceeds the budget and shapes.
    for _ in 0..4 {
        seed_three_bulky_person(&db);
    }
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let cap: u64 = 6000;
    let (status, budgeted) = get(
        &client,
        &format!("/nodes?label=Person&max_response_bytes={cap}&priority_properties=note,bio"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "list_nodes GET priority_properties must not 400: {budgeted}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "list_nodes shaped: {budgeted}"
    );

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "list_nodes",
        json!({
            "label": "Person",
            "max_response_bytes": cap,
            "priority_properties": ["note", "bio"],
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "list_nodes GET priority_properties=note,bio must equal the legacy array form"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (e2) traverse (traverse_temporal_tools::TraverseQuery) — a different handler
//      module — also honors the GET param, proving the fix is crate-wide.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn traverse_priority_properties_get_param_parity() {
    let db = fresh_db();
    let start = seed_three_bulky_person(&db);
    let other = seed_three_bulky_person(&db);
    db.create_edge(start, other, "KNOWS", PropertyMapBuilder::new().build())
        .expect("create edge");
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let cap: u64 = 6000;
    let (status, budgeted) = get(
        &client,
        &format!(
            "/traverse?start_node_id={}&edge_label=KNOWS&depth=1&max_response_bytes={cap}&priority_properties=note,bio",
            start.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "traverse GET priority_properties must not 400: {budgeted}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "traverse shaped: {budgeted}"
    );

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "traverse",
        json!({
            "start_node_id": start.as_u64(),
            "edge_label": "KNOWS",
            "depth": 1,
            "max_response_bytes": cap,
            "priority_properties": ["note", "bio"],
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "traverse GET priority_properties=note,bio must equal the legacy array form"
    );
}
