//! Full-surface parity + completeness sweep (Issue #3524 PR8, the COMPLETING
//! slice of the autumn-web migration).
//!
//! Every prior slice ported one cluster of the AletheiaDB surface onto the
//! unified autumn-web `#[api_doc(mcp)]` handlers and pinned it byte-for-byte
//! against the legacy 0.4 HTTP router / legacy MCP server. This file is the
//! consolidating sweep: it asserts, from the LIVE assembled app, that the
//! **whole** surface is now complete and matches the cross-lane source of
//! truth `tests/parity/inventory.json` — no tool missing, none extra, every
//! access class in lockstep, all five HTTP routes routed, `/openapi.json`
//! served and covering the tool routes, and the budgetable/cursorable behavior
//! sets exactly as inventory documents.
//!
//! It intentionally holds NO production code of its own — it only reads the
//! inventory and drives the same `build_server_client` app the parity suites
//! drive. A failure here is a REAL gap in the merged surface (a tool that
//! silently dropped off `/mcp`, a class drift, an unrouted HTTP route), which
//! is exactly what a completing sweep exists to catch.

use std::collections::BTreeSet;
use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheia_server::edge_tools::{ALL_DISPATCH_ROUTED, DISPATCH_ROUTED_READ_TOOLS};
use aletheia_server::security::rbac;
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Role};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";

/// The repo-root inventory, located relative to this crate's manifest dir so the
/// test is CWD-independent (mirrors `tests/security_rbac.rs`).
const INVENTORY_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/parity/inventory.json"
));

/// The five #3360 cursorable tools (CLAUDE.md "Cursor continuation for large
/// scans"): `list_nodes`, `find_nodes_at_time`, `get_outgoing_edges`,
/// `get_incoming_edges`, `traverse`. Inventory carries a `budgetable` flag but
/// no `cursorable` flag, so the cursorable set is pinned here and cross-checked
/// against the live surface (each must be routable and — since cursoring is a
/// slow-read behavior — budgetable).
const CURSORABLE_TOOLS: &[&str] = &[
    "list_nodes",
    "find_nodes_at_time",
    "get_outgoing_edges",
    "get_incoming_edges",
    "traverse",
];

// ── fixtures ────────────────────────────────────────────────────────────────

/// A minimal DB (one `Person` node with a vector) + an auth store carrying an
/// admin key (Admin passes every RBAC class, so one token exercises the whole
/// surface).
fn fixture() -> (Arc<AletheiaDB>, Arc<AuthStore>) {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert_vector("embedding", &[0.1_f32, 0.2, 0.3, 0.4])
            .build(),
    )
    .expect("create node");

    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("insert admin key");
    (db, store)
}

/// Map an inventory `access_class` string to the typed [`AccessClass`] (panics
/// on any value outside the known four — an unexpected class fails loudly).
/// Mirrors `tests/security_rbac.rs::class_from_inventory`.
fn class_from_inventory(s: &str) -> AccessClass {
    match s {
        "read" => AccessClass::Read,
        "write" => AccessClass::Write,
        "metrics" => AccessClass::Metrics,
        "admin" => AccessClass::Admin,
        other => panic!("unexpected access_class {other:?} in inventory.json"),
    }
}

/// Parse the pinned inventory document.
fn inventory() -> Value {
    serde_json::from_str(INVENTORY_JSON).expect("inventory.json parses")
}

/// The live set of tool names the assembled app actually routes on `/mcp`
/// (the JSON-RPC `tools/list` catalog).
async fn live_mcp_catalog(client: &TestClient) -> BTreeSet<String> {
    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200, "tools/list must succeed");
    let body: Value = resp.json();
    body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_string())
        .collect()
}

// ════════════════════════════════════════════════════════════════════════════
// (1) MCP catalog == inventory — EXACT set equality across all 46 tools.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mcp_catalog_equals_inventory_exactly() {
    let (db, store) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let live = live_mcp_catalog(&client).await;

    let doc = inventory();
    let inv: BTreeSet<String> = doc["mcp"]["tools"]
        .as_array()
        .expect("mcp.tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_string())
        .collect();

    assert_eq!(
        inv.len(),
        46,
        "inventory must advertise exactly 46 MCP tools"
    );

    // Symmetric difference, reported precisely so a drift names the culprits.
    let missing: BTreeSet<&String> = inv.difference(&live).collect();
    let extra: BTreeSet<&String> = live.difference(&inv).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "live /mcp catalog must EQUAL inventory.json mcp.tools[] exactly.\n  \
         missing from live (in inventory, not routed): {missing:?}\n  \
         extra on live   (routed, not in inventory):  {extra:?}"
    );
    assert_eq!(
        live.len(),
        46,
        "the live catalog must be exactly 46 tools (got {})",
        live.len()
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (2) Access-class conformance for all 46 (belt-and-suspenders full-set check;
//     `tests/security_rbac.rs::registry_matches_inventory_exactly` pins the
//     static registry, this pins the *live-catalog-anchored* class per tool).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn access_class_conformance_for_all_46() {
    let (db, store) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let live = live_mcp_catalog(&client).await;

    let doc = inventory();
    for t in doc["mcp"]["tools"].as_array().expect("mcp.tools array") {
        let name = t["name"].as_str().expect("tool name");
        let expected = class_from_inventory(t["access_class"].as_str().expect("access_class"));
        // Every inventory tool is routed live...
        assert!(
            live.contains(name),
            "inventory tool {name:?} is not routed on the live /mcp catalog"
        );
        // ...and its RBAC-registry class equals inventory exactly.
        assert_eq!(
            rbac::tool_access_class(name),
            Some(expected),
            "RBAC class for {name:?} must equal inventory access_class ({expected:?})"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// (3) All five HTTP routes are served (routed, not the autumn no-route 404).
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn all_five_http_routes_are_served() {
    let (db, store) = fixture();
    // Anonymous mode: the synthetic principal is Admin, so every route's class
    // gate passes and a non-404 status positively proves the method+path is
    // routed (a 401/403 could otherwise mask an unrouted path).
    let client = build_server_client(db, store.clone(), AuthMode::Anonymous);

    // Pre-mint a key so the revoke route hits an existing id (its own semantic
    // 404-on-unknown-id would otherwise be indistinguishable from unrouted).
    let (principal, _key) = store
        .create_key("sweep-revoke", Role::Reader)
        .expect("mint key");

    let doc = inventory();
    let routes = doc["http"]["routes"].as_array().expect("http.routes array");
    assert_eq!(routes.len(), 5, "inventory must list exactly 5 HTTP routes");

    for route in routes {
        let method = route["method"].as_str().expect("method");
        let path = route["path"].as_str().expect("path");

        let status = match (method, path) {
            ("GET", p) => client.get(p).send().await.status.as_u16(),
            ("POST", "/admin/keys") => client
                .post(path)
                .json(&json!({ "name": "sweep-create", "role": "reader" }))
                .send()
                .await
                .status
                .as_u16(),
            ("POST", "/admin/keys/revoke") => client
                .post(path)
                .json(&json!({ "id": principal.id }))
                .send()
                .await
                .status
                .as_u16(),
            ("POST", "/query") => {
                // An empty body deserializes into the all-optional QueryBody;
                // the query tool returns its structured payload in-band. Routed
                // either way — we only assert non-404.
                client
                    .post(path)
                    .json(&json!({}))
                    .send()
                    .await
                    .status
                    .as_u16()
            }
            (m, p) => panic!("unexpected inventory route {m} {p}"),
        };

        assert_ne!(
            status, 404,
            "{method} {path} must be routed (got the no-route 404)"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// (4) /openapi.json is served and covers the MCP-projection routes for the
//     tools — one representative route from every ported cluster.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn openapi_served_and_covers_representative_tool_routes() {
    let (db, store) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200, "/openapi.json must be served");
    let spec: Value = resp.json();
    let paths = &spec["paths"];
    assert!(paths.is_object(), "openapi spec must carry a paths object");

    // (path, method, cluster) — at least one representative per ported cluster,
    // plus the two HTTP admin/status routes, all of which the api_doc surface
    // projects into `paths`.
    let representatives: &[(&str, &str, &str)] = &[
        ("/nodes/{id}", "get", "node reads"),
        ("/nodes", "post", "node writes"),
        ("/edges/{id}", "get", "edge reads"),
        ("/nodes/{node_id}/edges/outgoing", "get", "edge adjacency"),
        ("/traverse", "get", "traverse/temporal"),
        ("/nodes/diff", "post", "temporal diff"),
        ("/find_similar", "post", "vector"),
        ("/query", "post", "query"),
        ("/schema", "get", "schema"),
        ("/batch", "post", "batch"),
        ("/constraints/unique", "post", "constraints"),
        ("/lineage/upstream", "post", "lineage"),
        ("/chain/head", "get", "audit/chain"),
        ("/status", "get", "http status"),
        ("/admin/keys", "post", "http admin"),
    ];

    for (path, method, cluster) in representatives {
        let op = &paths[path][method];
        assert!(
            !op.is_null(),
            "openapi must document {method} {path} ({cluster} cluster): available paths = {:?}",
            paths.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// (5) Budgetable / cursorable behavior sets match inventory.
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn budgetable_and_cursorable_sets_match_inventory() {
    let (db, store) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let live = live_mcp_catalog(&client).await;

    let doc = inventory();

    // inventory `budgetable:true` set.
    let inv_budgetable: BTreeSet<String> = doc["mcp"]["tools"]
        .as_array()
        .expect("mcp.tools array")
        .iter()
        .filter(|t| t["budgetable"].as_bool() == Some(true))
        .map(|t| t["name"].as_str().expect("tool name").to_string())
        .collect();
    assert_eq!(
        inv_budgetable.len(),
        13,
        "inventory must mark exactly 13 budgetable read tools"
    );
    // Cross-check against the totals block too.
    assert_eq!(
        doc["totals"]["mcp_budgetable_read_tools"].as_u64(),
        Some(13),
        "totals.mcp_budgetable_read_tools must be 13"
    );

    // The crate's DISPATCH_ROUTED_READ_TOOLS is precisely the budgetable set:
    // every budgetable tool forwards raw args through dispatch_tool_json so the
    // #3353 budget shaper runs. Assert EXACT equality (both directions).
    let dispatch_budgetable: BTreeSet<String> = DISPATCH_ROUTED_READ_TOOLS
        .iter()
        .map(|(n, _)| (*n).to_string())
        .collect();
    assert_eq!(
        dispatch_budgetable, inv_budgetable,
        "DISPATCH_ROUTED_READ_TOOLS must equal inventory's budgetable:true set exactly \
         (every budgetable tool is dispatch-routed so the token budget applies)"
    );

    // Every budgetable tool is live-routable and read-class.
    for name in &inv_budgetable {
        assert!(
            live.contains(name),
            "budgetable tool {name:?} must be routed on /mcp"
        );
        assert_eq!(
            rbac::tool_access_class(name),
            Some(AccessClass::Read),
            "budgetable tool {name:?} must be Read class"
        );
    }

    // ALL_DISPATCH_ROUTED is the budgetable set plus the three non-budgetable
    // provenance-hash-chain tools — the superset covering every dispatch site.
    let all_dispatch: BTreeSet<String> = ALL_DISPATCH_ROUTED
        .iter()
        .map(|(n, _)| (*n).to_string())
        .collect();
    assert!(
        dispatch_budgetable.is_subset(&all_dispatch),
        "DISPATCH_ROUTED_READ_TOOLS must be a subset of ALL_DISPATCH_ROUTED"
    );
    let chain_extra: BTreeSet<String> = all_dispatch
        .difference(&dispatch_budgetable)
        .cloned()
        .collect();
    assert_eq!(
        chain_extra,
        ["audit_export", "export_chain_head", "verify_chain"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<String>>(),
        "ALL_DISPATCH_ROUTED beyond the budgetable set must be exactly the 3 chain tools"
    );

    // Cursorable set (#3360): each is routable and — cursoring being a slow-read
    // behavior — also budgetable, so it composes with the token budget.
    let cursorable: BTreeSet<String> = CURSORABLE_TOOLS.iter().map(|s| s.to_string()).collect();
    assert_eq!(cursorable.len(), 5, "there are exactly 5 cursorable tools");
    for name in &cursorable {
        assert!(
            live.contains(name),
            "cursorable tool {name:?} must be routed on /mcp"
        );
        assert!(
            inv_budgetable.contains(name),
            "cursorable tool {name:?} must also be budgetable (cursorable ⊆ budgetable)"
        );
    }
}
