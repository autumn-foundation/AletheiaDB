//! Issue #3577 — crash + WAL-replay round-trip for compare-and-set writes.
//!
//! A successful CAS is buffered and persisted as an ordinary
//! `UpdateNode`/`UpdateEdge` WAL op carrying the exact full-replace property map
//! (the version precondition is a commit-time check, NOT part of the on-disk
//! format — there is zero WAL format change). This test proves the committed CAS
//! state survives a **pure WAL replay**: a durable database records
//! create + CAS, is dropped, its **index snapshot + manifest are deleted**, then
//! reopened — forcing recovery to replay the FULL WAL from `LSN::initial()` and
//! decode the CAS's `UpdateNode`/`UpdateEdge` entries rather than being satisfied
//! by an index snapshot the background persistence worker wrote at a later LSN on
//! drop.
//!
//! Deleting the `indexes/` directory is load-bearing (learned from the #3549
//! recovery test): `AletheiaDB::open()` enables index persistence whose
//! background worker does an unconditional persist on drop at an LSN AFTER the
//! CAS ops; without removing it, replay would start at that manifest LSN and skip
//! the CAS's WAL entries, so the assertions would be satisfied by the snapshot,
//! not by genuine WAL decode.

use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[test]
fn cas_writes_survive_wal_replay() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let data_dir = tempdir.path().join("db");

    let node_id: u64;
    let edge_id: u64;

    {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");

        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("scratch", "drop-me")
                    .build(),
            )
            .expect("create node a");
        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .expect("create node b");
        node_id = a.as_u64();

        let e = db
            .create_edge(
                a,
                b,
                "KNOWS",
                PropertyMapBuilder::new()
                    .insert("since", 2020_i64)
                    .insert("weight", 7_i64)
                    .build(),
            )
            .expect("create edge");
        edge_id = e.as_u64();

        // CAS the node: full-replace to {name: Alice2}, dropping `scratch`.
        let av1 = db.get_node(a).expect("get a").current_version;
        db.compare_and_set_node(
            a,
            av1,
            PropertyMapBuilder::new().insert("name", "Alice2").build(),
        )
        .expect("node CAS");

        // CAS the edge: full-replace to {since: 2024}, dropping `weight`.
        let ev1 = db.get_edge(e).expect("get e").current_version;
        db.compare_and_set_edge(
            e,
            ev1,
            PropertyMapBuilder::new().insert("since", 2024_i64).build(),
        )
        .expect("edge CAS");

        // No explicit persist; the background worker persists on drop, so we
        // delete the snapshot below to force WAL-only replay.
        drop(db);
    }

    // Force a full WAL replay from LSN::initial() (see module docs).
    std::fs::remove_dir_all(data_dir.join("indexes")).ok();

    let db = AletheiaDB::open(&data_dir).expect("reopen durable db");

    let node = db
        .get_node(aletheiadb::core::NodeId::new(node_id).expect("nid"))
        .expect("get node after replay");
    assert_eq!(
        node.get_property("name")
            .and_then(|v| v.as_str().map(String::from)),
        Some("Alice2".to_string()),
        "CAS'd name must be recovered from WAL"
    );
    assert!(
        node.get_property("scratch").is_none(),
        "full-replace CAS dropped `scratch`; removal must survive WAL replay"
    );

    let edge = db
        .get_edge(aletheiadb::core::EdgeId::new(edge_id).expect("eid"))
        .expect("get edge after replay");
    assert_eq!(
        edge.get_property("since").and_then(|v| v.as_int()),
        Some(2024),
        "CAS'd edge property recovered"
    );
    assert!(
        edge.get_property("weight").is_none(),
        "full-replace CAS dropped edge `weight`; removal must survive WAL replay"
    );
}
