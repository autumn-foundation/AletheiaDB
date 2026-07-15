//! Parity + behavior tests for the CONSTRAINTS / INDEX-ADMIN + LINEAGE +
//! AUDIT-CHAIN cluster (Issue #3524, PR7 — the FINAL tool slice, completing all
//! 46 MCP tools on the autumn surface): `enable_vector_index`,
//! `enable_unique_constraint`, `list_unique_constraints`, `lineage_upstream`,
//! `lineage_downstream`, `audit_export`, `verify_chain`, `export_chain_head`.
//!
//! Each test drives the new crate's autumn [`TestClient`] and asserts the
//! response equals the **legacy MCP surface** — byte-for-byte where the payload
//! is deterministic (`trace_id` + wall-clock-volatile bi-temporal timestamps
//! normalized), and via an offline-verify roundtrip for the one signed
//! (non-deterministic) payload, `audit_export`. Reads compare over the same
//! `Arc<AletheiaDB>` (no mutation); the writes (`enable_*`) compare over an
//! identically-seeded twin DB.
//!
//! Access-class cross-check (verified BY HAND against
//! `tests/parity/inventory.json`): `enable_vector_index`=**write**,
//! `enable_unique_constraint`=**write**, `list_unique_constraints`=read,
//! `lineage_upstream`=read, `lineage_downstream`=read, `audit_export`=read,
//! `verify_chain`=read, `export_chain_head`=read. Every handler's `Authorized<C>`
//! marker equals its inventory `access_class`; `write_auth_gating_*` proves the
//! two writes reject a reader token.
//!
//! #3220 (vector elision) / #3232 (temporal block) are N/A for this cluster: no
//! tool here returns an entity-property read payload (lineage entries carry only
//! version-pinned refs + depth + status; the chain tools return digests /
//! signed artifacts; `enable_*` return `{success,...}`), so there is no vector
//! or temporal block to assert.

use std::sync::Mutex;
use std::sync::{Arc, OnceLock};

use aletheia_server::build_server_client;
use aletheia_server::edge_tools::DISPATCH_ROUTED_READ_TOOLS;
use aletheia_server::security::rbac;
use aletheiadb::audit::{AuditSigningKey, verify_json_bytes};
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Role};
use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::core::NodeId;
use aletheiadb::mcp::AletheiaMcpServer;
use aletheiadb::provenance_chain::{ChainConfig, ChainFsyncMode};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";

/// A 32-byte Ed25519 seed for the audit-export signing-key env var.
const AUDIT_SEED: [u8; 32] = [23u8; 32];

/// Serializes the two tests that mutate the process-global
/// `ALETHEIADB_AUDIT_SIGNING_KEY` env var (this crate has no `serial_test`
/// dev-dep; a static mutex is sufficient because cargo runs each integration
/// test file as its own binary, so the only in-process racers are here).
fn audit_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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

fn fresh_db() -> Arc<AletheiaDB> {
    Arc::new(AletheiaDB::new().expect("create db"))
}

/// A durable, chain-enabled database rooted at a tempdir. Returns the guard so
/// the data dir outlives the DB.
fn chain_enabled_db() -> (tempfile::TempDir, Arc<AletheiaDB>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.path().join("wal"))
                .build(),
        )
        .chain(ChainConfig {
            enabled: true,
            fsync: ChainFsyncMode::Batched,
            dir: None,
        })
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("chain-enabled db");
    (dir, Arc::new(db))
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

/// The current version id of a node (for building version-pinned lineage refs).
fn node_ver(db: &Arc<AletheiaDB>, id: u64) -> u64 {
    db.node_lineage_ref(NodeId::new(id).expect("valid node id"))
        .expect("node has a current version")
        .version
        .as_u64()
}

/// Seed a lineage subgraph on `db` via the legacy MCP create_node path: two
/// source docs A and B, and a summary C derived from both. Returns
/// `(a_id, a_ver, b_id, b_ver, c_id, c_ver)`.
fn seed_lineage(
    server: &AletheiaMcpServer,
    db: &Arc<AletheiaDB>,
) -> (u64, u64, u64, u64, u64, u64) {
    let a: Value = serde_json::from_str(&server.dispatch_tool_json(
        "create_node",
        json!({"label": "Doc", "properties": {"t": "A"}}),
    ))
    .expect("create A");
    let a_id = a["id"].as_u64().expect("a id");
    let a_ver = node_ver(db, a_id);

    let b: Value = serde_json::from_str(&server.dispatch_tool_json(
        "create_node",
        json!({"label": "Doc", "properties": {"t": "B"}}),
    ))
    .expect("create B");
    let b_id = b["id"].as_u64().expect("b id");
    let b_ver = node_ver(db, b_id);

    let c: Value = serde_json::from_str(&server.dispatch_tool_json(
        "create_node",
        json!({
            "label": "Summary",
            "properties": {"t": "A+B"},
            "derived_from": [
                {"entity_kind": "node", "id": a_id, "version": a_ver},
                {"entity_kind": "node", "id": b_id, "version": b_ver},
            ],
        }),
    ))
    .expect("create C");
    let c_id = c["id"].as_u64().expect("c id");
    let c_ver = node_ver(db, c_id);

    (a_id, a_ver, b_id, b_ver, c_id, c_ver)
}

// ════════════════════════════════════════════════════════════════════════════
// list_unique_constraints + lineage_upstream/downstream: byte-parity vs the
// legacy MCP surface over the SAME DB (reads — no mutation), in both auth modes.
// ════════════════════════════════════════════════════════════════════════════

async fn assert_reads_parity(mode: AuthMode, token: Option<&'static str>) {
    let db = fresh_db();
    let server = mcp_server(&db);
    // Seed a constraint + a lineage subgraph.
    db.unique_constraint("Person", "email")
        .enable()
        .expect("enable constraint");
    let (a_id, a_ver, _b_id, _b_ver, c_id, c_ver) = seed_lineage(&server, &db);

    let client = build_server_client(db.clone(), make_store(), mode);

    // 1. list_unique_constraints (GET) — typed-method reference.
    let (s, autumn) = get(&client, "/constraints/unique", token).await;
    assert_eq!(s, 200, "list_unique_constraints: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("list_unique_constraints", json!({})))
            .expect("legacy list_unique_constraints json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "list_unique_constraints parity"
    );
    assert_eq!(autumn["count"], json!(1), "one constraint: {autumn}");

    // 2. lineage_upstream (POST) of the summary C — both docs at depth 1.
    let up_req = json!({"entity_kind": "node", "id": c_id, "version": c_ver});
    let (s, autumn) = post(&client, "/lineage/upstream", &up_req, token).await;
    assert_eq!(s, 200, "lineage_upstream: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("lineage_upstream", up_req.clone()))
            .expect("legacy lineage_upstream json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "lineage_upstream parity"
    );
    assert_eq!(autumn["direction"], "upstream");
    assert_eq!(autumn["count"].as_u64(), Some(2), "two upstream: {autumn}");
    assert_eq!(autumn["has_more"], json!(false));

    // 3. lineage_downstream (POST) of doc A — reaches the summary C.
    let down_req = json!({"entity_kind": "node", "id": a_id, "version": a_ver});
    let (s, autumn) = post(&client, "/lineage/downstream", &down_req, token).await;
    assert_eq!(s, 200, "lineage_downstream: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("lineage_downstream", down_req.clone()))
            .expect("legacy lineage_downstream json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "lineage_downstream parity"
    );
    assert_eq!(autumn["direction"], "downstream");
    assert_eq!(autumn["entries"][0]["id"].as_u64(), Some(c_id));
}

#[tokio::test]
async fn reads_byte_parity_required_mode() {
    assert_reads_parity(AuthMode::Required, Some(READER_TOKEN)).await;
}

#[tokio::test]
async fn reads_byte_parity_anonymous_mode() {
    assert_reads_parity(AuthMode::Anonymous, None).await;
}

// ════════════════════════════════════════════════════════════════════════════
// lineage: #3226 pagination (has_more / next_offset) + #3234 error kind.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lineage_upstream_pagination_has_more_and_next_offset() {
    let db = fresh_db();
    let server = mcp_server(&db);
    let (a_id, a_ver, b_id, b_ver, c_id, c_ver) = seed_lineage(&server, &db);
    let _ = (a_id, a_ver, b_id, b_ver);
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // C has two upstream parents; limit=1 → one entry, has_more=true, next_offset=1.
    let req = json!({"entity_kind": "node", "id": c_id, "version": c_ver, "limit": 1});
    let (s, autumn) = post(&client, "/lineage/upstream", &req, Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "paged upstream: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("lineage_upstream", req.clone()))
            .expect("legacy paged json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "lineage pagination parity"
    );
    assert_eq!(autumn["count"].as_u64(), Some(1), "one per page: {autumn}");
    assert_eq!(autumn["has_more"], json!(true), "more pages: {autumn}");
    assert_eq!(
        autumn["next_offset"].as_u64(),
        Some(1),
        "next_offset advances: {autumn}"
    );
}

#[tokio::test]
async fn lineage_upstream_bad_entity_kind_is_invalid_argument() {
    let db = fresh_db();
    let client = build_server_client(db, make_store(), AuthMode::Required);

    // A structurally-invalid root ref (bad entity kind) is the #3234
    // INVALID_ARGUMENT the query tool preserves. (The write-path dangling-ref
    // NOT_FOUND lives on create_node/create_edge's `derived_from`, PR2/PR3.)
    let req = json!({"entity_kind": "widget", "id": 1, "version": 1});
    let (s, autumn) = post(&client, "/lineage/upstream", &req, Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "in-band error still 200: {autumn}");
    assert_eq!(
        autumn["error"]["code"], "INVALID_ARGUMENT",
        "bad entity kind → INVALID_ARGUMENT: {autumn}"
    );
    assert_eq!(autumn["error"]["retriable"], json!(false));
}

// ════════════════════════════════════════════════════════════════════════════
// enable_vector_index / enable_unique_constraint: WriteClass twin-DB parity +
// idempotency + write-auth gating (a reader token is denied).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn enable_vector_index_twin_db_parity_and_idempotent() {
    let a = fresh_db();
    let b = fresh_db();
    let client = build_server_client(a.clone(), make_store(), AuthMode::Required);

    let req = json!({"property_name": "embedding", "dimensions": 4, "distance_metric": "cosine"});
    let (s, autumn) = post(&client, "/vector/indexes", &req, Some(ADMIN_TOKEN)).await;
    assert_eq!(s, 200, "enable_vector_index: {autumn}");
    let legacy: Value = serde_json::from_str(
        &mcp_server(&b).dispatch_tool_json("enable_vector_index", req.clone()),
    )
    .expect("legacy json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "enable_vector_index parity"
    );
    assert_eq!(autumn["success"], json!(true), "enabled: {autumn}");
    assert_eq!(autumn["property_name"], "embedding");

    // Re-enable: same index, byte-parity against the twin re-enabling too.
    let (s, autumn2) = post(&client, "/vector/indexes", &req, Some(ADMIN_TOKEN)).await;
    assert_eq!(s, 200, "re-enable: {autumn2}");
    let legacy2: Value = serde_json::from_str(
        &mcp_server(&b).dispatch_tool_json("enable_vector_index", req.clone()),
    )
    .expect("legacy re-enable json");
    assert_eq!(
        normalize(autumn2),
        normalize(legacy2),
        "enable_vector_index idempotency parity"
    );
}

#[tokio::test]
async fn enable_unique_constraint_twin_db_parity_and_idempotent() {
    let a = fresh_db();
    let b = fresh_db();
    let client = build_server_client(a.clone(), make_store(), AuthMode::Required);

    let req = json!({"label": "Person", "property": "email"});
    let (s, autumn) = post(&client, "/constraints/unique", &req, Some(ADMIN_TOKEN)).await;
    assert_eq!(s, 200, "enable_unique_constraint: {autumn}");
    let legacy: Value = serde_json::from_str(
        &mcp_server(&b).dispatch_tool_json("enable_unique_constraint", req.clone()),
    )
    .expect("legacy json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "enable_unique_constraint parity"
    );
    assert_eq!(autumn["success"], json!(true), "enabled: {autumn}");
    assert_eq!(autumn["label"], "Person");
    assert_eq!(autumn["property"], "email");

    // The constraint is now visible via list_unique_constraints on db `a`.
    let (s, list) = get(&client, "/constraints/unique", Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "list after enable: {list}");
    assert_eq!(list["count"], json!(1), "constraint listed: {list}");

    // Idempotent re-enable parity.
    let (s, autumn2) = post(&client, "/constraints/unique", &req, Some(ADMIN_TOKEN)).await;
    assert_eq!(s, 200, "re-enable: {autumn2}");
    let legacy2: Value = serde_json::from_str(
        &mcp_server(&b).dispatch_tool_json("enable_unique_constraint", req.clone()),
    )
    .expect("legacy re-enable json");
    assert_eq!(
        normalize(autumn2),
        normalize(legacy2),
        "enable_unique_constraint idempotency parity"
    );
}

#[tokio::test]
async fn write_auth_gating_reader_denied_on_enables() {
    // The two enable_* tools are WriteClass: a reader token (grants only Read)
    // must be denied (403 PERMISSION_DENIED) — proving the handler class is
    // Write, not Read.
    let db = fresh_db();
    let client = build_server_client(db, make_store(), AuthMode::Required);

    let (s, body) = post(
        &client,
        "/vector/indexes",
        &json!({"property_name": "embedding", "dimensions": 4}),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(s, 403, "reader denied enable_vector_index: {body}");
    assert_eq!(body["code"], "PERMISSION_DENIED", "denial code: {body}");

    let (s, body) = post(
        &client,
        "/constraints/unique",
        &json!({"label": "Person", "property": "email"}),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(s, 403, "reader denied enable_unique_constraint: {body}");
    assert_eq!(body["code"], "PERMISSION_DENIED", "denial code: {body}");
}

// ════════════════════════════════════════════════════════════════════════════
// verify_chain / export_chain_head: provenance-hash-chain parity over a
// chain-enabled DB (same DB — reads), plus the disabled-chain FAILED_PRECONDITION.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn verify_and_export_chain_parity_on_enabled_chain() {
    let (_guard, db) = chain_enabled_db();
    let server = mcp_server(&db);
    // Seal real transactions so the chain has content.
    for _ in 0..5 {
        db.create_node("Person", PropertyMapBuilder::new().build())
            .expect("seed node");
    }
    let client = build_server_client(db.clone(), make_store(), AuthMode::Required);

    // export_chain_head (GET) — deterministic head digest on the same DB.
    let (s, head) = get(&client, "/chain/head", Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "export_chain_head: {head}");
    let legacy_head: Value =
        serde_json::from_str(&server.dispatch_tool_json("export_chain_head", json!({})))
            .expect("legacy head json");
    assert_eq!(
        normalize(head.clone()),
        normalize(legacy_head),
        "export_chain_head parity"
    );
    assert!(head["digest"].as_str().is_some(), "head has digest: {head}");

    // verify_chain (POST, full) — passes, deterministic result on the same DB.
    let (s, full) = post(&client, "/chain/verify", &json!({}), Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "verify_chain full: {full}");
    let legacy_full: Value =
        serde_json::from_str(&server.dispatch_tool_json("verify_chain", json!({})))
            .expect("legacy full json");
    assert_eq!(
        normalize(full.clone()),
        normalize(legacy_full),
        "verify_chain full parity"
    );
    assert_eq!(full["passed"], json!(true), "full verify passes: {full}");
    assert_eq!(full["scope"], "full");

    // verify_chain against the exported head — anchor-extension mode passes.
    let against = json!({ "against": head.clone() });
    let (s, anchor) = post(&client, "/chain/verify", &against, Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "verify_chain anchor: {anchor}");
    let legacy_anchor: Value =
        serde_json::from_str(&server.dispatch_tool_json("verify_chain", against.clone()))
            .expect("legacy anchor json");
    assert_eq!(
        normalize(anchor.clone()),
        normalize(legacy_anchor),
        "verify_chain anchor parity"
    );
    assert_eq!(anchor["scope"], "anchor", "anchor scope: {anchor}");
    assert_eq!(anchor["passed"], json!(true));
}

#[tokio::test]
async fn verify_and_export_chain_disabled_is_failed_precondition() {
    // The default (non-chain) DB → both tools return a structured
    // FAILED_PRECONDITION (never a silent pass / fabricated head).
    let db = fresh_db();
    let client = build_server_client(db, make_store(), AuthMode::Required);

    let (s, v) = post(&client, "/chain/verify", &json!({}), Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "verify (disabled) in-band error 200: {v}");
    assert_eq!(
        v["error"]["code"], "FAILED_PRECONDITION",
        "verify disabled: {v}"
    );
    assert_eq!(v["error"]["retriable"], json!(false));

    let (s, e) = get(&client, "/chain/head", Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "export (disabled) in-band error 200: {e}");
    assert_eq!(
        e["error"]["code"], "FAILED_PRECONDITION",
        "export disabled: {e}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// audit_export: signed-artifact roundtrip (offline-verify) + the missing-key
// FAILED_PRECONDITION. The signed payload embeds volatile export timestamps, so
// a byte compare is not meaningful; instead we verify the artifact offline
// against the operator public key exactly as the legacy tool guarantees.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
// The env-var serialization lock intentionally spans the request `.await`: the
// process-global `ALETHEIADB_AUDIT_SIGNING_KEY` must stay set for the duration of
// the audit_export call. `#[tokio::test]` is a current-thread runtime, so the
// std `MutexGuard` never crosses threads.
#[allow(clippy::await_holding_lock)]
async fn audit_export_roundtrip_signs_and_redacts() {
    let _lock = audit_env_lock().lock().expect("audit env lock");
    // SAFETY: env mutation serialized via `audit_env_lock` within this binary;
    // restored below.
    let seed_hex: String = AUDIT_SEED.iter().map(|b| format!("{b:02x}")).collect();
    unsafe {
        std::env::set_var("ALETHEIADB_AUDIT_SIGNING_KEY", &seed_hex);
    }

    let db = fresh_db();
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("ssn", "secret")
                .build(),
        )
        .expect("create alice");
    let client = build_server_client(db, make_store(), AuthMode::Required);

    let req = json!({
        "entity_type": "node",
        "entity_id": alice.as_u64(),
        "database_id": "prod-db",
        "redact_keys": ["ssn"],
    });
    let (s, resp) = post(&client, "/audit/export", &req, Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "audit_export: {resp}");
    assert_eq!(
        resp["version_count"].as_u64(),
        Some(1),
        "one version: {resp}"
    );

    let expected_pub = AuditSigningKey::from_seed_bytes(AUDIT_SEED).public_key();
    assert_eq!(
        resp["public_key"].as_str(),
        Some(expected_pub.to_hex().as_str()),
        "public key travels in the response: {resp}"
    );

    // The artifact verifies offline against the operator's public key, and the
    // redacted value never leaked into it.
    let artifact_bytes = serde_json::to_vec(&resp["artifact"]).expect("artifact bytes");
    let report = verify_json_bytes(&artifact_bytes, Some(&expected_pub))
        .expect("autumn-produced artifact must verify offline");
    assert!(report.passed, "offline verification passes");
    assert!(
        report.redacted_keys.iter().any(|k| k == "ssn"),
        "ssn redacted"
    );
    assert!(
        !String::from_utf8(artifact_bytes)
            .expect("utf8")
            .contains("secret"),
        "redacted value must not leak"
    );

    unsafe {
        std::env::remove_var("ALETHEIADB_AUDIT_SIGNING_KEY");
    }
}

#[tokio::test]
// See `audit_export_roundtrip_signs_and_redacts`: the env-var lock spans the
// request `.await` by design (current-thread runtime).
#[allow(clippy::await_holding_lock)]
async fn audit_export_without_signing_key_is_failed_precondition() {
    let _lock = audit_env_lock().lock().expect("audit env lock");
    // SAFETY: env mutation serialized via `audit_env_lock`.
    unsafe {
        std::env::remove_var("ALETHEIADB_AUDIT_SIGNING_KEY");
    }

    let db = fresh_db();
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "a").build())
        .expect("create alice");
    let client = build_server_client(db, make_store(), AuthMode::Required);

    let req = json!({"entity_type": "node", "entity_id": alice.as_u64()});
    let (s, resp) = post(&client, "/audit/export", &req, Some(READER_TOKEN)).await;
    assert_eq!(s, 200, "in-band error 200: {resp}");
    assert_eq!(
        resp["error"]["code"], "FAILED_PRECONDITION",
        "missing signing key → FAILED_PRECONDITION: {resp}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Auth: keyless required-mode calls are 401 for all eight tools.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn all_eight_tools_require_credential_in_required_mode() {
    let db = fresh_db();
    let client = build_server_client(db, make_store(), AuthMode::Required);

    // GET reads.
    for uri in ["/constraints/unique", "/chain/head"] {
        let resp = client.get(uri).send().await;
        assert_eq!(resp.status.as_u16(), 401, "{uri} unauth");
        assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
    }
    // POST tools (writes + reads).
    let posts: &[(&str, Value)] = &[
        (
            "/vector/indexes",
            json!({"property_name": "e", "dimensions": 4}),
        ),
        (
            "/constraints/unique",
            json!({"label": "Person", "property": "email"}),
        ),
        (
            "/lineage/upstream",
            json!({"entity_kind": "node", "id": 1, "version": 1}),
        ),
        (
            "/lineage/downstream",
            json!({"entity_kind": "node", "id": 1, "version": 1}),
        ),
        (
            "/audit/export",
            json!({"entity_type": "node", "entity_id": 1}),
        ),
        ("/chain/verify", json!({})),
    ];
    for (uri, body) in posts {
        let resp = client.post(uri).json(body).send().await;
        assert_eq!(resp.status.as_u16(), 401, "{uri} unauth");
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Routing: all eight tools are routed on /mcp with their inventory access_class.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn all_eight_tools_routable_on_mcp_with_correct_class() {
    let db = fresh_db();
    let client = build_server_client(db, make_store(), AuthMode::Required);

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

    // Every one of the eight is advertised, and its RBAC class matches inventory.
    let expected: &[(&str, AccessClass)] = &[
        ("enable_vector_index", AccessClass::Write),
        ("enable_unique_constraint", AccessClass::Write),
        ("list_unique_constraints", AccessClass::Read),
        ("lineage_upstream", AccessClass::Read),
        ("lineage_downstream", AccessClass::Read),
        ("audit_export", AccessClass::Read),
        ("verify_chain", AccessClass::Read),
        ("export_chain_head", AccessClass::Read),
    ];
    for (name, class) in expected {
        assert!(
            tools.iter().any(|t| t["name"] == json!(name)),
            "/mcp must advertise {name}: {body}"
        );
        assert_eq!(
            rbac::tool_access_class(name),
            Some(*class),
            "{name} RBAC class must equal inventory access_class {class:?}"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Conformance: none of the eight is budgetable/cursorable, so none is registered
// in the shared DISPATCH_ROUTED_READ_TOOLS pinned-name table (the three
// audit-chain tools use dispatch_tool_json but pin their OWN name — no
// cross-tool escalation, so they are intentionally not pinned).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn none_of_the_eight_is_dispatch_routed() {
    for name in [
        "enable_vector_index",
        "enable_unique_constraint",
        "list_unique_constraints",
        "lineage_upstream",
        "lineage_downstream",
        "audit_export",
        "verify_chain",
        "export_chain_head",
    ] {
        assert!(
            !DISPATCH_ROUTED_READ_TOOLS.iter().any(|(n, _)| *n == name),
            "{name} is non-budgetable/non-cursorable and must NOT be dispatch-routed"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// OpenAPI fidelity: the POST-body tools expose a named per-operation request
// schema (never the shared generic `Value` a `Json<Value>` body emits, the #3589
// lesson); the two GET reads are documented.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn openapi_documents_the_eight_tools_with_named_bodies() {
    let db = fresh_db();
    let client = build_server_client(db, make_store(), AuthMode::Required);
    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let spec: Value = serde_json::from_str(&resp.text()).expect("openapi json");

    // POST bodies ref a typed per-operation schema, never the generic Value.
    for path in [
        "/vector/indexes",
        "/constraints/unique",
        "/lineage/upstream",
        "/lineage/downstream",
        "/audit/export",
        "/chain/verify",
    ] {
        let schema =
            &spec["paths"][path]["post"]["requestBody"]["content"]["application/json"]["schema"];
        let sref = schema["$ref"]
            .as_str()
            .unwrap_or_else(|| panic!("{path} requestBody must be a $ref: {schema}"));
        assert_ne!(
            sref, "#/components/schemas/Value",
            "{path} must not degrade to the shared generic Value component"
        );
    }

    // The two GET reads are documented.
    for path in ["/constraints/unique", "/chain/head"] {
        assert!(
            !spec["paths"][path]["get"].is_null(),
            "GET {path} documented: {}",
            spec["paths"]
        );
    }
}
