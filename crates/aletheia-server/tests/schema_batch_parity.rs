//! Parity + behavior tests for the SCHEMA / STATS / EXTENT + BATCH cluster
//! (Issue #3524, PR6): `get_schema`, `temporal_extent`, `database_stats`, and
//! the atomic multi-write `apply_batch`.
//!
//! Each test drives the new crate's autumn [`TestClient`] and asserts the
//! response equals the **legacy MCP surface** over the same (reads) or an
//! identically-seeded twin (the mutating `apply_batch`) `Arc<AletheiaDB>` —
//! byte-for-byte (parsed-JSON equality, `trace_id` + wall-clock-volatile
//! bi-temporal timestamps normalized) — in both anonymous and required auth
//! modes.
//!
//! This slice spans **three** access classes (the first Metrics tool + a Write
//! in an otherwise read-heavy migration), each verified against
//! `tests/parity/inventory.json`: `get_schema`=Read (budgetable),
//! `temporal_extent`=Read, `database_stats`=**Metrics**, `apply_batch`=**Write**.
//! The legacy reference is `dispatch_tool_json` for the dispatch-routed budgetable
//! `get_schema` and the typed methods for the other three.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::mcp::{
    AletheiaMcpServer, ApplyBatchRequest, DatabaseStatsRequest, TemporalExtentRequest,
};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnop";

/// A long note so a single schema/entity response can exceed a modest byte
/// budget, forcing the #3353 ladder to shape it.
const LONG_NOTE: &str = "a deliberately long note property, repeated so the full \
     serialized response comfortably exceeds a moderate byte budget and the #3353 \
     ladder must shape it down: lorem ipsum dolor sit amet consectetur adipiscing \
     elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua ut enim \
     ad minim veniam quis nostrud exercitation ullamco laboris nisi ut aliquip ex";

fn make_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("insert admin key");
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("insert reader key");
    store
        .insert_bootstrap_key("metrics", Role::Metrics, METRICS_TOKEN)
        .expect("insert metrics key");
    store
}

fn mcp_server(db: &Arc<AletheiaDB>) -> AletheiaMcpServer {
    AletheiaMcpServer::new(db.clone())
}

/// A shared DB with a small graph: Alice --KNOWS--> Bob, plus a `Document`
/// node, so `get_schema` reports two node labels + one edge type and
/// `temporal_extent` has real bounds. Returns `(db, store, alice)`.
fn fixture() -> (Arc<AletheiaDB>, Arc<AuthStore>, NodeId) {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30_i64)
                .build(),
        )
        .expect("create alice");
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .expect("create bob");
    db.create_node(
        "Document",
        PropertyMapBuilder::new().insert("title", "Spec").build(),
    )
    .expect("create doc");
    db.create_edge(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020_i64).build(),
    )
    .expect("create edge");

    (db, make_store(), alice)
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
                            // temporal_extent bounds are wall-clock "now" for
                            // freshly-committed data; the *shape* (present vs
                            // null) is asserted separately.
                            | "earliest"
                            | "latest"
                            | "valid_time"
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

// ════════════════════════════════════════════════════════════════════════════
// Byte-parity for the three read/metrics tools vs the legacy MCP surface, in
// both auth modes (they share one DB — no mutation). `apply_batch` (a write) is
// covered by its own two-DB parity tests below.
// ════════════════════════════════════════════════════════════════════════════

async fn assert_reads_parity(mode: AuthMode, token: Option<&'static str>) {
    let (db, store, _alice) = fixture();
    let client = build_server_client(db.clone(), store.clone(), mode);
    let server = mcp_server(&db);

    // 1. get_schema (GET) — dispatch reference (budgetable, dispatch-routed).
    let (s, autumn) = get(&client, "/schema", token).await;
    assert_eq!(s, 200, "get_schema: {autumn}");
    let legacy: Value = serde_json::from_str(&server.dispatch_tool_json("get_schema", json!({})))
        .expect("legacy get_schema json");
    assert_eq!(normalize(autumn), normalize(legacy), "get_schema parity");

    // 2. temporal_extent (GET) — typed-method reference.
    let (s, autumn) = get(&client, "/temporal_extent", token).await;
    assert_eq!(s, 200, "temporal_extent: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.temporal_extent(TemporalExtentRequest { by_label: None }))
            .expect("legacy temporal_extent json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "temporal_extent parity"
    );

    // 3. database_stats (GET, metrics-class) — typed-method reference. A metrics
    //    token satisfies the MetricsClass gate; in anonymous mode no token.
    let stats_token = match mode {
        AuthMode::Required => Some(METRICS_TOKEN),
        _ => token,
    };
    let (s, autumn) = get(&client, "/database_stats", stats_token).await;
    assert_eq!(s, 200, "database_stats: {autumn}");
    // `database_stats` is now role-sensitive (Issue #3678): the changefeed
    // per-principal breakdown is admin-gated. The reference must be computed at
    // the SAME visibility the HTTP route uses for the driving token — the
    // metrics token is non-admin in Required mode, while anonymous mode's
    // synthetic principal is admin — else the reference would carry a
    // `per_principal` key the gated route omits.
    let stats_include_per_principal = matches!(mode, AuthMode::Anonymous);
    let legacy: Value = serde_json::from_str(
        &server
            .database_stats_with_visibility(DatabaseStatsRequest {}, stats_include_per_principal),
    )
    .expect("legacy database_stats json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "database_stats parity"
    );
}

#[tokio::test]
async fn reads_byte_parity_required_mode() {
    // Reads authenticate as reader (Reader ⊇ Read & Metrics), so the shared
    // helper's reader token satisfies get_schema/temporal_extent; database_stats
    // uses the metrics token (also valid — Reader ⊇ Metrics, but the metrics
    // token exercises the exact MetricsClass gate).
    assert_reads_parity(AuthMode::Required, Some(READER_TOKEN)).await;
}

#[tokio::test]
async fn reads_byte_parity_anonymous_mode() {
    assert_reads_parity(AuthMode::Anonymous, None).await;
}

// ════════════════════════════════════════════════════════════════════════════
// get_schema: bi-temporal AS OF + #3353 budget shaping.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_schema_as_of_matches_legacy() {
    let (db, store, _alice) = fixture();
    let client = build_server_client(db.clone(), store.clone(), AuthMode::Required);
    let server = mcp_server(&db);

    // A bi-temporal AS OF snapshot (both dimensions "now"): switches to the
    // point-in-time schema path (scans version heads) rather than current-state.
    let now = "2999-01-01T00:00:00Z"; // far future → everything valid & recorded
    let uri = format!("/schema?as_of_valid_time={now}&as_of_transaction_time={now}");
    let (s, autumn) = get(&client, &uri, Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "get_schema as_of: {autumn}");

    let legacy_args = json!({
        "as_of_valid_time": now,
        "as_of_transaction_time": now,
    });
    let legacy: Value = serde_json::from_str(&server.dispatch_tool_json("get_schema", legacy_args))
        .expect("legacy as_of json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "get_schema AS OF parity"
    );
    // Both Person and Document should be present in the snapshot.
    let text = serde_json::to_string(&autumn).expect("serialize");
    assert!(text.contains("Person"), "Person label present: {autumn}");
    assert!(
        text.contains("Document"),
        "Document label present: {autumn}"
    );
}

#[tokio::test]
async fn get_schema_budget_shapes_response_and_matches_legacy() {
    // A graph with many labels + bulky property keys so the full schema exceeds a
    // modest byte cap and the #3353 ladder shapes it.
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let bulky = LONG_NOTE.repeat(4);
    for i in 0..12 {
        db.create_node(
            format!("Label{i}").as_str(),
            PropertyMapBuilder::new()
                .insert("name", format!("n{i}").as_str())
                .insert(bulky.as_str(), "v")
                .build(),
        )
        .expect("create node");
    }
    let store = make_store();
    let client = build_server_client(db.clone(), store, AuthMode::Required);
    let server = mcp_server(&db);

    let (s, full) = get(&client, "/schema", Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "get_schema full: {full}");
    assert!(full.get("budget").is_none(), "no budget block: {full}");

    let cap: u64 = 800;
    assert!(
        len_of(&full) as u64 > cap,
        "full response should exceed the cap: {}",
        len_of(&full)
    );

    // Legacy dispatch is the reference; its emitted string is the exact bytes the
    // #3353 contract bounds.
    let budget_args = json!({ "max_response_bytes": cap });
    let legacy_text = server.dispatch_tool_json("get_schema", budget_args);
    assert!(
        legacy_text.len() as u64 <= cap,
        "emitted string must fit the byte budget: {} > {cap}",
        legacy_text.len()
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");

    let (s, budgeted) = get(
        &client,
        &format!("/schema?max_response_bytes={cap}"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(s, 200, "get_schema budgeted: {budgeted}");
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped response smaller: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn get_schema must equal the legacy dispatch"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// temporal_extent: empty-db explicit nulls + by_label breakdown.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn temporal_extent_empty_db_returns_explicit_nulls() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let store = make_store();
    let client = build_server_client(db.clone(), store, AuthMode::Required);
    let server = mcp_server(&db);

    let (s, autumn) = get(&client, "/temporal_extent", Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "temporal_extent empty: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.temporal_extent(TemporalExtentRequest { by_label: None }))
            .expect("legacy json");
    // Full byte parity (no data → no volatile timestamps to normalize).
    assert_eq!(autumn, legacy, "empty-db temporal_extent parity");
    // Empty DB → explicit null bounds, never epoch 0.
    assert!(
        autumn["valid_time"]["earliest"].is_null(),
        "empty valid_time.earliest is null: {autumn}"
    );
    assert!(
        autumn["transaction_time"]["latest"].is_null(),
        "empty transaction_time.latest is null: {autumn}"
    );
}

#[tokio::test]
async fn temporal_extent_by_label_matches_legacy() {
    let (db, store, _alice) = fixture();
    let client = build_server_client(db.clone(), store, AuthMode::Required);
    let server = mcp_server(&db);

    let (s, autumn) = get(
        &client,
        "/temporal_extent?by_label=true",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(s, 200, "temporal_extent by_label: {autumn}");
    let legacy: Value = serde_json::from_str(&server.temporal_extent(TemporalExtentRequest {
        by_label: Some(true),
    }))
    .expect("legacy by_label json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "temporal_extent by_label parity"
    );
    // The per-label breakdown surfaces node labels / edge types.
    let text = serde_json::to_string(&autumn).expect("serialize");
    assert!(
        text.contains("node_labels") || text.contains("Person"),
        "by_label breakdown present: {autumn}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// database_stats: snapshot shape + MetricsClass gating (the handler carries
// MetricsClass, not ReadClass). We prove the gate via the access matrix — a
// metrics-role token (grants ONLY Metrics) reaches database_stats but is denied
// a ReadClass tool — WITHOUT touching security internals.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn database_stats_snapshot_shape() {
    let (db, store, _alice) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let (s, body) = get(&client, "/database_stats", Some(METRICS_TOKEN)).await;
    assert_eq!(s, 200, "database_stats: {body}");
    // Holistic snapshot: current size, historical depth, cold_storage presence,
    // WAL state — the #3222 shape.
    assert!(body["current"].is_object(), "current block: {body}");
    assert!(
        body["current"]["node_count"].is_number() || body["current"].get("node_count").is_some(),
        "current.node_count present: {body}"
    );
    assert!(body["historical"].is_object(), "historical block: {body}");
    assert!(
        body["cold_storage"].is_object(),
        "cold_storage block: {body}"
    );
    assert!(body["wal"].is_object(), "wal block: {body}");
}

#[tokio::test]
async fn database_stats_is_metrics_class_gated_not_read() {
    // The metrics role grants ONLY the Metrics class. If `database_stats` were
    // mis-declared ReadClass, the metrics token would be denied it; if a
    // ReadClass tool (`get_schema`) were reachable by the metrics token, the
    // metrics role would grant Read. Both together pin the handler's declared
    // class to Metrics (verified through the auth surface, not internals).
    let (db, store, _alice) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    // Metrics token → database_stats is permitted (200): the handler's class is
    // ≤ Metrics.
    let (s, _body) = get(&client, "/database_stats", Some(METRICS_TOKEN)).await;
    assert_eq!(s, 200, "metrics token permitted on database_stats");

    // Metrics token → get_schema (ReadClass) is denied (403): the metrics role
    // lacks Read, so database_stats being permitted proves it is Metrics, not
    // Read.
    let (s, body) = get(&client, "/schema", Some(METRICS_TOKEN)).await;
    assert_eq!(s, 403, "metrics token denied on a ReadClass tool: {body}");
    assert_eq!(
        body["error"]["code"], "PERMISSION_DENIED",
        "denial code: {body}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// apply_batch: the WriteClass atomic multi-write. Each test compares the autumn
// response against the legacy `apply_batch` typed method over an IDENTICALLY
// seeded twin DB (a write mutates, so one shared DB would double-apply). Id
// generation is deterministic from a fresh `AletheiaDB::new()`.
// ════════════════════════════════════════════════════════════════════════════

/// Run `req` against the autumn `/batch` surface (db `a`) and the legacy typed
/// method (twin db `b`), returning `(autumn, legacy)` normalized-comparable
/// values. Both DBs must be seeded identically before calling.
async fn batch_parity(a: Arc<AletheiaDB>, b: Arc<AletheiaDB>, req: Value) -> (Value, Value) {
    let store = make_store();
    let client = build_server_client(a, store, AuthMode::Required);
    let (s, autumn) = post(&client, "/batch", &req, Some(ADMIN_TOKEN)).await;
    assert_eq!(s, 200, "apply_batch autumn: {autumn}");

    let legacy_server = mcp_server(&b);
    let typed: ApplyBatchRequest =
        serde_json::from_value(req).expect("request deserializes as ApplyBatchRequest");
    let legacy: Value =
        serde_json::from_str(&legacy_server.apply_batch(typed)).expect("legacy apply_batch json");
    (autumn, legacy)
}

#[tokio::test]
async fn apply_batch_success_multi_op_with_ref_map_and_input_order() {
    let a = Arc::new(AletheiaDB::new().expect("db a"));
    let b = Arc::new(AletheiaDB::new().expect("db b"));

    // A subgraph in ONE atomic call: create two nodes (one aliased) + an edge
    // referencing the alias.
    let req = json!({
        "operations": [
            { "op": "create_node", "label": "Person", "properties": {"name": "Alice"}, "ref": "alice" },
            { "op": "create_node", "label": "Person", "properties": {"name": "Bob"}, "ref": "bob" },
            { "op": "create_edge", "source_id": "$alice", "target_id": "$bob", "label": "KNOWS" }
        ]
    });

    let (autumn, legacy) = batch_parity(a, b, req).await;
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "apply_batch success parity"
    );
    assert_eq!(autumn["success"], json!(true), "batch succeeded: {autumn}");
    // Per-op results in input order (3 ops).
    let results = autumn["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        3,
        "one result per op in input order: {autumn}"
    );
    // ref_map exposes the alias→committed-id mapping.
    assert!(
        autumn["ref_map"]["alice"].is_number(),
        "ref_map has alice: {autumn}"
    );
    assert!(
        autumn["ref_map"]["bob"].is_number(),
        "ref_map has bob: {autumn}"
    );
}

#[tokio::test]
async fn apply_batch_per_op_failure_reports_failed_op_index() {
    let a = Arc::new(AletheiaDB::new().expect("db a"));
    let b = Arc::new(AletheiaDB::new().expect("db b"));

    // Op 0 is fine; op 1's edge references an UNKNOWN local ref (`$ghost`) — a
    // static pre-validation failure rejected BEFORE any transaction opens, so the
    // per-op error carries `details.failed_op_index` pointing at op 1, and ZERO
    // writes take effect (all-or-nothing).
    let req = json!({
        "operations": [
            { "op": "create_node", "label": "Person", "properties": {"name": "Alice"} },
            { "op": "create_edge", "source_id": "$ghost", "target_id": "$ghost", "label": "KNOWS" }
        ]
    });

    let (autumn, legacy) = batch_parity(a.clone(), b, req).await;
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "apply_batch per-op-failure parity"
    );
    // The structured error names the failing op index (op 1, the edge).
    assert!(autumn.get("error").is_some(), "error payload: {autumn}");
    assert_eq!(
        autumn["error"]["details"]["failed_op_index"],
        json!(1),
        "failed_op_index points at the edge op: {autumn}"
    );
    // Rejected statically before any transaction opened: db `a` has no nodes.
    assert_eq!(a.node_count(), 0, "all-or-nothing: no nodes committed");
}

#[tokio::test]
async fn apply_batch_over_cap_is_rejected_with_null_failed_op_index() {
    let a = Arc::new(AletheiaDB::new().expect("db a"));
    let b = Arc::new(AletheiaDB::new().expect("db b"));

    // Default cap is 1000 ops; 1001 is rejected statically before any
    // transaction opens (failed_op_index is JSON null for the over-cap
    // rejection).
    let ops: Vec<Value> = (0..1001)
        .map(|_| json!({ "op": "create_node", "label": "Person" }))
        .collect();
    let req = json!({ "operations": ops });

    let (autumn, legacy) = batch_parity(a.clone(), b, req).await;
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "apply_batch over-cap parity"
    );
    assert!(autumn.get("error").is_some(), "over-cap error: {autumn}");
    assert_eq!(
        autumn["error"]["details"]["failed_op_index"],
        Value::Null,
        "over-cap rejection has null failed_op_index: {autumn}"
    );
    assert_eq!(a.node_count(), 0, "over-cap rejected before any commit");
}

#[tokio::test]
async fn apply_batch_in_batch_detach_delete_contract() {
    // Two identically-seeded DBs: a Person with one outgoing edge already
    // committed. An in-batch delete_node without detach must be REFUSED (the
    // #3209 contract applies against committed edges via the batch-local
    // adjacency ledger); with detach:true it succeeds.
    fn seed(db: &Arc<AletheiaDB>) -> (NodeId, NodeId) {
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .expect("alice");
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .expect("bob");
        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .expect("edge");
        (alice, bob)
    }

    let a = Arc::new(AletheiaDB::new().expect("db a"));
    let b = Arc::new(AletheiaDB::new().expect("db b"));
    let (alice_a, _) = seed(&a);
    let (alice_b, _) = seed(&b);
    assert_eq!(alice_a, alice_b, "deterministic ids across twins");

    // No detach → refused (connected_edges reported), all-or-nothing.
    let refuse_req = json!({
        "operations": [
            { "op": "delete_node", "node_id": alice_a.as_u64() }
        ]
    });
    let (autumn, legacy) = batch_parity(a.clone(), b.clone(), refuse_req).await;
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "apply_batch DETACH-refuse parity"
    );
    assert!(
        autumn.get("error").is_some(),
        "delete_node without detach refused: {autumn}"
    );

    // detach:true → succeeds. Fresh twins to keep the comparison clean.
    let c = Arc::new(AletheiaDB::new().expect("db c"));
    let d = Arc::new(AletheiaDB::new().expect("db d"));
    let (alice_c, _) = seed(&c);
    seed(&d);
    let detach_req = json!({
        "operations": [
            { "op": "delete_node", "node_id": alice_c.as_u64(), "detach": true }
        ]
    });
    let (autumn, legacy) = batch_parity(c, d, detach_req).await;
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "apply_batch DETACH-delete parity"
    );
    assert_eq!(
        autumn["success"],
        json!(true),
        "detach delete succeeded: {autumn}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Auth: keyless required-mode calls are 401 for all four tools.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn schema_batch_tools_require_credential_in_required_mode() {
    let (db, store, _alice) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    for uri in ["/schema", "/temporal_extent", "/database_stats"] {
        let resp = client.get(uri).send().await;
        assert_eq!(resp.status.as_u16(), 401, "{uri} unauth");
        assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
    }
    let resp = client
        .post("/batch")
        .json(&json!({"operations": []}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401, "/batch unauth");
}

// ════════════════════════════════════════════════════════════════════════════
// Conformance: get_schema (the sole dispatch-routed budgetable read in this
// cluster) is pinned in the shared DISPATCH_ROUTED_READ_TOOLS table with Read
// class (extends the coverage `dispatch_pinned_names_match_routed_class` in
// server_parity.rs asserts). temporal_extent / database_stats / apply_batch are
// typed/non-budgetable and are intentionally NOT pinned.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dispatch_routed_table_includes_get_schema() {
    use aletheia_server::edge_tools::DISPATCH_ROUTED_READ_TOOLS;
    let entry = DISPATCH_ROUTED_READ_TOOLS
        .iter()
        .find(|(n, _)| *n == "get_schema")
        .expect("get_schema must be pinned in DISPATCH_ROUTED_READ_TOOLS");
    assert_eq!(entry.1, AccessClass::Read, "get_schema pinned as Read");

    for name in ["temporal_extent", "database_stats", "apply_batch"] {
        assert!(
            !DISPATCH_ROUTED_READ_TOOLS.iter().any(|(n, _)| *n == name),
            "{name} is typed/non-budgetable and must NOT be dispatch-routed"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// OpenAPI schema fidelity: apply_batch (POST) must expose a named, per-operation
// request schema in /openapi.json — never the generic `Value`; get_schema /
// temporal_extent / database_stats (GET) are documented.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn openapi_documents_the_four_tools() {
    let (db, store, _alice) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let spec: Value = serde_json::from_str(&resp.text()).expect("openapi json");

    // apply_batch POST body refs a typed per-operation schema.
    let schema =
        &spec["paths"]["/batch"]["post"]["requestBody"]["content"]["application/json"]["schema"];
    let sref = schema["$ref"]
        .as_str()
        .unwrap_or_else(|| panic!("/batch requestBody schema must be a $ref: {schema}"));
    assert!(
        sref.ends_with("/ApplyBatchRequest"),
        "/batch request body must ref the typed ApplyBatchRequest schema, got {sref}"
    );
    assert_ne!(
        sref, "#/components/schemas/Value",
        "/batch must not degrade to the shared generic Value component"
    );

    // The three GET reads are documented.
    for path in ["/schema", "/temporal_extent", "/database_stats"] {
        assert!(
            !spec["paths"][path]["get"].is_null(),
            "GET {path} documented: {}",
            spec["paths"]
        );
    }
}
