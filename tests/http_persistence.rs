//! End-to-end persistence test: boot with a data_dir, create a node, drop
//! the DB, reopen with the same data_dir, verify the node is still there.
//!
//! Exercises the real `with_unified_config` code path that `run_server` now
//! uses in production (and that the Docker image depends on to behave like
//! a Postgres container).
//!
//! Run with: `cargo test --test http_persistence --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::auth::AuthMode;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use aletheiadb::{AletheiaDB, AletheiaDBConfig, NodeId};
use autumn_web::test::TestApp;
use serde_json::{Value, json};

/// Build the unified config the production server would use for this
/// `data_dir`. Delegating to `ServerConfig::to_unified_config` instead of
/// hand-rolling the builder call is the whole point of that method —
/// otherwise this helper would drift out of sync with the real wiring
/// in `src/http/server.rs::build_database`.
fn unified_config(data_dir: &std::path::Path) -> AletheiaDBConfig {
    ServerConfig::builder()
        .data_dir(data_dir)
        .build()
        .to_unified_config()
        .expect("data_dir set => Some(config)")
}

#[tokio::test]
async fn node_survives_database_restart_with_same_data_dir() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let data_path = tempdir.path().to_path_buf();

    // ── Round 1: create a node, then call persist_indexes (the same method
    // our `on_shutdown` hook in run_server invokes) before dropping. ──
    let alice_id: u64;
    {
        let db =
            Arc::new(AletheiaDB::with_unified_config(unified_config(&data_path)).expect("open db"));
        let state = AppState::new(db.clone());
        // Explicit anonymous opt-in (Issue #3350).
        let config = ServerConfig::builder()
            .auth_mode(AuthMode::Anonymous)
            .build();
        let router = build_test_router(state, &config).expect("build router");
        let client = TestApp::from_router(router);

        let resp = client
            .post("/query")
            .json(&json!({
                "operation": "create_node",
                "label": "Person",
                "properties": { "name": "Alice", "age": 30 }
            }))
            .send()
            .await;
        assert_eq!(resp.status.as_u16(), 200);

        let body: Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(body["success"], true);
        alice_id = body["data"]["id"]
            .as_u64()
            .expect("response should include node id");

        // Simulate the shutdown path: flush string interner + index
        // checkpoints to disk. Without this, the default
        // PersistencePolicies (threshold 500 strings / 10-minute interval)
        // would leave label strings out of the checkpoint and the label
        // would decode to `<unknown:N>` on reopen. The on_shutdown hook
        // in src/http/server.rs calls this same method.
        db.persist_indexes().expect("persist_indexes");

        drop(client);
        drop(db);
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // ── Round 2: reopen the database pointing at the same directory ──
    let db =
        Arc::new(AletheiaDB::with_unified_config(unified_config(&data_path)).expect("reopen db"));

    let nid = NodeId::new(alice_id).expect("valid node id");
    let node = db
        .get_node(nid)
        .expect("node should still exist after restart");
    assert_eq!(node.id.as_u64(), alice_id);

    let state = AppState::new(db);
    // Explicit anonymous opt-in (Issue #3350).
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    let client = TestApp::from_router(router);

    let resp = client
        .post("/query")
        .json(&json!({ "operation": "get_node", "node_id": alice_id }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let body: Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["success"], true);
    // Label round-trips — this is the key assertion that proves
    // persist_indexes captured the string interner state.
    assert_eq!(body["data"]["label"], "Person");
    assert_eq!(body["data"]["properties"]["name"], "Alice");
    assert_eq!(body["data"]["properties"]["age"], 30);
}

#[tokio::test]
async fn data_dir_none_means_in_memory() {
    // Verify default (no data_dir) ServerConfig really does use the
    // in-memory path. We can't directly observe `AletheiaDB::new` vs
    // `with_unified_config` from the outside, but we can assert the
    // config's `data_dir()` accessor returns None.
    let config = ServerConfig::default();
    assert!(config.data_dir().is_none());

    let config = ServerConfig::builder().port(8081).build();
    assert!(config.data_dir().is_none());
}

#[tokio::test]
async fn data_dir_some_round_trips_through_builder() {
    let path = std::path::PathBuf::from("/var/lib/aletheiadb");
    let config = ServerConfig::builder().data_dir(&path).build();
    assert_eq!(config.data_dir(), Some(path.as_path()));

    // Ensure a node created via the configured DB ends up in the expected
    // on-disk layout. Uses tempdir to avoid polluting the host.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let db = AletheiaDB::with_unified_config(unified_config(tempdir.path())).expect("open");

    let _ = db
        .create_node("Probe", PropertyMapBuilder::new().insert("k", "v").build())
        .expect("create_node");
    drop(db);

    assert!(
        tempdir.path().join("wal").exists(),
        "WAL directory should have been created"
    );
    assert!(
        tempdir.path().join("indexes").exists(),
        "Indexes directory should have been created"
    );
}
