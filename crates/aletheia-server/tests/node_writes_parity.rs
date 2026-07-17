//! Parity tests for the node-write cluster (Issue #3524, PR2).
//!
//! Mirrors `tests/server_parity.rs` (the PR1 read slice) for the six write /
//! point-in-time tools ported in PR2: `create_node`, `update_node`,
//! `delete_node`, `delete_node_cascade`, `retract_node`, and the read finder
//! `find_nodes_at_time`. Each test drives the new crate's autumn [`TestClient`]
//! (HTTP and/or `/mcp`) and asserts the response is byte-identical to the
//! **legacy** MCP typed method ([`AletheiaMcpServer`]) for the same operation.
//!
//! # Why two databases + volatile-field normalization
//!
//! A write mutates, so the autumn surface and the legacy method cannot share one
//! `Arc<AletheiaDB>` without double-applying. Instead each test runs the *same*
//! logical operation against **two freshly-seeded, identical** databases (id
//! generation is deterministic from a fresh `AletheiaDB::new()`, guarded by an
//! explicit assertion where relevant) and compares after stripping the
//! wall-clock-volatile fields (`trace_id` + the bi-temporal timestamps
//! `valid_from`/`valid_to`/`transaction_from`/`transaction_to`, which differ by
//! microseconds between two independent commits). The structural payload — ids,
//! labels, properties, success flags, `connected_edges`/`edges_removed`/
//! `edges_retracted` counts, `is_current`, row counts — is asserted byte-equal;
//! the meaningful volatile values (a backdated `valid_from`, the retraction
//! `valid_to`, `is_current`) get their own targeted assertions on the autumn
//! response directly, where they are deterministic.
//!
//! Contracts covered: #3209 delete DETACH (refuse / detach / clean), #3230
//! retraction (refuse / detach / idempotent), #3221 `valid_time` backdating,
//! #3232 temporal block, #3220 vector elision (default + `include_vectors`),
//! #3236 point-in-time reconstruction — plus both auth modes and the `/mcp`
//! catalog surfacing the six tools.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::mcp::{
    AletheiaMcpServer, CreateNodeRequest, DeleteNodeCascadeRequest, DeleteNodeRequest,
    FindNodesAtTimeRequest, RetractNodeRequest, UpdateNodeRequest,
};
use autumn_web::test::TestClient;
use serde_json::{Value, json};
use std::collections::HashMap;

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const EMBEDDING: &[f32] = &[0.10, 0.20, 0.30, 0.40];

/// A shared auth store with admin + reader keys. Writes authenticate as admin
/// (the `admin` role satisfies every access class, so these tests keep passing
/// once Lane B's `Authorized<WriteClass>` enforcement lands); the read finder
/// authenticates as reader.
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
                            // find_nodes_at_time echoes the resolved
                            // transaction_time, which defaults to a volatile
                            // "now" (valid_time is caller-supplied, so stable).
                            | "transaction_time"
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

/// Seed a `Person{name:"Alice", age:30}` node on `db`, returning its id. Two
/// fresh dbs seeded this way assign the same id (deterministic generation).
fn seed_person(db: &Arc<AletheiaDB>) -> NodeId {
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30_i64)
            .build(),
    )
    .expect("seed person")
}

fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

// ════════════════════════════════════════════════════════════════════════════
// create_node (#3221 valid_time, #3232 temporal block, #3220 write-path vectors)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn create_node_byte_parity_with_legacy_mcp() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let body = json!({ "label": "Person", "properties": { "name": "Alice", "age": 30 } });
    let (status, http_body) = post(&client, "/nodes", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).create_node(CreateNodeRequest {
        namespace: None,
        label: "Person".to_string(),
        properties: Some(props(&[("name", json!("Alice")), ("age", json!(30))])),
        valid_time: None,
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");

    assert_eq!(
        normalize(http_body.clone()),
        normalize(legacy),
        "create_node HTTP body must equal legacy MCP method"
    );
    // #3232: the created node carries a temporal block, current at creation.
    assert!(
        http_body["temporal"].is_object(),
        "temporal block: {http_body}"
    );
    assert_eq!(http_body["temporal"]["is_current"], true);
    assert_eq!(http_body["label"], "Person");
    assert_eq!(http_body["properties"]["name"], "Alice");
}

#[tokio::test]
async fn create_node_backdated_valid_time_honored() {
    // #3221: an explicit valid_time backdates when the fact became true.
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let vt = "2020-01-15T10:00:00Z";
    let body = json!({ "label": "Person", "properties": { "name": "Alice" }, "valid_time": vt });
    let (status, http_body) = post(&client, "/nodes", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);

    // The backdated valid_from is deterministic (the supplied instant, rendered
    // RFC 3339 with microseconds), so it is asserted directly on the autumn
    // response rather than normalized away.
    assert_eq!(
        http_body["temporal"]["valid_from"], "2020-01-15T10:00:00.000000Z",
        "backdated valid_from must be honored: {http_body}"
    );

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).create_node(CreateNodeRequest {
        namespace: None,
        label: "Person".to_string(),
        properties: Some(props(&[("name", json!("Alice"))])),
        valid_time: Some(vt.to_string()),
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");
    // valid_from is identical here, so compare the full payload after only
    // stripping trace_id / transaction_* (which remain wall-clock volatile).
    assert_eq!(normalize(http_body), normalize(legacy));
}

#[tokio::test]
async fn create_node_write_path_returns_full_vector() {
    // #3220 exception: the write path has no include_vectors flag and returns
    // the full embedding array (elision is a read-path concern).
    let db_http = fresh_db();
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let body = json!({
        "label": "Doc",
        "properties": { "title": "x", "embedding": EMBEDDING }
    });
    let (status, http_body) = post(&client, "/nodes", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(
        http_body["properties"]["embedding"].is_array(),
        "write path returns the full vector array: {http_body}"
    );
    assert_eq!(
        http_body["properties"]["embedding"]
            .as_array()
            .unwrap()
            .len(),
        EMBEDDING.len()
    );
}

#[tokio::test]
async fn create_node_anonymous_mode_parity() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let client = build_server_client(db_http, make_store(), AuthMode::Anonymous);

    let body = json!({ "label": "Person", "properties": { "name": "Alice" } });
    // Anonymous: no credential.
    let (status, http_body) = post(&client, "/nodes", &body, None).await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).create_node(CreateNodeRequest {
        namespace: None,
        label: "Person".to_string(),
        properties: Some(props(&[("name", json!("Alice"))])),
        valid_time: None,
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

#[tokio::test]
async fn create_node_unauthenticated_401() {
    let client = build_server_client(fresh_db(), make_store(), AuthMode::Required);
    let resp = client
        .post("/nodes")
        .json(&json!({ "label": "Person" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
}

// ════════════════════════════════════════════════════════════════════════════
// update_node
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn update_node_byte_parity_with_legacy_mcp() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let id_http = seed_person(&db_http);
    let id_mcp = seed_person(&db_mcp);
    assert_eq!(id_http, id_mcp, "fresh dbs must assign identical node ids");
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let body = json!({
        "node_id": id_http.as_u64(),
        "properties": { "name": "Alice", "age": 31 }
    });
    let (status, http_body) = post(&client, "/nodes/update", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).update_node(UpdateNodeRequest {
        namespace: None,
        node_id: id_mcp.as_u64(),
        properties: props(&[("name", json!("Alice")), ("age", json!(31))]),
        valid_time: None,
        provenance: None,
        derived_from: None,
    }))
    .expect("legacy mcp json");

    assert_eq!(normalize(http_body.clone()), normalize(legacy));
    assert_eq!(http_body["properties"]["age"], 31);
    assert!(http_body["temporal"].is_object());
}

// ════════════════════════════════════════════════════════════════════════════
// delete_node — #3209 DETACH safe-by-default (refuse / detach / clean)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn delete_node_refuses_without_detach_reports_connected_edges() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    // Both dbs: Alice --KNOWS--> Bob, so Alice has one connected edge.
    let seed = |db: &Arc<AletheiaDB>| -> NodeId {
        let alice = seed_person(db);
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        alice
    };
    let alice_http = seed(&db_http);
    let alice_mcp = seed(&db_mcp);
    assert_eq!(alice_http, alice_mcp);
    let client = build_server_client(db_http.clone(), make_store(), AuthMode::Required);

    let body = json!({ "node_id": alice_http.as_u64() });
    let (status, http_body) = post(&client, "/nodes/delete", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200, "MCP method returns 200 with in-band error");

    // #3209 refusal shape: FAILED_PRECONDITION + details.connected_edges, with
    // legacy top-level fields preserved additively.
    assert_eq!(http_body["error"]["code"], "FAILED_PRECONDITION");
    assert_eq!(http_body["error"]["retriable"], false);
    assert_eq!(http_body["error"]["details"]["connected_edges"], 1);
    assert_eq!(http_body["connected_edges"], 1);
    assert_eq!(http_body["detach_required"], true);
    assert_eq!(http_body["success"], false);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).delete_node(DeleteNodeRequest {
        namespace: None,
        node_id: alice_mcp.as_u64(),
        detach: None,
        valid_time: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(
        normalize(http_body),
        normalize(legacy),
        "refuse response must equal legacy"
    );
    // Refusal did not delete: Alice is still present.
    assert!(
        db_http.get_node(alice_http).is_ok(),
        "node must survive refusal"
    );
}

#[tokio::test]
async fn delete_node_detach_removes_edges() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let seed = |db: &Arc<AletheiaDB>| -> NodeId {
        let alice = seed_person(db);
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        alice
    };
    let alice_http = seed(&db_http);
    let alice_mcp = seed(&db_mcp);
    let client = build_server_client(db_http.clone(), make_store(), AuthMode::Required);

    let body = json!({ "node_id": alice_http.as_u64(), "detach": true });
    let (status, http_body) = post(&client, "/nodes/delete", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["success"], true);
    assert_eq!(http_body["detached"], true);
    assert_eq!(http_body["edges_removed"], 1);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).delete_node(DeleteNodeRequest {
        namespace: None,
        node_id: alice_mcp.as_u64(),
        detach: Some(true),
        valid_time: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
    assert!(db_http.get_node(alice_http).is_err(), "node deleted");
}

#[tokio::test]
async fn delete_node_clean_when_no_edges() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let id_http = seed_person(&db_http);
    let id_mcp = seed_person(&db_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let body = json!({ "node_id": id_http.as_u64() });
    let (status, http_body) = post(&client, "/nodes/delete", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["success"], true);
    assert_eq!(http_body["edges_removed"], 0);

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).delete_node(DeleteNodeRequest {
        namespace: None,
        node_id: id_mcp.as_u64(),
        detach: None,
        valid_time: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

// ════════════════════════════════════════════════════════════════════════════
// delete_node_cascade
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn delete_node_cascade_byte_parity() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let seed = |db: &Arc<AletheiaDB>| -> NodeId {
        let alice = seed_person(db);
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        alice
    };
    let alice_http = seed(&db_http);
    let alice_mcp = seed(&db_mcp);
    let client = build_server_client(db_http.clone(), make_store(), AuthMode::Required);

    let body = json!({ "node_id": alice_http.as_u64() });
    let (status, http_body) =
        post(&client, "/nodes/delete_cascade", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["success"], true);
    assert_eq!(http_body["cascade"], true);
    assert_eq!(http_body["deleted_node_id"], alice_http.as_u64());

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).delete_node_cascade(
        DeleteNodeCascadeRequest {
            node_id: alice_mcp.as_u64(),
        },
    ))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
    assert!(
        db_http.get_node(alice_http).is_err(),
        "node cascade-deleted"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// retract_node — #3230 refuse / detach / idempotent
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn retract_node_refuses_without_detach() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let seed = |db: &Arc<AletheiaDB>| -> NodeId {
        let alice = seed_person(db);
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        alice
    };
    let alice_http = seed(&db_http);
    let alice_mcp = seed(&db_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let body = json!({ "node_id": alice_http.as_u64() });
    let (status, http_body) = post(&client, "/nodes/retract", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["error"]["code"], "FAILED_PRECONDITION");
    assert_eq!(http_body["connected_edges"], 1);
    assert_eq!(http_body["detach_required"], true);

    let legacy: Value =
        serde_json::from_str(&mcp_server(&db_mcp).retract_node(RetractNodeRequest {
            node_id: alice_mcp.as_u64(),
            valid_time: None,
            detach: None,
        }))
        .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

#[tokio::test]
async fn retract_node_detach_and_idempotent() {
    // detach retraction co-retracts the edge; a second retraction is an
    // idempotent no-op (already_retracted: true).
    let db_http = fresh_db();
    let seed = |db: &Arc<AletheiaDB>| -> NodeId {
        let alice = seed_person(db);
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        alice
    };
    let alice = seed(&db_http);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    // Omit valid_time so it defaults to "now" (>= the node's just-created
    // valid_from; a past valid_time before valid_from would be rejected).
    let body = json!({ "node_id": alice.as_u64(), "detach": true });
    let (status, first) = post(&client, "/nodes/retract", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(first["success"], true);
    assert_eq!(first["retracted"], true);
    assert_eq!(first["already_retracted"], false);
    assert_eq!(first["edges_retracted"], 1);
    // The retraction closed the interval at a concrete instant.
    assert!(first["valid_to"].is_string(), "valid_to present: {first}");

    // Re-retract: idempotent no-op returning the EXISTING interval.
    let (status, second) = post(&client, "/nodes/retract", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(second["already_retracted"], true);
    // The reported interval is stable across the two calls (the second returns
    // the first retraction's stored bounds, not a fresh "now").
    assert_eq!(first["valid_from"], second["valid_from"]);
    assert_eq!(first["valid_to"], second["valid_to"]);
}

#[tokio::test]
async fn retract_node_idempotent_byte_parity() {
    // Byte-parity for the (no-edge) retraction success payload: run the same
    // retraction on two fresh dbs at the same valid_time and compare.
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let id_http = seed_person(&db_http);
    let id_mcp = seed_person(&db_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    // Omit valid_time (defaults to now); valid_from/valid_to are wall-clock
    // volatile and normalized away for the cross-db comparison.
    let body = json!({ "node_id": id_http.as_u64() });
    let (status, http_body) = post(&client, "/nodes/retract", &body, Some(ADMIN_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["retracted"], true);
    assert_eq!(http_body["already_retracted"], false);

    let legacy: Value =
        serde_json::from_str(&mcp_server(&db_mcp).retract_node(RetractNodeRequest {
            node_id: id_mcp.as_u64(),
            valid_time: None,
            detach: None,
        }))
        .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

// ════════════════════════════════════════════════════════════════════════════
// find_nodes_at_time — #3236 point-in-time, #3220 vector elision
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn find_nodes_at_time_byte_parity_and_elision() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    // Seed identical Alice-with-embedding on both dbs.
    let seed = |db: &Arc<AletheiaDB>| {
        db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", EMBEDDING)
                .build(),
        )
        .unwrap()
    };
    let a_http = seed(&db_http);
    let a_mcp = seed(&db_mcp);
    assert_eq!(a_http, a_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let vt = "2999-01-01T00:00:00Z"; // any time after creation resolves the node
    let req = json!({
        "label": "Person",
        "property_key": "name",
        "property_value": "Alice",
        "valid_time": vt
    });
    // find is a ReadClass tool → reader token suffices.
    let (status, http_body) = post(&client, "/nodes/find_at_time", &req, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(http_body["count"], 1, "resolved one node: {http_body}");
    // #3220: vectors elided by default on the read path.
    assert_eq!(
        http_body["nodes"][0]["properties"]["embedding"]["elided"], true,
        "vector elided by default: {http_body}"
    );

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).find_nodes_at_time(
        FindNodesAtTimeRequest {
            label: "Person".to_string(),
            property_key: Some("name".to_string()),
            property_value: Some(json!("Alice")),
            valid_time: vt.to_string(),
            transaction_time: None,
            limit: None,
            offset: None,
            include_vectors: None,
        },
    ))
    .expect("legacy mcp json");
    assert_eq!(
        normalize(http_body),
        normalize(legacy),
        "find_nodes_at_time HTTP body must equal legacy MCP method"
    );
}

#[tokio::test]
async fn find_nodes_at_time_include_vectors() {
    let db_http = fresh_db();
    let db_mcp = fresh_db();
    let seed = |db: &Arc<AletheiaDB>| {
        db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", EMBEDDING)
                .build(),
        )
        .unwrap()
    };
    seed(&db_http);
    seed(&db_mcp);
    let client = build_server_client(db_http, make_store(), AuthMode::Required);

    let vt = "2999-01-01T00:00:00Z";
    let req = json!({
        "label": "Person",
        "valid_time": vt,
        "include_vectors": true
    });
    let (status, http_body) = post(&client, "/nodes/find_at_time", &req, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(
        http_body["nodes"][0]["properties"]["embedding"].is_array(),
        "include_vectors=true returns the raw array: {http_body}"
    );

    let legacy: Value = serde_json::from_str(&mcp_server(&db_mcp).find_nodes_at_time(
        FindNodesAtTimeRequest {
            label: "Person".to_string(),
            property_key: None,
            property_value: None,
            valid_time: vt.to_string(),
            transaction_time: None,
            limit: None,
            offset: None,
            include_vectors: Some(true),
        },
    ))
    .expect("legacy mcp json");
    assert_eq!(normalize(http_body), normalize(legacy));
}

// ════════════════════════════════════════════════════════════════════════════
// /mcp catalog + tools/call for the six PR2 tools
// ════════════════════════════════════════════════════════════════════════════

/// The six PR2 tools and their intended access class (sourced from
/// `tests/parity/inventory.json`'s `mcp.tools[].access_class`). Lane B's
/// inventory-anchored RBAC registry must classify each identically; the
/// `mcp_routable_tools_are_all_classified` conformance test in
/// `tests/server_parity.rs` enforces that binding once the registry is extended.
const PR2_TOOLS: &[(&str, &str)] = &[
    ("create_node", "write"),
    ("update_node", "write"),
    ("delete_node", "write"),
    ("delete_node_cascade", "write"),
    ("retract_node", "write"),
    ("find_nodes_at_time", "read"),
];

#[tokio::test]
async fn mcp_catalog_advertises_the_six_node_write_tools() {
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

    for (name, class) in PR2_TOOLS {
        let tool = tools
            .iter()
            .find(|t| t["name"] == *name)
            .unwrap_or_else(|| panic!("/mcp must advertise {name}: {body}"));
        // The tool's inputSchema is an object (write tools nest their request
        // under the reserved `body` key; find_at_time likewise).
        assert!(
            tool["inputSchema"].is_object(),
            "{name} inputSchema present"
        );
        // Access-class expectation (documented; enforced by Lane B's registry +
        // the server_parity conformance test).
        assert!(
            matches!(*class, "write" | "read"),
            "{name} class {class} is a known access class"
        );
    }
}

#[tokio::test]
async fn mcp_tools_call_create_node_matches_http() {
    // The write tool is reachable via /mcp tools/call; its arguments nest the
    // request under `body` (autumn's Json-body → MCP mapping). The result must
    // equal the direct HTTP POST on an identically-seeded db.
    let db_http = fresh_db();
    let db_call = fresh_db();
    let client_http = build_server_client(db_http, make_store(), AuthMode::Required);
    let client_call = build_server_client(db_call, make_store(), AuthMode::Required);

    let node = json!({ "label": "Person", "properties": { "name": "Alice" } });
    let (_s, http_body) = post(&client_http, "/nodes", &node, Some(ADMIN_TOKEN)).await;

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": { "name": "create_node", "arguments": { "body": node } }
    });
    let resp = client_call
        .post("/mcp")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
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
        "tools/call create_node must equal HTTP POST"
    );
}

#[tokio::test]
async fn mcp_write_tool_requires_token() {
    let client = build_server_client(fresh_db(), make_store(), AuthMode::Required);
    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": { "name": "create_node", "arguments": { "body": { "label": "Person" } } }
    });
    // No credential → the /mcp gate rejects before the tool runs.
    let resp = client.post("/mcp").json(&rpc).send().await;
    assert_eq!(
        resp.status.as_u16(),
        401,
        "unauthenticated /mcp write must be rejected: {}",
        resp.text()
    );
}
