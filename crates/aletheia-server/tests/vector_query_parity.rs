//! Parity + behavior tests for the VECTOR + HYBRID + QUERY cluster (Issue #3524,
//! PR5): `find_similar`, `list_vector_indexes`, `hybrid_query`, and `query`.
//!
//! Each test drives the new crate's autumn [`TestClient`] and asserts the
//! response equals the **legacy MCP surface** over the same `Arc<AletheiaDB>` —
//! byte-for-byte (parsed-JSON equality, `trace_id` + wall-clock-volatile
//! bi-temporal timestamps normalized) — in both anonymous and required auth
//! modes. The legacy reference is `dispatch_tool_json` for the three
//! dispatch-routed budgetable reads (`find_similar`, `hybrid_query`, `query`) and
//! the typed `list_vector_indexes` method. Plus: #3220 vector elision on
//! `find_similar` / `hybrid_query`, #3353 budget shaping (results never
//! dropped/reordered — only per-result payloads degrade), the `query` tool's
//! structured-error-kind preservation (`parse_error`, `read_only_violation`,
//! cursor → `unsupported_construct`, and — depending on the build — either
//! `language_unavailable` (the default build, `cypher` off, so `language:"cypher"`
//! is unavailable) or successful Cypher execution (the Code Coverage build, which
//! compiles `cypher` in). The two mutually-exclusive `#[cfg]`-gated tests
//! `query_cypher_language_unavailable_when_feature_off` /
//! `query_cypher_executes_when_feature_on` cover both cases. Plus the shared
//! `DISPATCH_ROUTED_READ_TOOLS` pin.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Role};
use aletheiadb::mcp::{AletheiaMcpServer, EnableVectorIndexRequest, ListVectorIndexesRequest};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";

/// A long note so a single ranked result comfortably exceeds a modest byte
/// budget, forcing the #3353 ladder to shape it down without dropping results.
const LONG_NOTE: &str = "a deliberately long note property, repeated so the full \
     serialized response comfortably exceeds a moderate byte budget and the #3353 \
     ladder must shape it down: lorem ipsum dolor sit amet consectetur adipiscing \
     elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua ut enim \
     ad minim veniam quis nostrud exercitation ullamco laboris nisi ut aliquip ex";

struct Fixture {
    db: Arc<AletheiaDB>,
    store: Arc<AuthStore>,
    alice: u64,
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

/// A shared DB with a `cosine` vector index on `embedding` (dim 4) and a small
/// graph: Alice --KNOWS--> {Bob, Carol, Dave}, each Document/Person bearing a
/// distinct embedding + a bulky `note` so ranked responses are budgetable.
fn fixture() -> Fixture {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    // Enable the vector index up front (writer/admin op done directly on the db,
    // bypassing the autumn gate — this is fixture setup, not the surface under
    // test).
    let setup = mcp_server(&db);
    let enabled = setup.enable_vector_index(EnableVectorIndexRequest {
        property_name: "embedding".to_string(),
        dimensions: 4,
        distance_metric: Some("cosine".to_string()),
    });
    assert!(
        serde_json::from_str::<Value>(&enabled).expect("json")["success"] == json!(true),
        "vector index must enable: {enabled}"
    );

    // A bulky note so a full ranked/query response comfortably exceeds a cap set
    // above the #3353 minimal-rung "min viable" budget (which strips property
    // values, so it does NOT grow with note size).
    let bulky = LONG_NOTE.repeat(8);
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("note", bulky.as_str())
                .insert_vector("embedding", &[0.10_f32, 0.20, 0.30, 0.40])
                .build(),
        )
        .expect("create alice")
        .as_u64();

    let mut idx = 1.0_f32;
    for name in ["Bob", "Carol", "Dave"] {
        let target = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", name)
                    .insert("note", bulky.as_str())
                    .insert_vector(
                        "embedding",
                        &[0.10 * idx, 0.20 * idx, 0.30 * idx, 0.40 * idx],
                    )
                    .build(),
            )
            .expect("create target");
        db.create_edge(
            aletheiadb::core::NodeId::new(alice).unwrap(),
            target,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        )
        .expect("create edge");
        idx += 1.0;
    }

    Fixture {
        db,
        store: make_store(),
        alice,
    }
}

/// Recursively drop the per-request `trace_id` and the wall-clock-volatile
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

// ════════════════════════════════════════════════════════════════════════════
// Byte-parity for all four tools vs the legacy MCP surface, in both auth modes.
// ════════════════════════════════════════════════════════════════════════════

async fn assert_all_four_parity(mode: AuthMode, token: Option<&'static str>) {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), mode);
    let server = mcp_server(&fx.db);

    // 1. find_similar (POST) — dispatch reference. `FindSimilarRequest` is
    //    `#[non_exhaustive]` (cannot be built out-of-crate), so the legacy
    //    reference is the equivalent public `dispatch_tool_json` path — exactly
    //    what the autumn handler forwards to.
    let fs_args = json!({
        "property_name": "embedding",
        "embedding": [0.10, 0.20, 0.30, 0.40],
        "k": 3
    });
    let (s, autumn) = post(&client, "/find_similar", &fs_args, token).await;
    assert_eq!(s, 200, "find_similar: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("find_similar", fs_args.clone()))
            .expect("legacy find_similar json");
    assert_eq!(normalize(autumn), normalize(legacy), "find_similar parity");

    // 2. hybrid_query (POST) — dispatch reference.
    let hq_args = json!({
        "start_node_id": fx.alice,
        "traverse_edge": "KNOWS",
        "traverse_depth": 1,
        "vector_property": "embedding",
        "query_embedding": [0.10, 0.20, 0.30, 0.40],
        "top_k": 3
    });
    let (s, autumn) = post(&client, "/hybrid_query", &hq_args, token).await;
    assert_eq!(s, 200, "hybrid_query: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.dispatch_tool_json("hybrid_query", hq_args.clone()))
            .expect("legacy hybrid_query json");
    assert_eq!(normalize(autumn), normalize(legacy), "hybrid_query parity");

    // 3. query (POST, AQL — always available) — dispatch reference.
    let q_args = json!({ "language": "aql", "query": "MATCH (n:Person) RETURN n" });
    let (s, autumn) = post(&client, "/query", &q_args, token).await;
    assert_eq!(s, 200, "query: {autumn}");
    let legacy: Value = serde_json::from_str(&server.dispatch_tool_json("query", q_args.clone()))
        .expect("legacy query json");
    assert_eq!(normalize(autumn), normalize(legacy), "query parity");

    // 4. list_vector_indexes (GET) — typed method reference.
    let (s, autumn) = get(&client, "/vector/indexes", token).await;
    assert_eq!(s, 200, "list_vector_indexes: {autumn}");
    let legacy: Value =
        serde_json::from_str(&server.list_vector_indexes(ListVectorIndexesRequest {}))
            .expect("legacy list_vector_indexes json");
    assert_eq!(
        normalize(autumn),
        normalize(legacy),
        "list_vector_indexes parity"
    );
}

#[tokio::test]
async fn all_four_byte_parity_required_mode() {
    assert_all_four_parity(AuthMode::Required, Some(READER_TOKEN)).await;
}

#[tokio::test]
async fn all_four_byte_parity_anonymous_mode() {
    assert_all_four_parity(AuthMode::Anonymous, None).await;
}

// ════════════════════════════════════════════════════════════════════════════
// #3220 vector elision on find_similar / hybrid_query, plus include_vectors.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn find_similar_elides_vectors_by_default_and_preserves_score() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);

    let args = json!({
        "property_name": "embedding",
        "embedding": [0.10, 0.20, 0.30, 0.40],
        "k": 3
    });
    let (status, body) = post(&client, "/find_similar", &args, Some(READER_TOKEN)).await;
    assert_eq!(status, 200, "find_similar: {body}");
    let results = body["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected ranked results: {body}");
    let first = &results[0];
    // The similarity score is always returned in full (never elided).
    assert!(
        first["score"].is_number(),
        "score returned in full: {first}"
    );
    // #3220: the embedding is elided by default on the vector read path.
    assert_eq!(
        first["node"]["properties"]["embedding"]["elided"], true,
        "embedding elided by default: {first}"
    );

    // include_vectors=true returns the raw array.
    let mut raw_args = args.clone();
    raw_args["include_vectors"] = json!(true);
    let (_s, raw) = post(&client, "/find_similar", &raw_args, Some(READER_TOKEN)).await;
    assert!(
        raw["results"][0]["node"]["properties"]["embedding"].is_array(),
        "include_vectors=true returns the raw array: {raw}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// #3353 token budget: find_similar / hybrid_query / query are budgetable. The
// budget shapes the response and matches the legacy dispatch; find_similar /
// hybrid_query NEVER drop or reorder ranked results (only per-result payloads
// degrade).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn find_similar_budget_shapes_without_dropping_results_and_matches_legacy() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);

    let base = json!({
        "property_name": "embedding",
        "embedding": [0.10, 0.20, 0.30, 0.40],
        "k": 4
    });

    // Full (no budget): no `budget` block, exceeds the cap.
    let (status, full) = post(&client, "/find_similar", &base, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(full.get("budget").is_none(), "no budget block: {full}");
    let full_count = full["results"].as_array().expect("results").len();
    assert!(full_count >= 3, "need several ranked results: {full}");
    // Capture the full ranked order for the drop/reorder check.
    let full_ids: Vec<u64> = full["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["node"]["id"].as_u64().expect("node id"))
        .collect();
    // Above the #3353 min-viable budget (~3116B for these results) yet below the
    // bulky full response, so the ladder shapes (not rejects) the response.
    let cap: u64 = 5000;
    assert!(
        len_of(&full) as u64 > cap,
        "full response should exceed the cap: {}",
        len_of(&full)
    );

    // Legacy dispatch is the reference; its emitted string is the exact bytes the
    // #3353 contract bounds.
    let mut budget_args = base.clone();
    budget_args["max_response_bytes"] = json!(cap);
    let legacy_text = server.dispatch_tool_json("find_similar", budget_args.clone());
    assert!(
        legacy_text.len() as u64 <= cap,
        "emitted string must fit the byte budget: {} > {cap}",
        legacy_text.len()
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert!(legacy["budget"].is_object(), "budget block: {legacy}");

    // Autumn with the same budget: shaped, matches legacy, results not dropped
    // or reordered (only per-result payloads degrade).
    let (status, budgeted) = post(&client, "/find_similar", &budget_args, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(budgeted["budget"].is_object(), "shaped: {budgeted}");
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped response smaller: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
    let budgeted_ids: Vec<u64> = budgeted["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["node"]["id"].as_u64().expect("node id"))
        .collect();
    assert_eq!(
        budgeted_ids, full_ids,
        "ranked results must NOT be dropped or reordered by the budget: {budgeted}"
    );
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn find_similar must equal the legacy dispatch"
    );
}

#[tokio::test]
async fn query_budget_shapes_response_and_matches_legacy() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);

    let base = json!({ "language": "aql", "query": "MATCH (n:Person) RETURN n" });
    let (status, full) = post(&client, "/query", &base, Some(READER_TOKEN)).await;
    assert_eq!(status, 200, "query: {full}");
    assert!(full.get("budget").is_none(), "no budget block: {full}");
    // Above the #3353 min-viable budget (~1508B) yet below the bulky full
    // response, so the ladder shapes (not rejects) the response.
    let cap: u64 = 3000;
    assert!(
        len_of(&full) as u64 > cap,
        "full response should exceed the cap: {}",
        len_of(&full)
    );

    let mut budget_args = base.clone();
    budget_args["max_response_bytes"] = json!(cap);
    let legacy_text = server.dispatch_tool_json("query", budget_args.clone());
    assert!(
        legacy_text.len() as u64 <= cap,
        "emitted string must fit the byte budget: {} > {cap}",
        legacy_text.len()
    );
    let legacy: Value = serde_json::from_str(&legacy_text).expect("legacy json");
    assert!(legacy["budget"].is_object(), "budget block: {legacy}");

    let (status, budgeted) = post(&client, "/query", &budget_args, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(budgeted["budget"].is_object(), "shaped: {budgeted}");
    assert!(
        len_of(&budgeted) < len_of(&full),
        "shaped response smaller: {} !< {}",
        len_of(&budgeted),
        len_of(&full)
    );
    assert_eq!(
        normalize(budgeted),
        normalize(legacy),
        "budgeted autumn query must equal the legacy dispatch"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// `query` structured-error-kind preservation. All error semantics are produced
// by dispatch and returned verbatim; each kind must match the legacy dispatch.
// ════════════════════════════════════════════════════════════════════════════

/// Assert the autumn `/query` response equals the legacy dispatch for `args` and
/// carries the expected structured error `kind`.
async fn assert_query_error_kind(
    client: &TestClient,
    server: &AletheiaMcpServer,
    args: Value,
    kind: &str,
) {
    let (status, autumn) = post(client, "/query", &args, Some(READER_TOKEN)).await;
    assert_eq!(
        status, 200,
        "query error returns 200 with in-band error: {autumn}"
    );
    let legacy: Value = serde_json::from_str(&server.dispatch_tool_json("query", args.clone()))
        .expect("legacy query error json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "query error parity for kind {kind}: {args}"
    );
    assert_eq!(
        autumn["error"]["kind"],
        json!(kind),
        "expected error kind {kind}: {autumn}"
    );
}

#[tokio::test]
async fn query_read_only_violation_kind_preserved() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);
    // A mutating clause is rejected BEFORE execution.
    assert_query_error_kind(
        &client,
        &server,
        json!({ "language": "aql", "query": "CREATE (n:Person {name: 'Mallory'}) RETURN n" }),
        "read_only_violation",
    )
    .await;
}

#[tokio::test]
async fn query_parse_error_kind_preserved() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);
    // Syntactically invalid AQL (passes the read-only guard, fails to parse).
    assert_query_error_kind(
        &client,
        &server,
        json!({ "language": "aql", "query": "MATCH (n:Person RETURN n !!!" }),
        "parse_error",
    )
    .await;
}

#[tokio::test]
async fn query_cursor_request_is_unsupported_construct() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);
    // Cursor paging is not supported for the `query` tool in v1 — no silent
    // fallback; a structured `unsupported_construct` is returned.
    assert_query_error_kind(
        &client,
        &server,
        json!({ "language": "aql", "query": "MATCH (n:Person) RETURN n", "use_cursor": true }),
        "unsupported_construct",
    )
    .await;
}

#[cfg(not(feature = "cypher"))]
#[tokio::test]
async fn query_cypher_language_unavailable_when_feature_off() {
    // The default build does NOT compile the `cypher` feature into the linked
    // `aletheiadb` rlib (this crate's dep enables only `http-server` +
    // `mcp-server` + defaults, and no `--features cypher` is passed), so
    // `language:"cypher"` returns `language_unavailable`. Both the autumn handler
    // and the legacy reference run in this same compiled crate, so parity holds.
    // The Code Coverage CI job compiles `cypher` in; the symmetric companion
    // `query_cypher_executes_when_feature_on` covers that build.
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);
    assert_query_error_kind(
        &client,
        &server,
        json!({ "language": "cypher", "query": "MATCH (n:Person) RETURN n" }),
        "language_unavailable",
    )
    .await;
}

/// Companion to `query_cypher_language_unavailable_when_feature_off`: when the
/// `cypher` feature IS compiled in (the Code Coverage CI build passes
/// `--features ...,cypher`, enabling `aletheiadb/cypher` through this crate's
/// forwarding feature), the same `language:"cypher"` query no longer reports
/// `language_unavailable` — it executes and returns the Person rows. Byte-parity
/// with the legacy dispatch still holds (both run in this compiled crate).
#[cfg(feature = "cypher")]
#[tokio::test]
async fn query_cypher_executes_when_feature_on() {
    let fx = fixture();
    let client = build_server_client(fx.db.clone(), fx.store.clone(), AuthMode::Required);
    let server = mcp_server(&fx.db);
    let args = json!({ "language": "cypher", "query": "MATCH (n:Person) RETURN n" });

    let (status, autumn) = post(&client, "/query", &args, Some(READER_TOKEN)).await;
    assert_eq!(
        status, 200,
        "cypher query returns 200 with a result body: {autumn}"
    );
    // With cypher compiled in the query EXECUTES: no structured error at all
    // (the old `language_unavailable` contract is inverted).
    assert!(
        autumn.get("error").is_none(),
        "cypher query must not report an error when the feature is on: {autumn}"
    );
    // Byte-parity vs the legacy dispatch reference over the same compiled crate.
    let legacy: Value = serde_json::from_str(&server.dispatch_tool_json("query", args.clone()))
        .expect("legacy query json");
    assert_eq!(
        normalize(autumn.clone()),
        normalize(legacy),
        "cypher-on query parity: {args}"
    );
    // The fixture has four Person nodes (Alice, Bob, Carol, Dave); `MATCH
    // (n:Person) RETURN n` returns all four with a `Person`-labeled entity each.
    assert_eq!(autumn["language"], "cypher", "echoed language: {autumn}");
    assert_eq!(
        autumn["row_count"].as_u64(),
        Some(4),
        "all four Person nodes returned: {autumn}"
    );
    let rows = autumn["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 4, "four rows: {autumn}");
    for row in rows {
        assert_eq!(
            row["entity"]["label"], "Person",
            "each row is a Person entity: {row}"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Auth: the four tools are ReadClass — a keyless required-mode call is 401.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn vector_query_tools_require_credential_in_required_mode() {
    let fx = fixture();
    let client = build_server_client(fx.db, fx.store, AuthMode::Required);

    // GET list_vector_indexes.
    let resp = client.get("/vector/indexes").send().await;
    assert_eq!(resp.status.as_u16(), 401, "list_vector_indexes unauth");
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));

    // POST find_similar / hybrid_query / query.
    for uri in ["/find_similar", "/hybrid_query", "/query"] {
        let resp = client.post(uri).json(&json!({})).send().await;
        assert_eq!(resp.status.as_u16(), 401, "{uri} unauth");
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Conformance: the three dispatch-routed budgetable reads are pinned in the
// shared DISPATCH_ROUTED_READ_TOOLS table with Read class (extends the coverage
// the `dispatch_pinned_names_match_routed_class` test in server_parity.rs
// asserts). `list_vector_indexes` (typed, non-budgetable) is intentionally NOT
// pinned.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dispatch_routed_table_includes_vector_query_reads() {
    use aletheia_server::edge_tools::DISPATCH_ROUTED_READ_TOOLS;
    for name in ["find_similar", "hybrid_query", "query"] {
        let entry = DISPATCH_ROUTED_READ_TOOLS
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("{name} must be pinned in DISPATCH_ROUTED_READ_TOOLS"));
        assert_eq!(entry.1, AccessClass::Read, "{name} pinned as Read");
    }
    assert!(
        !DISPATCH_ROUTED_READ_TOOLS
            .iter()
            .any(|(n, _)| *n == "list_vector_indexes"),
        "list_vector_indexes is typed/non-budgetable and must NOT be dispatch-routed"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// OpenAPI schema fidelity: the three POST reads must expose a named,
// per-operation request schema in /openapi.json — never the generic `Value`.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn openapi_post_reads_have_typed_request_schemas() {
    let fx = fixture();
    let client = build_server_client(fx.db, fx.store, AuthMode::Required);
    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let spec: Value = serde_json::from_str(&resp.text()).expect("openapi json");

    for (path, expected_suffix) in [
        ("/find_similar", "/FindSimilarBody"),
        ("/hybrid_query", "/HybridQueryBody"),
        ("/query", "/QueryBody"),
    ] {
        let schema =
            &spec["paths"][path]["post"]["requestBody"]["content"]["application/json"]["schema"];
        let sref = schema["$ref"]
            .as_str()
            .unwrap_or_else(|| panic!("{path} requestBody schema must be a $ref: {schema}"));
        assert!(
            sref.ends_with(expected_suffix),
            "{path} request body must ref a typed per-operation schema, got {sref}"
        );
        assert_ne!(
            sref, "#/components/schemas/Value",
            "{path} must not degrade to the shared generic Value component"
        );
    }
    // GET list_vector_indexes is documented.
    assert!(
        !spec["paths"]["/vector/indexes"]["get"].is_null(),
        "GET /vector/indexes documented: {}",
        spec["paths"]
    );
}
