//! Integration tests for read-only-replica enforcement on the MCP surface
//! (Issue #3355, Slice A).
//!
//! Covers:
//! 5. A conformance sweep over every Write/Admin-class MCP tool
//!    (`AletheiaMcpServer::tool_access_classes()`), dispatched against a
//!    replica-role server: every one returns the structured
//!    `FAILED_PRECONDITION` error with `details.reason == "read_only_replica"`.
//!    Spot-checks that a Read tool (`get_node`) and a Metrics tool
//!    (`database_stats`) still succeed.
//! 7. `database_stats` JSON carries `replication.role`.
//!
//! Run with: `cargo test --test replication_mcp_readonly --features mcp-server`

#![cfg(feature = "mcp-server")]

use std::sync::Arc;

use aletheiadb::auth::AccessClass;
use aletheiadb::mcp::AletheiaMcpServer;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use serde_json::{Value, json};

fn dispatch(server: &AletheiaMcpServer, name: &str, args: Value) -> Value {
    let text = server.dispatch_tool_json(name, args);
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("tool '{name}' did not return valid JSON: {e}\n{text}"))
}

#[test]
fn every_write_and_admin_tool_is_refused_on_a_replica() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    db.enter_replica_mode();
    let server = AletheiaMcpServer::new(db);

    let mut checked = 0usize;
    for (name, class) in AletheiaMcpServer::tool_access_classes() {
        if !matches!(class, AccessClass::Write | AccessClass::Admin) {
            continue;
        }
        checked += 1;
        let value = dispatch(&server, name, json!({}));
        let error = value.get("error").unwrap_or_else(|| {
            panic!(
                "expected tool '{name}' ({class:?}-class) to be refused on a replica, got: \
                 {value}"
            )
        });
        assert_eq!(
            error["code"], "FAILED_PRECONDITION",
            "tool '{name}' should be FAILED_PRECONDITION, got: {value}"
        );
        assert_eq!(
            error["retriable"], false,
            "tool '{name}' should be non-retriable, got: {value}"
        );
        assert_eq!(
            error["details"]["reason"], "read_only_replica",
            "tool '{name}' details.reason mismatch, got: {value}"
        );
        assert_eq!(
            error["details"]["node_role"], "replica",
            "tool '{name}' details.node_role mismatch, got: {value}"
        );
    }
    assert!(
        checked >= 10,
        "expected to have swept a meaningful number of write/admin tools, got {checked}"
    );
}

#[test]
fn read_tool_still_succeeds_on_a_replica() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create node while primary");
    db.enter_replica_mode();
    let server = AletheiaMcpServer::new(db);

    let value = dispatch(&server, "get_node", json!({ "node_id": node_id.as_u64() }));
    assert!(
        value.get("error").is_none(),
        "get_node should succeed on a replica, got: {value}"
    );
}

#[test]
fn metrics_tool_still_succeeds_on_a_replica_and_reports_role() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    db.enter_replica_mode();
    let server = AletheiaMcpServer::new(db);

    let value = dispatch(&server, "database_stats", json!({}));
    assert!(
        value.get("error").is_none(),
        "database_stats should succeed on a replica, got: {value}"
    );
    assert_eq!(value["replication"]["role"], "replica");
}

#[test]
fn database_stats_reports_primary_role_by_default() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let server = AletheiaMcpServer::new(db);

    let value = dispatch(&server, "database_stats", json!({}));
    assert_eq!(value["replication"]["role"], "primary");
}
