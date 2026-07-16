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
use aletheiadb::core::{EdgeId, NodeId};
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

/// Seed `Alice --KNOWS--> Bob` where the **edge** carries the same THREE bulky
/// string properties (`note`, `bio`, `memo`), so the edge reads (`get_edge`,
/// `get_outgoing_edges`) can demonstrate #3353 elision + prioritization exactly
/// as the node reads do. Returns `(source_node, edge)`.
fn seed_bulky_edge(db: &Arc<AletheiaDB>) -> (NodeId, EdgeId) {
    let big = LONG_NOTE.repeat(6); // ~2.4 KiB each; three ~7.3 KiB total
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("seed alice");
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .expect("seed bob");
    let edge = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new()
                .insert("note", big.as_str())
                .insert("bio", big.as_str())
                .insert("memo", big.as_str())
                .build(),
        )
        .expect("seed bulky edge");
    (alice, edge)
}

/// Locate the reconstructed `properties` object of a single-entity read body.
fn props(body: &Value) -> &Value {
    &body["properties"]
}

fn is_elided(v: &Value) -> bool {
    v.get("elided").and_then(Value::as_bool).unwrap_or(false)
}

/// A property value did NOT survive in full. Depending on which #3353 rung the
/// cap lands on (which varies with a response's list/version wrapper overhead), an
/// unprotected bulky property is either replaced inline by an `{elided: true}`
/// descriptor (`elided_properties` rung) OR swept out of the object entirely into
/// the `entity_summaries` rung's `_budget_omitted` marker (so the key is absent).
/// Both outcomes honor the prioritization guarantee: a value that is no longer a
/// full string did not survive. (`priority_properties` protects the named keys so
/// they DO survive, which is asserted separately.)
fn did_not_survive(v: &Value) -> bool {
    !v.is_string()
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

// ════════════════════════════════════════════════════════════════════════════
// (f) #3583 DoS cap engages on the GET path. A `priority_properties` comma list
//     LONGER than `max_priority_properties` (default 1024) — sent WITH a budget
//     param (the #3583 cap check in `parse_budget` only runs when
//     `max_response_tokens`/`max_response_bytes` is present; it early-returns
//     `Ok(None)` otherwise) — is rejected with the #3234 structured
//     INVALID_ARGUMENT envelope naming the cap, byte-for-byte identical to the
//     legacy dispatch given the same over-cap array. This proves the DoS bound
//     is NOT bypassed by the comma-splitting GET surface.
// ════════════════════════════════════════════════════════════════════════════

/// `DEFAULT_MAX_PRIORITY_PROPERTIES` in `src/mcp/budget.rs` (the shared server is
/// built with `AletheiaMcpServer::new`, which uses this default). One more than
/// this trips the #3583 over-cap rejection.
const DEFAULT_MAX_PRIORITY_PROPERTIES: usize = 1024;
const OVER_CAP_COUNT: usize = DEFAULT_MAX_PRIORITY_PROPERTIES + 77; // 1101

#[tokio::test]
async fn get_node_priority_properties_over_cap_rejected_on_get_path() {
    let db = fresh_db();
    let id = seed_three_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // p0,p1,...,p1100 — 1101 non-empty entries, comfortably over the 1024 cap.
    let names: Vec<String> = (0..OVER_CAP_COUNT).map(|i| format!("p{i}")).collect();
    let csv = names.join(",");

    // A budget param MUST be present for the cap check to run (parse_budget
    // early-returns Ok(None) with no budget), so include max_response_bytes.
    let (status, body) = get(
        &client,
        &format!(
            "/nodes/{}?max_response_bytes={CAP}&priority_properties={csv}",
            id.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;

    // The handler returns `Json<Value>`, so an MCP error is an in-BODY #3234
    // envelope at HTTP 200 (not an HTTP error status).
    assert_eq!(
        status, 200,
        "the over-cap rejection is an in-body error envelope at 200: {body}"
    );
    assert_eq!(
        body["error"]["code"], "INVALID_ARGUMENT",
        "over-cap priority_properties must be INVALID_ARGUMENT: {body}"
    );
    assert_eq!(
        body["error"]["retriable"],
        json!(false),
        "caller-fault error is non-retriable: {body}"
    );
    // details names the cap and the given length (#3583 defense-in-depth).
    assert_eq!(
        body["error"]["details"]["max_priority_properties"],
        json!(DEFAULT_MAX_PRIORITY_PROPERTIES),
        "details must name the cap: {body}"
    );
    assert_eq!(
        body["error"]["details"]["given"],
        json!(OVER_CAP_COUNT),
        "details must name the given length: {body}"
    );

    // Parity: the GET surface produces the IDENTICAL envelope to the legacy
    // dispatch given the same 1101-entry array — the DoS bound is not bypassed
    // by comma-splitting (the split Vec hits the same over-cap guard).
    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_node",
        json!({
            "node_id": id.as_u64(),
            "max_response_bytes": CAP,
            "priority_properties": names,
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(body),
        normalize(legacy),
        "GET over-cap comma list must equal the legacy over-cap array rejection"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (g) get_edge (edge_tools::GetEdgeQuery) — a bulky EDGE elides its unprotected
//     property while the two named ones survive; parity with the legacy array.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_edge_priority_properties_get_param_prioritizes_and_matches_legacy() {
    let db = fresh_db();
    let (_alice, edge) = seed_bulky_edge(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, budgeted) = get(
        &client,
        &format!(
            "/edges/{}?max_response_bytes={CAP}&priority_properties=note,bio",
            edge.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "get_edge GET priority_properties must not 400: {budgeted}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "get_edge shaped: {budgeted}"
    );

    let p = props(&budgeted);
    assert!(
        p["note"].is_string(),
        "protected edge `note` must survive in full: {p}"
    );
    assert!(
        p["bio"].is_string(),
        "protected edge `bio` must survive in full: {p}"
    );
    assert!(
        is_elided(&p["memo"]),
        "unprotected bulky edge `memo` must be elided: {p}"
    );

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_edge",
        json!({
            "edge_id": edge.as_u64(),
            "max_response_bytes": CAP,
            "priority_properties": ["note", "bio"],
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "get_edge GET priority_properties=note,bio must equal the legacy array form"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (h) get_outgoing_edges (edge_tools::AdjacencyQuery — shared with the incoming
//     handler) honors the GET param: the listed edge's unprotected prop elides
//     while the named ones survive; parity with the legacy array.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_outgoing_edges_priority_properties_get_param_parity() {
    let db = fresh_db();
    let (alice, _edge) = seed_bulky_edge(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // A single ~7.3 KiB edge inside the list wrapper needs more headroom than the
    // single-entity CAP so the ladder keeps the protected props (lands on
    // elided_properties / entity_summaries) rather than dropping the whole edge
    // (counts_and_handles).
    let cap: u64 = 6800;
    let (status, budgeted) = get(
        &client,
        &format!(
            "/nodes/{}/edges/outgoing?max_response_bytes={cap}&priority_properties=note,bio",
            alice.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "get_outgoing_edges GET priority_properties must not 400: {budgeted}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "get_outgoing_edges shaped: {budgeted}"
    );

    // The single listed edge protects the two named props; the unprotected `memo`
    // did not survive (inline-elided or swept, per the applied rung).
    let edge0 = &budgeted["edges"][0]["properties"];
    assert!(
        edge0["note"].is_string(),
        "protected `note` on the listed edge survives in full: {budgeted}"
    );
    assert!(
        edge0["bio"].is_string(),
        "protected `bio` on the listed edge survives in full: {budgeted}"
    );
    assert!(
        did_not_survive(&edge0["memo"]),
        "unprotected `memo` on the listed edge did not survive: {budgeted}"
    );

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_outgoing_edges",
        json!({
            "node_id": alice.as_u64(),
            "max_response_bytes": cap,
            "priority_properties": ["note", "bio"],
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "get_outgoing_edges GET priority_properties=note,bio must equal the legacy array form"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (i) get_node_history (traverse_temporal_tools::HistoryBudgetQuery) — a bulky
//     single-version history elides its unprotected prop while the named ones
//     survive; parity with the legacy array. The per-version micros-as-string
//     temporal fields are dropped by `normalize` (shared with the other reads).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_node_history_priority_properties_get_param_parity() {
    let db = fresh_db();
    let id = seed_three_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, budgeted) = get(
        &client,
        &format!(
            "/nodes/{}/history?max_response_bytes={CAP}&priority_properties=note,bio",
            id.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "get_node_history GET priority_properties must not 400: {budgeted}"
    );
    assert!(
        budgeted["budget"].is_object(),
        "get_node_history shaped: {budgeted}"
    );

    // The lone version protects the two named props; the unprotected `memo` did
    // not survive (inline-elided or swept into `_budget_omitted`, per the rung).
    let v0 = &budgeted["versions"][0]["properties"];
    assert!(
        v0["note"].is_string(),
        "protected `note` in the history version survives in full: {budgeted}"
    );
    assert!(
        v0["bio"].is_string(),
        "protected `bio` in the history version survives in full: {budgeted}"
    );
    assert!(
        did_not_survive(&v0["memo"]),
        "unprotected `memo` in the history version did not survive: {budgeted}"
    );

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_node_history",
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
        "get_node_history GET priority_properties=note,bio must equal the legacy array form"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (j) get_schema (schema_batch_tools::GetSchemaQuery) — the schema response has
//     no per-entity bulky property values for the ladder to protect, so this is
//     the documented FALLBACK: assert the comma param is accepted (200, not the
//     old 400) and the GET body equals the legacy dispatch given the array form.
//     A dropped `deserialize_with` annotation would 400 here, so this still
//     locks the crate-wide claim for the `get_schema` route.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_schema_priority_properties_get_param_accepted_and_parity() {
    let db = fresh_db();
    seed_three_bulky_person(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // Include a budget param so `priority_properties` is actually consumed by
    // parse_budget (not early-returned), exercising the full GET → budget path.
    let cap: u64 = 5000;
    let (status, body) = get(
        &client,
        &format!("/schema?max_response_bytes={cap}&priority_properties=name,title"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(
        status, 200,
        "get_schema GET priority_properties must be accepted, not 400: {body}"
    );

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_schema",
        json!({
            "max_response_bytes": cap,
            "priority_properties": ["name", "title"],
        }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(body),
        normalize(legacy),
        "get_schema GET priority_properties=name,title must equal the legacy array form"
    );
}
