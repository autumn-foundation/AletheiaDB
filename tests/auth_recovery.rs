//! Crash-recovery / restart test with authentication enabled
//! (Issue #3350 Phase 3, AC6).
//!
//! Exercises the full durable stack the production servers use:
//!
//! 1. A durable database (`AletheiaDB::open`) plus a persistent
//!    `AuthStore` (`with_persistence`) in the same data dir the HTTP/MCP
//!    binaries derive (`{data_dir}/auth/keys.json`).
//! 2. Authenticated writes through the MCP surface, stamping the verified
//!    principal into version provenance (AC4).
//! 3. Full restart: drop everything, reopen from disk.
//! 4. After recovery: graph data intact, the provenance **principal**
//!    intact (answerable from history, not just current state), the auth
//!    store reloaded from its file, a previously-created key still
//!    verifies, and a previously-revoked key is still revoked.
//!
//! Run with: `cargo test --test auth_recovery --features mcp-server`

#![cfg(feature = "mcp-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore, Role, SecretString};
use aletheiadb::core::NodeId;
use aletheiadb::mcp::{AletheiaMcpServer, CreateNodeRequest, McpAuthConfig, ProvenanceRequest};
use serde_json::Value;

#[test]
fn recovery_preserves_data_provenance_principal_and_key_store() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let data_dir = tempdir.path().join("db");
    // Same derivation `src/bin/server.rs` / `src/bin/mcp_server.rs` use.
    let keys_path = data_dir.join("auth").join("keys.json");

    let node_id: u64;
    let writer_key: String;
    let revoked_key: String;

    // ── Round 1: durable DB + persistent key store + authenticated writes ──
    {
        let db = Arc::new(AletheiaDB::open(&data_dir).expect("open durable db"));
        let store =
            Arc::new(AuthStore::with_persistence(&keys_path).expect("open persistent store"));

        let (_writer, key) = store
            .create_key("recovery-writer", Role::Writer)
            .expect("create writer key");
        writer_key = key.to_string();

        let (victim, key) = store
            .create_key("victim-reader", Role::Reader)
            .expect("create victim key");
        revoked_key = key.to_string();
        assert!(store.revoke(&victim.id).expect("revoke"), "key existed");

        let server = AletheiaMcpServer::with_auth(
            Arc::clone(&db),
            McpAuthConfig::new(AuthMode::Required, Arc::clone(&store))
                .with_credential(SecretString::new(writer_key.as_str())),
        );

        // Authenticated write with caller-supplied provenance: the stored
        // bundle must compose the caller's `source` with the server-stamped
        // `principal`.
        let response = server.create_node(CreateNodeRequest {
            namespace: None,
            derived_from: None,
            label: "Person".to_string(),
            properties: Some(
                [("name".to_string(), Value::String("Alice".to_string()))]
                    .into_iter()
                    .collect(),
            ),
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("hr-system".to_string()),
                confidence: Some(0.9),
                note: None,
                correlation_id: None,
            }),
        });
        let created: Value = serde_json::from_str(&response).expect("create_node JSON");
        assert!(
            created.get("error").is_none(),
            "authenticated create must succeed: {created}"
        );
        node_id = created["id"].as_u64().expect("created node id");

        // Pre-restart sanity: the principal is already stamped.
        let provenance = db
            .get_node_provenance(NodeId::new(node_id).expect("node id"))
            .expect("provenance lookup")
            .expect("provenance recorded");
        assert_eq!(provenance.principal(), Some("recovery-writer"));
        assert_eq!(provenance.source(), Some("hr-system"));

        // Flush index checkpoints the way the servers' shutdown hooks do.
        db.persist_indexes().expect("persist_indexes");
        drop(server);
        drop(db);
        drop(store);
    }

    // ── Round 2: reopen everything from disk ──
    let db = Arc::new(AletheiaDB::open(&data_dir).expect("reopen durable db"));
    let store = Arc::new(AuthStore::with_persistence(&keys_path).expect("reload key store"));

    // Data intact.
    let nid = NodeId::new(node_id).expect("node id");
    let node = db.get_node(nid).expect("node survives restart");
    assert_eq!(node.id.as_u64(), node_id);

    // Provenance principal intact — both on the current version and via the
    // version-level lookup ("who wrote this fact" is answerable from
    // history, not just an in-memory side effect).
    let provenance = db
        .get_node_provenance(nid)
        .expect("provenance lookup")
        .expect("provenance survives restart");
    assert_eq!(provenance.principal(), Some("recovery-writer"));
    assert_eq!(provenance.source(), Some("hr-system"));
    assert_eq!(provenance.confidence(), Some(0.9));

    let version_provenance = db
        .get_node_version_provenance(node.current_version)
        .expect("version provenance lookup")
        .expect("version provenance survives restart");
    assert_eq!(version_provenance.principal(), Some("recovery-writer"));

    // Auth store reloaded: the writer key still verifies with its role...
    let principal = store
        .verify(&writer_key)
        .expect("previously-created key must still verify after reload");
    assert_eq!(principal.name, "recovery-writer");
    assert_eq!(principal.role, Role::Writer);

    // ...and the revoked key is STILL revoked (revocation is durable).
    assert!(
        store.verify(&revoked_key).is_none(),
        "revoked key must stay revoked across restarts"
    );

    // The recovered stack serves authenticated traffic end-to-end.
    let server = AletheiaMcpServer::with_auth(
        Arc::clone(&db),
        McpAuthConfig::new(AuthMode::Required, Arc::clone(&store))
            .with_credential(SecretString::new(writer_key.as_str())),
    );
    let response = server.get_node(aletheiadb::mcp::GetNodeRequest {
        node_id,
        include_vectors: None,
    });
    let read: Value = serde_json::from_str(&response).expect("get_node JSON");
    assert!(
        read.get("error").is_none(),
        "recovered server must serve the recovered key: {read}"
    );
    assert_eq!(read["provenance"]["principal"], "recovery-writer");
    assert_eq!(read["provenance"]["source"], "hr-system");
}

/// Crash-style variant: NO `persist_indexes()` checkpoint before the
/// restart, so round 2 must reconstruct state purely from WAL replay.
/// The write, its `provenance.principal`, and the key-store state must all
/// survive.
#[test]
fn crash_recovery_via_wal_replay_preserves_provenance_principal_and_key_store() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let data_dir = tempdir.path().join("db");
    let keys_path = data_dir.join("auth").join("keys.json");

    let node_id: u64;
    let writer_key: String;
    let revoked_key: String;

    // ── Round 1: durable DB + persistent key store + authenticated write,
    //    then "crash" (drop with no index checkpoint) ──
    {
        let db = Arc::new(AletheiaDB::open(&data_dir).expect("open durable db"));
        let store =
            Arc::new(AuthStore::with_persistence(&keys_path).expect("open persistent store"));

        let (_writer, key) = store
            .create_key("crash-writer", Role::Writer)
            .expect("create writer key");
        writer_key = key.to_string();

        let (victim, key) = store
            .create_key("crash-victim", Role::Reader)
            .expect("create victim key");
        revoked_key = key.to_string();
        assert!(store.revoke(&victim.id).expect("revoke"), "key existed");

        let server = AletheiaMcpServer::with_auth(
            Arc::clone(&db),
            McpAuthConfig::new(AuthMode::Required, Arc::clone(&store))
                .with_credential(SecretString::new(writer_key.as_str())),
        );

        let response = server.create_node(CreateNodeRequest {
            namespace: None,
            derived_from: None,
            label: "Person".to_string(),
            properties: Some(
                [("name".to_string(), Value::String("Bob".to_string()))]
                    .into_iter()
                    .collect(),
            ),
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("crm".to_string()),
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        });
        let created: Value = serde_json::from_str(&response).expect("create_node JSON");
        assert!(
            created.get("error").is_none(),
            "authenticated create must succeed: {created}"
        );
        node_id = created["id"].as_u64().expect("created node id");

        // Deliberately NO db.persist_indexes(): recovery must come from
        // the WAL alone (the group-commit WAL has fsynced the write by the
        // time create_node returned).
        drop(server);
        drop(db);
        drop(store);
    }

    // ── Round 2: reopen — forces WAL replay (no index snapshot exists) ──
    let db = Arc::new(AletheiaDB::open(&data_dir).expect("reopen durable db"));
    let store = Arc::new(AuthStore::with_persistence(&keys_path).expect("reload key store"));

    // Data replayed from the WAL.
    let nid = NodeId::new(node_id).expect("node id");
    let node = db.get_node(nid).expect("node survives WAL replay");
    assert_eq!(node.id.as_u64(), node_id);

    // The stamped principal survives WAL replay (v5 payload format).
    let provenance = db
        .get_node_provenance(nid)
        .expect("provenance lookup")
        .expect("provenance survives WAL replay");
    assert_eq!(provenance.principal(), Some("crash-writer"));
    assert_eq!(provenance.source(), Some("crm"));

    // Key store state survives independently of the DB.
    let principal = store
        .verify(&writer_key)
        .expect("writer key must still verify after crash restart");
    assert_eq!(principal.name, "crash-writer");
    assert_eq!(principal.role, Role::Writer);
    assert!(
        store.verify(&revoked_key).is_none(),
        "revoked key must stay revoked across crash restarts"
    );

    // The recovered stack serves authenticated traffic end-to-end.
    let server = AletheiaMcpServer::with_auth(
        Arc::clone(&db),
        McpAuthConfig::new(AuthMode::Required, Arc::clone(&store))
            .with_credential(SecretString::new(writer_key.as_str())),
    );
    let response = server.get_node(aletheiadb::mcp::GetNodeRequest {
        node_id,
        include_vectors: None,
    });
    let read: Value = serde_json::from_str(&response).expect("get_node JSON");
    assert!(
        read.get("error").is_none(),
        "recovered server must serve the recovered key: {read}"
    );
    assert_eq!(read["provenance"]["principal"], "crash-writer");
    assert_eq!(read["provenance"]["source"], "crm");
}
