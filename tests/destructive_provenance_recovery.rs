//! Issue #3427 (Phase B) — end-to-end crash-recovery proof that a
//! caller-supplied provenance principal on a *destructive* operation
//! (delete / retract) survives real WAL replay.
//!
//! Mirrors the `tests/auth_recovery.rs::crash_recovery_via_wal_replay_*`
//! pattern: a durable database (real WAL + index persistence) records the
//! writes, then the database is dropped **without persisting indexes** so a
//! reopen is forced to reconstruct state purely from WAL replay. Before
//! Phase B the four destructive WAL payloads carried no provenance blob, so a
//! RAM-only principal evaporated on replay; these tests pin that it now
//! survives on the tombstone / retraction version.

use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::temporal::time;
use aletheiadb::core::{EdgeId, NodeId};
use aletheiadb::{AletheiaDB, PropertyMapBuilder, Provenance};

fn person(name: &str) -> aletheiadb::core::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// Delete a node and retract a node/edge — each stamped with a distinct
/// acting principal via the new #3427 write-path API — then crash-recover
/// from the WAL alone and assert every principal survived on the
/// tombstone / retraction version.
#[test]
fn crash_recovery_preserves_delete_and_retract_provenance_principal() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let data_dir = tempdir.path().join("db");

    let del_node: u64;
    let ret_node: u64;
    let ret_edge: u64;

    // ── Round 1: durable DB, destructive writes with provenance, then
    //    "crash" (drop with NO index checkpoint) ──
    {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");

        // Delete path: isolated node deleted with a principal.
        let a = db
            .create_node("Person", person("DeleteMe"))
            .expect("create A");
        del_node = a.as_u64();
        let del_prov = Provenance::builder()
            .source("mcp")
            .principal("svc-deleter")
            .build()
            .expect("build delete provenance");
        db.delete_node_with_options(a, WriteRequestOptions::new().with_provenance(del_prov))
            .expect("delete node with provenance");

        // Retract-node path: isolated node retracted with a principal.
        let b = db
            .create_node("Person", person("RetractMe"))
            .expect("create B");
        ret_node = b.as_u64();
        let ret_prov = Provenance::builder()
            .principal("svc-retractor")
            .build()
            .expect("build retract provenance");
        db.retract_node_with_provenance(b, time::now(), Some(ret_prov))
            .expect("retract node with provenance");

        // Retract-edge path: an edge retracted with a principal (its endpoint
        // nodes survive).
        let c = db
            .create_node("Person", person("EdgeSrc"))
            .expect("create C");
        let d = db
            .create_node("Person", person("EdgeDst"))
            .expect("create D");
        let e = db
            .create_edge(c, d, "KNOWS", PropertyMapBuilder::new().build())
            .expect("create edge");
        ret_edge = e.as_u64();
        let edge_prov = Provenance::builder()
            .principal("svc-edge-retractor")
            .build()
            .expect("build edge retract provenance");
        db.retract_edge_with_provenance(e, time::now(), Some(edge_prov))
            .expect("retract edge with provenance");

        // Deliberately NO db.persist_indexes(): recovery must come from the
        // WAL alone (group-commit WAL has fsynced by the time each call
        // returned).
        drop(db);
    }

    // ── Round 2: reopen — forces WAL replay (no index snapshot exists) ──
    let db = AletheiaDB::open(&data_dir).expect("reopen durable db");

    // Delete tombstone carries the deleter's principal after WAL replay.
    let del_nid = NodeId::new(del_node).expect("del node id");
    let del_history = db.get_node_history(del_nid).expect("delete history");
    assert_eq!(del_history.version_count(), 2, "create + tombstone");
    let tombstone = del_history
        .current_version()
        .expect("tombstone version")
        .provenance
        .as_ref()
        .expect("tombstone must carry provenance after WAL replay (#3427)");
    assert_eq!(tombstone.principal(), Some("svc-deleter"));
    assert_eq!(tombstone.source(), Some("mcp"));

    // Retraction version carries the retractor's principal after WAL replay.
    let ret_nid = NodeId::new(ret_node).expect("ret node id");
    let ret_history = db.get_node_history(ret_nid).expect("retract history");
    assert_eq!(ret_history.version_count(), 2, "create + retraction");
    let retraction = ret_history
        .current_version()
        .expect("retraction version")
        .provenance
        .as_ref()
        .expect("retraction must carry provenance after WAL replay (#3427)");
    assert_eq!(retraction.principal(), Some("svc-retractor"));

    // Edge retraction version carries its principal too.
    let ret_eid = EdgeId::new(ret_edge).expect("ret edge id");
    let edge_history = db.get_edge_history(ret_eid).expect("edge history");
    let edge_retraction = edge_history
        .current_version()
        .expect("edge retraction version")
        .provenance
        .as_ref()
        .expect("edge retraction must carry provenance after WAL replay (#3427)");
    assert_eq!(edge_retraction.principal(), Some("svc-edge-retractor"));
}

/// A cascade (detach) delete must attribute EVERY tombstone it creates — the
/// node AND every co-deleted edge — with the same acting principal, and all
/// of them must survive WAL replay.
#[test]
fn crash_recovery_cascade_delete_stamps_provenance_on_co_deleted_edges() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let data_dir = tempdir.path().join("db");

    let victim: u64;
    let edge1: u64;
    let edge2: u64;

    {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");

        // A hub node with two connected edges (one outgoing, one incoming).
        let hub = db.create_node("Person", person("Hub")).expect("create hub");
        let leaf_a = db
            .create_node("Person", person("LeafA"))
            .expect("create leaf A");
        let leaf_b = db
            .create_node("Person", person("LeafB"))
            .expect("create leaf B");
        victim = hub.as_u64();
        let e1 = db
            .create_edge(hub, leaf_a, "KNOWS", PropertyMapBuilder::new().build())
            .expect("edge 1");
        let e2 = db
            .create_edge(leaf_b, hub, "KNOWS", PropertyMapBuilder::new().build())
            .expect("edge 2");
        edge1 = e1.as_u64();
        edge2 = e2.as_u64();

        let prov = Provenance::builder()
            .source("mcp")
            .principal("svc-cascader")
            .build()
            .expect("build cascade provenance");
        db.delete_node_cascade_with_options(hub, WriteRequestOptions::new().with_provenance(prov))
            .expect("cascade delete with provenance");

        // Crash: no index checkpoint.
        drop(db);
    }

    let db = AletheiaDB::open(&data_dir).expect("reopen durable db");

    // Node tombstone carries the principal.
    let hub_nid = NodeId::new(victim).expect("hub node id");
    let hub_tombstone = db
        .get_node_history(hub_nid)
        .expect("hub history")
        .current_version()
        .expect("hub tombstone")
        .provenance
        .clone()
        .expect("hub tombstone must carry provenance after WAL replay (#3427)");
    assert_eq!(hub_tombstone.principal(), Some("svc-cascader"));

    // Both co-deleted edge tombstones carry the SAME principal.
    for eid in [edge1, edge2] {
        let edge_id = EdgeId::new(eid).expect("edge id");
        let edge_tombstone = db
            .get_edge_history(edge_id)
            .expect("edge history")
            .current_version()
            .expect("edge tombstone")
            .provenance
            .clone()
            .unwrap_or_else(|| {
                panic!(
                    "co-deleted edge {eid} tombstone must carry provenance after WAL replay (#3427)"
                )
            });
        assert_eq!(
            edge_tombstone.principal(),
            Some("svc-cascader"),
            "co-deleted edge {eid} must carry the cascade principal"
        );
    }
}
