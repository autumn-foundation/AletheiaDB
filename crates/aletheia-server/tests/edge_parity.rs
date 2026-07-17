//! Parity tests for the edge cluster (Issue #3524, PR3).
//!
//! Mirrors `tests/node_writes_parity.rs` (the PR2 write cluster) for the nine
//! edge tools ported in PR3: the reads `get_edge`, `list_edges`, `count_edges`,
//! `get_outgoing_edges`, `get_incoming_edges`, and the writes `create_edge`,
//! `update_edge`, `delete_edge`, `retract_edge`. Each test drives the new
//! crate's autumn [`TestClient`] (HTTP and/or `/mcp`) and asserts the response
//! is byte-identical to the **legacy** MCP typed method ([`AletheiaMcpServer`])
//! for the same operation.
//!
//! # Why two databases + volatile-field normalization
//!
//! A write mutates, so the autumn surface and the legacy method cannot share one
//! `Arc<AletheiaDB>` without double-applying. Instead each write test runs the
//! *same* logical operation against **two freshly-seeded, identical** databases
//! (id generation is deterministic from a fresh `AletheiaDB::new()`, guarded by
//! an explicit assertion where relevant) and compares after stripping the
//! wall-clock-volatile fields (`trace_id` + the bi-temporal timestamps
//! `valid_from`/`valid_to`/`transaction_from`/`transaction_to`, which differ by
//! microseconds between two independent commits). The structural payload — ids,
//! labels, properties, success flags, `deleted_edge_id`, `already_retracted`,
//! `is_current`, row counts — is asserted byte-equal; the meaningful volatile
//! values (`already_retracted`, `is_current`) get their own targeted assertions
//! on the autumn response directly, where they are deterministic.
//!
//! Contracts covered: #3232 temporal block, #3220 vector elision (default +
//! `include_vectors`), #3230 retraction (idempotent), #3221 `valid_time` on the
//! write path — plus both auth modes and the `/mcp` catalog surfacing the nine
//! tools with their access classes.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::{EdgeId, NodeId};
use aletheiadb::mcp::{
    AletheiaMcpServer, CountEdgesRequest, CreateEdgeRequest, DeleteEdgeRequest, GetEdgeRequest,
    GetIncomingEdgesRequest, GetOutgoingEdgesRequest, ListEdgesRequest, RetractEdgeRequest,
    UpdateEdgeRequest,
};
use autumn_web::test::TestClient;
use serde_json::{Value, json};
use std::collections::HashMap;

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const EMBEDDING: &[f32] = &[0.10, 0.20, 0.30, 0.40];

/// A shared auth store with admin + reader keys. Writes authenticate as admin
/// (the `admin` role satisfies every access class); reads authenticate as
/// reader (the `reader` role satisfies [`ReadClass`]).
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

/// A fresh, empty database.
fn fresh_db() -> Arc<AletheiaDB> {
    Arc::new(AletheiaDB::new().expect("create db"))
}

fn mcp_server(db: &Arc<AletheiaDB>) -> AletheiaMcpServer {
    AletheiaMcpServer::new(db.clone())
}

/// Recursively drop the per-request `trace_id` and every wall-clock-volatile
/// bi-temporal timestamp so two independent commits are comparable. Structural
/// fields (ids, labels, properties, flags, counts, `is_current`) survive.
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

fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// Seed `Alice --KNOWS--> Bob` on `db`, returning `(alice, bob, edge)`. Two
/// fresh dbs seeded this way assign identical ids (deterministic generation).
fn seed_edge(db: &Arc<AletheiaDB>) -> (NodeId, NodeId, EdgeId) {
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
            PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        )
        .expect("seed edge");
    (alice, bob, edge)
}

/// Seed `Alice --KNOWS[embedding]--> Bob`, returning `(alice, bob, edge)`.
fn seed_edge_with_vector(db: &Arc<AletheiaDB>) -> (NodeId, NodeId, EdgeId) {
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
                .insert("since", 2020_i64)
                .insert_vector("embedding", EMBEDDING)
                .build(),
        )
        .expect("seed edge");
    (alice, bob, edge)
}

// ════════════════════════════════════════════════════════════════════════════
// create_edge (#3221 valid_time, #3232 temporal block)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn create_edge_byte_parity_with_legacy_mcp() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    // Seed two identical nodes on both dbs so the edge endpoints exist.
    let seed_nodes = |db: &Arc<AletheiaDB>| -> (NodeId, NodeId) {
        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        (a, b)
    };
    let (a_http, b_http) = seed_nodes(&db_http);
    let (a_mcp, b_mcp) = seed_nodes(&db_mcp);
    assert_eq!((a_http, b_http), (a_mcp, b_mcp), "deterministic node ids");
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let body = json!({
        "source_id": a_http.as_u64(),
        "target_id": b_http.as_u64(),
        "label": "KNOWS",
        "properties": { "since": 2020 }
    });
    let (status, http_body) = post(&client, "/edges", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).create_edge(CreateEdgeRequest {
        namespace: None,
        source_id: a_mcp.as_u64(),
        target_id: b_mcp.as_u64(),
        label: "KNOWS".to_string(),
        properties: Some(props(&[("since", json!(2020))])),
        valid_time: None,
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");

    assert_eq!(
        normalize(http_body.clone()),
        normalize(legacy),
        "create_edge HTTP body must equal legacy MCP method"
    );
    // #3232: the created edge carries a temporal block, current at creation.
    assert!(
        http_body["temporal"].is_object(),
        "temporal block: {http_body}"
    );
    assert_eq!(http_body["temporal"]["is_current"], true);
    assert_eq!(http_body["label"], "KNOWS");
    assert_eq!(http_body["source_id"], a_http.as_u64());
    assert_eq!(http_body["target_id"], b_http.as_u64());
    assert_eq!(http_body["properties"]["since"], 2020);
}

#[tokio::test]
async fn create_edge_backdated_valid_time_honored() {
    // #3221: an explicit valid_time backdates when the relationship became true.
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let seed_nodes = |db: &Arc<AletheiaDB>| -> (NodeId, NodeId) {
        let a = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let b = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        (a, b)
    };
    let (a_http, b_http) = seed_nodes(&db_http);
    let (a_mcp, b_mcp) = seed_nodes(&db_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let vt = "2020-01-15T10:00:00Z";
    let body = json!({
        "source_id": a_http.as_u64(),
        "target_id": b_http.as_u64(),
        "label": "KNOWS",
        "valid_time": vt
    });
    let (status, http_body) = post(&client, "/edges", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(
        http_body["temporal"]["valid_from"], "2020-01-15T10:00:00.000000Z",
        "backdated valid_from must be honored: {http_body}"
    );

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).create_edge(CreateEdgeRequest {
        namespace: None,
        source_id: a_mcp.as_u64(),
        target_id: b_mcp.as_u64(),
        label: "KNOWS".to_string(),
        properties: None,
        valid_time: Some(vt.to_string()),
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

#[tokio::test]
async fn create_edge_anonymous_mode_parity() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let seed_nodes = |db: &Arc<AletheiaDB>| -> (NodeId, NodeId) {
        let a = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let b = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        (a, b)
    };
    let (a_http, b_http) = seed_nodes(&db_http);
    let (a_mcp, b_mcp) = seed_nodes(&db_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Anonymous);

    let body = json!({
        "source_id": a_http.as_u64(),
        "target_id": b_http.as_u64(),
        "label": "KNOWS"
    });
    let (status, http_body) = post(&client, "/edges", &body, None).await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).create_edge(CreateEdgeRequest {
        namespace: None,
        source_id: a_mcp.as_u64(),
        target_id: b_mcp.as_u64(),
        label: "KNOWS".to_string(),
        properties: None,
        valid_time: None,
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

#[tokio::test]
async fn create_edge_unauthenticated_401() {
    let client = build_server_client(fresh_db(), make_store(), AuthMode::Required);
    let resp = client
        .post("/edges")
        .json(&json!({ "source_id": 1, "target_id": 2, "label": "KNOWS" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
}

// ════════════════════════════════════════════════════════════════════════════
// get_edge — #3232 temporal block, #3220 vector elision (default + include)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_edge_byte_parity_and_temporal_block() {
    let db = fresh_db();
    let (_a, _b, edge) = seed_edge(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, http_body) = get(
        &client,
        &format!("/edges/{}", edge.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(http_body["id"], edge.as_u64());
    assert_eq!(http_body["label"], "KNOWS");
    assert!(http_body["temporal"].is_object(), "temporal: {http_body}");
    assert_eq!(http_body["temporal"]["is_current"], true);

    // Shared-db byte parity: get is a pure read, so both surfaces observe the
    // same version and are byte-identical without normalization.
    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_edge(GetEdgeRequest {
        namespace: None,
        edge_id: edge.as_u64(),
        include_vectors: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(
        normalize(http_body),
        normalize(legacy),
        "get_edge HTTP body must equal legacy MCP method"
    );
}

#[tokio::test]
async fn get_edge_vector_elided_by_default_and_full_with_flag() {
    let db = fresh_db();
    let (_a, _b, edge) = seed_edge_with_vector(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // #3220: vectors elided by default on the read path.
    let (status, elided) = get(
        &client,
        &format!("/edges/{}", edge.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        elided["properties"]["embedding"]["elided"], true,
        "vector elided by default: {elided}"
    );
    let legacy_elided: Value = serde_json::from_str(&mcp_server(&db).get_edge(GetEdgeRequest {
        namespace: None,
        edge_id: edge.as_u64(),
        include_vectors: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(elided), normalize(legacy_elided));

    // include_vectors=true returns the raw array.
    let (status, full) = get(
        &client,
        &format!("/edges/{}?include_vectors=true", edge.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        full["properties"]["embedding"].is_array(),
        "include_vectors=true returns the raw array: {full}"
    );
    let legacy_full: Value = serde_json::from_str(&mcp_server(&db).get_edge(GetEdgeRequest {
        namespace: None,
        edge_id: edge.as_u64(),
        include_vectors: Some(true),
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(full), normalize(legacy_full));
}

#[tokio::test]
async fn get_edge_missing_returns_not_found_in_band() {
    let db = fresh_db();
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);
    let (status, http_body) = get(&client, "/edges/999", Some(READER_TOKEN)).await;
    assert_eq!(status, 200, "MCP method returns 200 with in-band error");
    assert_eq!(http_body["error"]["code"], "NOT_FOUND");

    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_edge(GetEdgeRequest {
        namespace: None,
        edge_id: 999,
        include_vectors: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

// ════════════════════════════════════════════════════════════════════════════
// update_edge
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn update_edge_byte_parity_with_legacy_mcp() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let (_a_http, _b_http, e_http) = seed_edge(&db_http);
    let (_a_mcp, _b_mcp, e_mcp) = seed_edge(&db_mcp);
    assert_eq!(e_http, e_mcp, "fresh dbs must assign identical edge ids");
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let body = json!({
        "edge_id": e_http.as_u64(),
        "properties": { "since": 2021 }
    });
    let (status, http_body) = post(&client, "/edges/update", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).update_edge(UpdateEdgeRequest {
        namespace: None,
        edge_id: e_mcp.as_u64(),
        properties: props(&[("since", json!(2021))]),
        valid_time: None,
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");

    assert_eq!(normalize(http_body.clone()), normalize(legacy));
    assert_eq!(http_body["properties"]["since"], 2021);
    assert!(http_body["temporal"].is_object());
}

// ════════════════════════════════════════════════════════════════════════════
// delete_edge
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn delete_edge_byte_parity() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let (_a_http, _b_http, e_http) = seed_edge(&db_http);
    let (_a_mcp, _b_mcp, e_mcp) = seed_edge(&db_mcp);
    let client = build_server_client(db_http.clone(), make_store(), AuthMode::Required);

    let body = json!({ "edge_id": e_http.as_u64() });
    let (status, http_body) = post(&client, "/edges/delete", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["success"], true);
    assert_eq!(http_body["deleted_edge_id"], e_http.as_u64());

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).delete_edge(DeleteEdgeRequest {
        namespace: None,
        edge_id: e_mcp.as_u64(),
        valid_time: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
    assert!(db_http.get_edge(e_http).is_err(), "edge deleted");
}

// ════════════════════════════════════════════════════════════════════════════
// retract_edge — #3230 idempotent close of valid-time
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn retract_edge_idempotent_byte_parity() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let (_a_http, _b_http, e_http) = seed_edge(&db_http);
    let (_a_mcp, _b_mcp, e_mcp) = seed_edge(&db_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    // Omit valid_time (defaults to now); valid_from/valid_to are wall-clock
    // volatile and normalized away for the cross-db comparison.
    let body = json!({ "edge_id": e_http.as_u64() });
    let (status, first) = post(&client, "/edges/retract", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(first["success"], true);
    assert_eq!(first["retracted"], true);
    assert_eq!(first["already_retracted"], false);
    assert!(first["valid_to"].is_string(), "valid_to present: {first}");

    let legacy: Value =
        serde_json::from_str(&mcp_server(&db_mcp).retract_edge(RetractEdgeRequest {
            edge_id: e_mcp.as_u64(),
            valid_time: None,
        }))
        .expect("legacy mcp json");
    assert_eq!(normalize(first.clone()), normalize(legacy));

    // Re-retract: idempotent no-op returning the EXISTING interval.
    let (status, second) = post(&client, "/edges/retract", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(second["already_retracted"], true);
    // The reported interval is stable across the two calls.
    assert_eq!(first["valid_from"], second["valid_from"]);
    assert_eq!(first["valid_to"], second["valid_to"]);
}

// ════════════════════════════════════════════════════════════════════════════
// list_edges / count_edges
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_edges_byte_parity() {
    let db = fresh_db();
    seed_edge(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, http_body) = get(&client, "/edges", Some(READER_TOKEN)).await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db).list_edges(ListEdgesRequest {
        namespace: None,
        label: None,
        limit: None,
        offset: None,
        include_vectors: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(
        normalize(http_body),
        normalize(legacy),
        "list_edges HTTP body must equal legacy MCP method"
    );
}

#[tokio::test]
async fn count_edges_byte_parity() {
    let db = fresh_db();
    seed_edge(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, http_body) = get(&client, "/edges/count", Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["count"], 1);

    let legacy: Value =
        serde_json::from_str(&mcp_server(&db).count_edges(CountEdgesRequest { label: None }))
            .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

// ════════════════════════════════════════════════════════════════════════════
// get_outgoing_edges / get_incoming_edges — #3220 elision, #3232 temporal
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_outgoing_edges_byte_parity_and_elision() {
    let db = fresh_db();
    let (alice, _bob, _edge) = seed_edge_with_vector(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, http_body) = get(
        &client,
        &format!("/nodes/{}/edges/outgoing", alice.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(http_body["count"], 1, "one outgoing edge: {http_body}");
    // #3220: vectors elided by default on the read path.
    assert_eq!(
        http_body["edges"][0]["properties"]["embedding"]["elided"], true,
        "vector elided by default: {http_body}"
    );

    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_outgoing_edges(
        GetOutgoingEdgesRequest {
            node_id: alice.as_u64(),
            label: None,
            include_vectors: None,
        },
    ))
    .expect("legacy mcp json");
    assert_eq!(
        normalize(http_body),
        normalize(legacy),
        "get_outgoing_edges HTTP body must equal legacy MCP method"
    );
}

#[tokio::test]
async fn get_outgoing_edges_include_vectors() {
    let db = fresh_db();
    let (alice, _bob, _edge) = seed_edge_with_vector(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, http_body) = get(
        &client,
        &format!(
            "/nodes/{}/edges/outgoing?include_vectors=true",
            alice.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        http_body["edges"][0]["properties"]["embedding"].is_array(),
        "include_vectors=true returns the raw array: {http_body}"
    );

    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_outgoing_edges(
        GetOutgoingEdgesRequest {
            node_id: alice.as_u64(),
            label: None,
            include_vectors: Some(true),
        },
    ))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

#[tokio::test]
async fn get_incoming_edges_byte_parity() {
    let db = fresh_db();
    let (_alice, bob, _edge) = seed_edge(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let (status, http_body) = get(
        &client,
        &format!("/nodes/{}/edges/incoming", bob.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(http_body["count"], 1, "one incoming edge: {http_body}");

    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_incoming_edges(
        GetIncomingEdgesRequest {
            node_id: bob.as_u64(),
            label: None,
            include_vectors: None,
        },
    ))
    .expect("legacy mcp json");
    assert_eq!(
        normalize(http_body),
        normalize(legacy),
        "get_incoming_edges HTTP body must equal legacy MCP method"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// #3353 token budget + #3360 cursor paging on the budgetable/cursorable edge
// reads. These params ride the RAW arguments and are applied in
// `AletheiaMcpServer::dispatch_tool` (the typed per-tool methods bypass that
// pipeline), so the handlers forward through `dispatch_tool_json`. The tests
// use that same public entry as the legacy reference.
// ════════════════════════════════════════════════════════════════════════════

/// Seed `count` outgoing `KNOWS` edges from a fresh source node, each carrying a
/// long `note` property so the full response comfortably exceeds a small byte
/// budget. Returns `(source, edge_ids)`.
fn seed_fanout(db: &Arc<AletheiaDB>, count: usize) -> (NodeId, Vec<EdgeId>) {
    let src = db
        .create_node(
            "Hub",
            PropertyMapBuilder::new().insert("name", "Hub").build(),
        )
        .expect("seed hub");
    let mut edges = Vec::new();
    for i in 0..count {
        let dst = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("idx", i as i64).build(),
            )
            .expect("seed dst");
        let e = db
            .create_edge(
                src,
                dst,
                "KNOWS",
                PropertyMapBuilder::new()
                    .insert("idx", i as i64)
                    .insert(
                        "note",
                        "a deliberately long note property, repeated so the full \
                         serialized response comfortably exceeds a moderate byte budget \
                         and the #3353 ladder must shape it down: lorem ipsum dolor sit \
                         amet consectetur adipiscing elit sed do eiusmod tempor incididunt \
                         ut labore et dolore magna aliqua ut enim ad minim veniam quis",
                    )
                    .build(),
            )
            .expect("seed fanout edge");
        edges.push(e);
    }
    (src, edges)
}

#[tokio::test]
async fn get_outgoing_edges_token_budget_shapes_response() {
    // #3353: a small max_response_bytes shapes the response (disclosed `budget`
    // block, emitted string within the cap) and the autumn surface matches the
    // legacy dispatch byte-for-byte for the same budget; omitting the budget
    // reproduces the full, unshaped response.
    let db = fresh_db();
    let (src, _edges) = seed_fanout(&db, 5);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // Full (no budget): larger than the cap, no `budget` block.
    let (status, full) = get(
        &client,
        &format!("/nodes/{}/edges/outgoing", src.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(full.get("budget").is_none(), "no budget block: {full}");
    assert_eq!(full["count"], 5);
    let full_len = serde_json::to_string(&full).unwrap().len();
    // The cap sits below the full response (so shaping is forced) and above the
    // minimal-rung floor (so it shapes rather than returning the #3353
    // too-small-budget error).
    let cap: u64 = 2000;
    assert!(
        full_len as u64 > cap,
        "full response should exceed the cap: {full_len}"
    );

    // Budgeted: the legacy dispatch is the reference; its emitted string is the
    // exact bytes the #3353 contract bounds.
    let budget_args = json!({ "node_id": src.as_u64(), "max_response_bytes": cap });
    let legacy_text = mcp_server(&db).dispatch_tool_json("get_outgoing_edges", budget_args);
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
        &format!(
            "/nodes/{}/edges/outgoing?max_response_bytes={cap}",
            src.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(budgeted["budget"].is_object(), "shaped: {budgeted}");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn response must equal the legacy dispatch for the same budget"
    );
}

#[tokio::test]
async fn get_edge_token_budget_parity() {
    // The single-entity budgetable read also forwards the budget: a max token
    // budget yields the same shaped result as the legacy dispatch.
    let db = fresh_db();
    let (_a, _b, edge) = seed_edge_with_vector(&db);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    let tokens: u64 = 64;
    let (status, budgeted) = get(
        &client,
        &format!("/edges/{}?max_response_tokens={tokens}", edge.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);

    let legacy_text = mcp_server(&db).dispatch_tool_json(
        "get_edge",
        json!({ "edge_id": edge.as_u64(), "max_response_tokens": tokens }),
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "get_edge budgeted autumn response must equal the legacy dispatch"
    );
}

#[tokio::test]
async fn get_outgoing_edges_cursor_paging_snapshot_anchored() {
    // #3360: use_cursor:true opens a snapshot-anchored scan returning an opaque
    // cursor + first page; passing the cursor back continues it, duplicate-free
    // and gap-free, until exhausted — the union equals the full adjacency.
    let db = fresh_db();
    let (src, edge_ids) = seed_fanout(&db, 3);
    let expected: std::collections::BTreeSet<u64> = edge_ids.iter().map(|e| e.as_u64()).collect();
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // First page: opt in with a per-page size of 1 to force continuation.
    let (status, page1) = get(
        &client,
        &format!(
            "/nodes/{}/edges/outgoing?use_cursor=true&limit=1",
            src.as_u64()
        ),
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

    // Collect all edge ids across pages, following the cursor via the autumn
    // surface. The cursor secret lives on the process-lifetime shared server in
    // `ServerState`, so a token issued on one request resumes on the next (a
    // separate `AletheiaMcpServer` instance signs with a different secret and
    // could not decode it — hence the drive is autumn-only, not cross-instance).
    let mut seen: Vec<u64> = Vec::new();
    let mut page = page1.clone();
    let mut prev_snapshot = page1["snapshot_valid_time"].clone();
    let mut guard = 0;
    loop {
        for e in page["edges"].as_array().expect("edges array") {
            seen.push(e["id"].as_u64().expect("edge id"));
        }
        let Some(token) = page["cursor"].as_str() else {
            break;
        };
        // Continuation: the token carries the scan's node_id; the route still
        // requires a path node_id (superseded by the token). The base64url
        // no-pad token is URL-safe, so it embeds in the query verbatim.
        let token = token.to_string();
        let (status, next) = get(
            &client,
            &format!("/nodes/{}/edges/outgoing?cursor={}", src.as_u64(), token),
            Some(READER_TOKEN),
        )
        .await;
        assert_eq!(status, 200, "continuation page: {next}");
        // Snapshot-anchored: every page answers at the coordinate pinned on the
        // first page.
        assert_eq!(
            next["snapshot_valid_time"], prev_snapshot,
            "continuation stays on the pinned snapshot: {next}"
        );
        prev_snapshot = next["snapshot_valid_time"].clone();
        page = next;
        guard += 1;
        assert!(guard < 10, "cursor scan did not terminate");
    }

    // Duplicate-free and complete: the union equals the full adjacency.
    let seen_set: std::collections::BTreeSet<u64> = seen.iter().copied().collect();
    assert_eq!(seen.len(), seen_set.len(), "no duplicates across pages");
    assert_eq!(
        seen_set, expected,
        "union of pages equals the full adjacency"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// /mcp catalog + tools/call for the nine PR3 tools
// ════════════════════════════════════════════════════════════════════════════

/// The nine PR3 edge tools and their intended access class (sourced from
/// `tests/parity/inventory.json`'s `mcp.tools[].access_class`). Lane B's
/// inventory-anchored RBAC registry classifies each identically; the
/// `mcp_routable_tools_are_all_classified` conformance test in
/// `tests/server_parity.rs` enforces that binding.
const PR3_TOOLS: &[(&str, &str)] = &[
    ("get_edge", "read"),
    ("list_edges", "read"),
    ("count_edges", "read"),
    ("get_outgoing_edges", "read"),
    ("get_incoming_edges", "read"),
    ("create_edge", "write"),
    ("update_edge", "write"),
    ("delete_edge", "write"),
    ("retract_edge", "write"),
];

#[tokio::test]
async fn mcp_catalog_advertises_the_nine_edge_tools() {
    let client = build_server_client(fresh_db(), make_store(), AuthMode::Required);
    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    let tools = body["result"]["tools"].as_array().expect("tools array");

    for (name, class) in PR3_TOOLS {
        let tool = tools
            .iter()
            .find(|t| t["name"] == *name)
            .unwrap_or_else(|| panic!("/mcp must advertise {name}: {body}"));
        assert!(
            tool["inputSchema"].is_object(),
            "{name} inputSchema present"
        );
        assert!(
            matches!(*class, "write" | "read"),
            "{name} class {class} is a known access class"
        );
    }
}

#[tokio::test]
async fn mcp_tools_call_get_edge_matches_http() {
    let db_http = fresh_db();
    let db_call = fresh_db();
    let (_a, _b, e_http) = seed_edge(&db_http);
    let (_a2, _b2, e_call) = seed_edge(&db_call);
    assert_eq!(e_http, e_call, "deterministic edge ids");
    let client_http = build_server_client(db_http, make_store(), AuthMode::Required);
    let client_call = build_server_client(db_call, make_store(), AuthMode::Required);

    let (_s, http_body) = get(
        &client_http,
        &format!("/edges/{}", e_http.as_u64()),
        Some(READER_TOKEN),
    )
    .await;

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": { "name": "get_edge", "arguments": { "id": e_call.as_u64() } }
    });
    let resp = client_call
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    assert_eq!(body["id"], 7);
    assert_eq!(body["result"]["isError"], false, "tools/call ok: {body}");
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    let tool_json: Value = serde_json::from_str(text).expect("tool text JSON");
    assert_eq!(
        normalize(tool_json),
        normalize(http_body),
        "tools/call get_edge must equal HTTP GET"
    );
}

#[tokio::test]
async fn mcp_tools_call_create_edge_matches_http() {
    let db_http = fresh_db();
    let db_call = fresh_db();
    let seed_nodes = |db: &Arc<AletheiaDB>| -> (NodeId, NodeId) {
        let a = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let b = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        (a, b)
    };
    let (a_http, b_http) = seed_nodes(&db_http);
    let (a_call, b_call) = seed_nodes(&db_call);
    assert_eq!((a_http, b_http), (a_call, b_call));
    let client_http = build_server_client(db_http, make_store(), AuthMode::Required);
    let client_call = build_server_client(db_call, make_store(), AuthMode::Required);

    let edge = json!({
        "source_id": a_http.as_u64(),
        "target_id": b_http.as_u64(),
        "label": "KNOWS"
    });
    let (_s, http_body) = post(&client_http, "/edges", &edge, Some(ADMIN_TOKEN)).await;

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
        "params": { "name": "create_edge", "arguments": { "body": edge } }
    });
    let resp = client_call
        .post("/mcp")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    assert_eq!(body["id"], 8);
    assert_eq!(body["result"]["isError"], false, "tools/call ok: {body}");
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    let tool_json: Value = serde_json::from_str(text).expect("tool text JSON");
    assert_eq!(
        normalize(tool_json),
        normalize(http_body),
        "tools/call create_edge must equal HTTP POST"
    );
}

#[tokio::test]
async fn mcp_write_edge_tool_requires_token() {
    let client = build_server_client(fresh_db(), make_store(), AuthMode::Required);
    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": {
            "name": "create_edge",
            "arguments": { "body": { "source_id": 1, "target_id": 2, "label": "KNOWS" } }
        }
    });
    let resp = client.post("/mcp").json(&rpc).send().await;
    assert_eq!(
        resp.status.as_u16(),
        401,
        "unauthenticated /mcp write must be rejected: {}",
        resp.text()
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Per-surface error-envelope parity.
//
// The 4 budgetable edge reads are exposed as flat-HTTP routes (`GET /edges/{id}`
// etc.) AND as `/mcp` tools. The legacy flat-HTTP router (src/http) exposes ONLY
// /status, /admin/keys[/revoke], /query — there is NO legacy flat `/edges/*` or
// `/nodes/*` endpoint. So these routes are brand-new MCP projections whose
// contract (established by PR2) is "return the MCP tool result verbatim with
// HTTP 200, in-band errors and all". The flat `AletheiaHttpError` envelope
// belongs to the ported legacy routes (PR1), not to these tools. The envelope is
// therefore the MCP nested `{"error":{code,message,retriable,details}}` shape on
// BOTH surfaces — so HTTP↔/mcp envelope parity HOLDS (they are identical). These
// tests lock that in.
// ════════════════════════════════════════════════════════════════════════════

/// Extract the tool-result JSON text from a `/mcp` tools/call response body.
fn mcp_tool_result(body: &Value) -> Value {
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    serde_json::from_str(text).expect("tool text JSON")
}

#[tokio::test]
async fn get_edge_error_envelope_identical_across_http_and_mcp() {
    // A NOT_FOUND on the read path returns the SAME MCP nested envelope on the
    // flat-HTTP surface and the /mcp tools/call surface (no flat-vs-nested
    // divergence, because there is no legacy flat `/edges/{id}` to match).
    let db = fresh_db();
    let client = build_server_client(db, make_store(), AuthMode::Required);

    let (status, http_err) = get(&client, "/edges/999", Some(READER_TOKEN)).await;
    assert_eq!(status, 200, "MCP method returns 200 with in-band error");
    assert_eq!(http_err["error"]["code"], "NOT_FOUND");
    // It is the MCP nested envelope, not a flat AletheiaHttpError body.
    assert!(
        http_err["error"]["retriable"].is_boolean(),
        "nested MCP error envelope on HTTP: {http_err}"
    );

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 21,
        "method": "tools/call",
        "params": { "name": "get_edge", "arguments": { "id": 999 } }
    });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    let mcp_err = mcp_tool_result(&body);

    assert_eq!(
        normalize(http_err),
        normalize(mcp_err),
        "the error envelope must be identical on the HTTP and /mcp surfaces"
    );
}

#[tokio::test]
async fn too_small_budget_invalid_argument_envelope_across_surfaces() {
    // The #3353 too-small-budget INVALID_ARGUMENT also rides the same MCP nested
    // envelope on both surfaces (byte 1 is below any minimal rung).
    let db = fresh_db();
    let (src, _edges) = seed_fanout(&db, 3);
    let client = build_server_client(db, make_store(), AuthMode::Required);

    let (status, http_err) = get(
        &client,
        &format!(
            "/nodes/{}/edges/outgoing?max_response_bytes=1",
            src.as_u64()
        ),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(http_err["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(http_err["error"]["retriable"], false);

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 22,
        "method": "tools/call",
        "params": {
            "name": "get_outgoing_edges",
            "arguments": { "node_id": src.as_u64(), "query": { "max_response_bytes": 1 } }
        }
    });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    let mcp_err = mcp_tool_result(&body);

    assert_eq!(
        normalize(http_err),
        normalize(mcp_err),
        "too-small-budget INVALID_ARGUMENT envelope must be identical on both surfaces"
    );
}
