//! Issue #3506 — end-to-end crash-recovery proof that node/edge/constraint
//! **label** strings survive pure WAL replay.
//!
//! Mirrors `tests/destructive_provenance_recovery.rs`: a durable database
//! (real WAL + index persistence) records the writes, then the database is
//! dropped **without persisting indexes** so a reopen is forced to reconstruct
//! state purely from WAL replay. Before #3506 the label was written to the WAL
//! as a raw `InternedString` id and replayed via `from_raw` without
//! re-interning — correct only when the replaying process reproduced the exact
//! interner layout of the writer. v13/v14 segments serialize the label string
//! itself and re-intern on read, so labels are correct under any layout.

use aletheiadb::{AletheiaDB, PropertyMapBuilder};

fn person(name: &str) -> aletheiadb::core::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// Nodes, edges, and a unique constraint all recorded with distinct label
/// strings must recover with those exact labels after a WAL-only replay
/// (no index snapshot). This is the config the issue names as unprotected
/// (`persistence_manager.is_none()` / WAL-only replay path).
#[test]
fn crash_recovery_preserves_node_edge_and_constraint_labels() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let data_dir = tempdir.path().join("db");

    let alice: u64;
    let bob: u64;

    {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");

        // A unique constraint declaration carries two label strings through
        // the WAL (the label and the property).
        db.unique_constraint("Person", "email")
            .enable()
            .expect("declare unique constraint");

        let a = db
            .create_node("Person", person("Alice"))
            .expect("create Alice");
        let b = db
            .create_node("Organization", person("Acme"))
            .expect("create Acme");
        alice = a.as_u64();
        bob = b.as_u64();
        db.create_edge(a, b, "WORKS_AT", PropertyMapBuilder::new().build())
            .expect("create edge");

        // Deliberately NO db.persist_indexes(): recovery must come from the
        // WAL alone.
        drop(db);
    }

    // Reopen — forces WAL replay (no index snapshot exists).
    let db = AletheiaDB::open(&data_dir).expect("reopen durable db");

    // Node labels recovered exactly: the label-keyed index is populated only
    // if the replayed label string matches.
    let persons = db.get_nodes_by_label("Person");
    assert_eq!(persons.len(), 1, "exactly one Person after WAL replay");
    assert_eq!(persons[0].id.as_u64(), alice);

    let orgs = db.get_nodes_by_label("Organization");
    assert_eq!(orgs.len(), 1, "exactly one Organization after WAL replay");
    assert_eq!(orgs[0].id.as_u64(), bob);

    // A wrong-label lookup finds nothing (guards against a silent mislabel
    // resolving to some other interned string).
    assert!(
        db.get_nodes_by_label("email").is_empty(),
        "no node should be mislabeled to the constraint's property string"
    );

    // The unique constraint declaration recovered with both label strings.
    let constraints = db.list_unique_constraints();
    assert!(
        constraints
            .iter()
            .any(|(l, p)| l == "Person" && p == "email"),
        "unique constraint (Person, email) must survive WAL replay, got {constraints:?}"
    );
}

/// Two databases in the same process share the process-global interner, so the
/// second `open()` shifts the interner layout relative to the first. Recovering
/// the first database from its WAL under that shifted layout must still resolve
/// labels to the correct strings — the exact non-identity-layout scenario the
/// raw-id encoding got wrong.
#[test]
fn crash_recovery_correct_under_shifted_interner_layout() {
    let tmp_a = tempfile::tempdir().expect("tempdir A");
    let dir_a = tmp_a.path().join("db_a");

    let widget: u64;
    {
        let db = AletheiaDB::open(&dir_a).expect("open db A");
        let w = db
            .create_node("Widget", person("W1"))
            .expect("create Widget");
        widget = w.as_u64();
        drop(db);
    }

    // Open a SECOND database and intern a pile of unrelated label strings,
    // advancing the process-global interner's id space so any raw label id
    // recorded by db A no longer aligns with the same string.
    {
        let tmp_b = tempfile::tempdir().expect("tempdir B");
        let db_b = AletheiaDB::open(tmp_b.path().join("db_b")).expect("open db B");
        for i in 0..64 {
            db_b.create_node(&format!("DecoyLabel{i}"), person("decoy"))
                .expect("create decoy");
        }
        drop(db_b);
    }

    // Reopen db A from its WAL under the now-shifted interner layout.
    let db = AletheiaDB::open(&dir_a).expect("reopen db A");
    let widgets = db.get_nodes_by_label("Widget");
    assert_eq!(
        widgets.len(),
        1,
        "Widget label must resolve correctly under a shifted interner layout"
    );
    assert_eq!(widgets[0].id.as_u64(), widget);
}
