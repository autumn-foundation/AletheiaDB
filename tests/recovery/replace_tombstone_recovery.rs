//! Issue #3549 — crash + WAL-replay round-trip for the replace/tombstone API.
//!
//! A replace that REMOVES keys is buffered as an `UpdateNode`/`UpdateEdge` WAL
//! op carrying the *exact* target property map (no PATCH merge). This proves the
//! removal survives pure WAL replay: a durable database records create+replace,
//! is dropped WITHOUT persisting indexes (forcing WAL-only recovery), then
//! reopened — the reconstructed map must be the reduced one, and a relabel must
//! recover the new label.
//!
//! Mirrors the harness in `tests/wal_string_labels_recovery.rs`.

use aletheiadb::{AletheiaDB, PropertyMapBuilder};

fn props3(name: &str, age: i64, city: &str) -> aletheiadb::core::PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", name)
        .insert("age", age)
        .insert("city", city)
        .build()
}

#[test]
fn replace_removed_keys_survive_wal_replay() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let data_dir = tempdir.path().join("db");

    let node_id: u64;
    let edge_id: u64;

    {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");

        let a = db
            .create_node("Person", props3("Alice", 30, "London"))
            .expect("create node");
        let b = db
            .create_node("Person", props3("Bob", 40, "Paris"))
            .expect("create node b");
        node_id = a.as_u64();

        let e = db
            .create_edge(
                a,
                b,
                "KNOWS",
                PropertyMapBuilder::new()
                    .insert("since", 2020i64)
                    .insert("weight", 7i64)
                    .build(),
            )
            .expect("create edge");
        edge_id = e.as_u64();

        // Replace node: drop age & city AND relabel Person -> Employee.
        db.replace_node(
            a,
            "Employee",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("replace node");

        // Replace edge: keep only {since}, drop weight (endpoints/type immutable).
        db.replace_edge(
            e,
            PropertyMapBuilder::new().insert("since", 2024i64).build(),
        )
        .expect("replace edge");

        // Deliberately NO persist_indexes(): recovery must come from WAL alone.
        drop(db);
    }

    // Reopen — forces WAL replay (no index snapshot exists).
    let db = AletheiaDB::open(&data_dir).expect("reopen durable db");

    let node = db
        .get_node(aletheiadb::core::NodeId::new(node_id).expect("nid"))
        .expect("get node after replay");
    assert!(node.get_property("name").is_some(), "name recovered");
    assert!(
        node.get_property("age").is_none(),
        "age must stay REMOVED across WAL replay, got {:?}",
        node.get_property("age")
    );
    assert!(
        node.get_property("city").is_none(),
        "city must stay REMOVED across WAL replay"
    );

    // Relabel recovered: label index reflects Employee, not Person.
    let employees = db.get_nodes_by_label("Employee");
    assert_eq!(employees.len(), 1, "one Employee after replay");
    assert_eq!(employees[0].id.as_u64(), node_id);
    assert!(
        db.get_nodes_by_label("Person")
            .iter()
            .all(|n| n.id.as_u64() != node_id),
        "relabeled node must not appear under old Person label"
    );

    let edge = db
        .get_edge(aletheiadb::core::EdgeId::new(edge_id).expect("eid"))
        .expect("get edge after replay");
    assert_eq!(
        edge.get_property("since").and_then(|v| v.as_int()),
        Some(2024),
        "edge since overwritten value recovered"
    );
    assert!(
        edge.get_property("weight").is_none(),
        "edge weight must stay REMOVED across WAL replay"
    );
}
