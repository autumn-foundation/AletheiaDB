//! Secondary property (equality) index — behavior tests.
//!
//! Opt-in, node-only, current-state, rebuilt-on-load equality index over
//! `find_nodes_by_property`. The **scan is the oracle**: every indexed lookup
//! must return byte-identical results to the pre-existing full scan
//! (`src/storage/current/mod.rs`), and the index must never leak across
//! namespaces, never return stale entries, and never diverge from current state.
//!
//! Indexable value types are `String`, `Int`, `Bool`; `Float` (and other
//! variants) transparently fall back to the scan.

use std::sync::Arc;
use std::thread;

use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::temporal::time;
use aletheiadb::{AletheiaDB, NamespaceScope, PropertyMapBuilder, PropertyValue};

fn sorted(mut v: Vec<aletheiadb::core::id::NodeId>) -> Vec<u64> {
    v.sort_unstable();
    v.into_iter().map(|id| id.as_u64()).collect()
}

// ---------------------------------------------------------------------------
// 1. Equivalence: indexed lookups == scan lookups (String / Int / Bool)
// ---------------------------------------------------------------------------
#[test]
fn equivalence_scan_vs_index() {
    let db = AletheiaDB::new().expect("db");

    // Populate a mixed store.
    for i in 0..500i64 {
        db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("city", if i % 3 == 0 { "NYC" } else { "SF" })
                .insert("age", i % 5)
                .insert("active", i % 2 == 0)
                .build(),
        )
        .expect("create");
    }

    // Capture scan results BEFORE enabling the index (scan is the oracle).
    let scan_nyc = sorted(db.find_nodes_by_property("Person", "city", &PropertyValue::from("NYC")));
    let scan_age = sorted(db.find_nodes_by_property("Person", "age", &PropertyValue::Int(2)));
    let scan_act =
        sorted(db.find_nodes_by_property("Person", "active", &PropertyValue::Bool(true)));

    db.property_index("Person", "city")
        .enable()
        .expect("enable city");
    db.property_index("Person", "age")
        .enable()
        .expect("enable age");
    db.property_index("Person", "active")
        .enable()
        .expect("enable active");

    assert!(db.has_property_index("Person", "city"));
    assert!(db.has_property_index("Person", "age"));
    assert!(db.has_property_index("Person", "active"));

    // Indexed lookups must equal the scan oracle exactly.
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "city", &PropertyValue::from("NYC"))),
        scan_nyc
    );
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "age", &PropertyValue::Int(2))),
        scan_age
    );
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "active", &PropertyValue::Bool(true))),
        scan_act
    );
    // A value with no matches returns empty.
    assert!(
        db.find_nodes_by_property("Person", "city", &PropertyValue::from("LA"))
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// 2. Update removes the stale value entry
// ---------------------------------------------------------------------------
#[test]
fn update_removes_stale_entry() {
    let db = AletheiaDB::new().expect("db");
    db.property_index("Person", "email")
        .enable()
        .expect("enable");

    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("email", "a@x").build(),
        )
        .expect("create");

    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "email", &PropertyValue::from("a@x"))),
        vec![id.as_u64()]
    );

    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("email", "b@x").build(),
        None,
    )
    .expect("update");

    // Old value no longer resolves; new value does.
    assert!(
        db.find_nodes_by_property("Person", "email", &PropertyValue::from("a@x"))
            .is_empty()
    );
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "email", &PropertyValue::from("b@x"))),
        vec![id.as_u64()]
    );
}

// ---------------------------------------------------------------------------
// 3. Delete + cascade delete remove index entries
// ---------------------------------------------------------------------------
#[test]
fn delete_and_cascade_remove_entries() {
    let db = AletheiaDB::new().expect("db");
    db.property_index("Person", "email")
        .enable()
        .expect("enable");

    // Plain delete (no edges).
    let solo = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("email", "solo@x").build(),
        )
        .expect("create");
    db.delete_node_with_options(solo, WriteRequestOptions::new())
        .expect("delete");
    assert!(
        db.find_nodes_by_property("Person", "email", &PropertyValue::from("solo@x"))
            .is_empty()
    );

    // Cascade delete a node that has an edge.
    let a = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("email", "a@x").build(),
        )
        .expect("create a");
    let b = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("email", "b@x").build(),
        )
        .expect("create b");
    db.create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .expect("edge");

    db.delete_node_cascade_with_options(a, WriteRequestOptions::new())
        .expect("cascade delete");

    assert!(
        db.find_nodes_by_property("Person", "email", &PropertyValue::from("a@x"))
            .is_empty()
    );
    // b is untouched.
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "email", &PropertyValue::from("b@x"))),
        vec![b.as_u64()]
    );
}

// ---------------------------------------------------------------------------
// 4. Retraction removes the node from current-state index lookups
// ---------------------------------------------------------------------------
#[test]
fn retract_removes_entries() {
    let db = AletheiaDB::new().expect("db");
    db.property_index("Doc", "slug").enable().expect("enable");

    let id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new().insert("slug", "intro").build(),
        )
        .expect("create");
    assert_eq!(
        sorted(db.find_nodes_by_property("Doc", "slug", &PropertyValue::from("intro"))),
        vec![id.as_u64()]
    );

    db.retract_node(id, time::now()).expect("retract");

    assert!(
        db.find_nodes_by_property("Doc", "slug", &PropertyValue::from("intro"))
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// 5. Namespace no-leak (fail-closed via node_in_namespace_id retain)
// ---------------------------------------------------------------------------
#[test]
fn namespace_no_leak() {
    let db = AletheiaDB::new().expect("db");
    db.property_index("Person", "tag").enable().expect("enable");

    let a = db
        .create_node_in_namespace(
            "Person",
            PropertyMapBuilder::new().insert("tag", "v").build(),
            "agent:a",
        )
        .expect("create a");
    let d = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("tag", "v").build(),
        )
        .expect("create default");

    let scope_a = NamespaceScope::single(aletheiadb::Namespace::new("agent:a").unwrap());
    let scope_default = NamespaceScope::default();

    let in_a = db
        .find_nodes_by_property_scoped("Person", "tag", &PropertyValue::from("v"), &scope_a)
        .expect("scoped a");
    assert_eq!(sorted(in_a), vec![a.as_u64()]);

    let in_default = db
        .find_nodes_by_property_scoped("Person", "tag", &PropertyValue::from("v"), &scope_default)
        .expect("scoped default");
    assert_eq!(sorted(in_default), vec![d.as_u64()]);

    // Unscoped (All) sees both.
    let all = db
        .find_nodes_by_property_scoped(
            "Person",
            "tag",
            &PropertyValue::from("v"),
            &NamespaceScope::all(),
        )
        .expect("scoped all");
    assert_eq!(sorted(all), sorted(vec![a, d]));
}

// ---------------------------------------------------------------------------
// 6. Concurrent inserts + lookups: no torn/wrong ids, final count exact
// ---------------------------------------------------------------------------
#[test]
fn concurrent_insert_and_lookup() {
    let db = Arc::new(AletheiaDB::new().expect("db"));
    db.property_index("Item", "batch").enable().expect("enable");

    const THREADS: usize = 8;
    const PER: usize = 200;

    let mut handles = Vec::new();
    for _ in 0..THREADS {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for _ in 0..PER {
                db.create_node(
                    "Item",
                    PropertyMapBuilder::new().insert("batch", "v").build(),
                )
                .expect("create");
                // Interleave lookups; every returned id must currently hold value "v".
                let hits = db.find_nodes_by_property("Item", "batch", &PropertyValue::from("v"));
                for id in hits {
                    let node = db.get_node(id).expect("node exists");
                    assert_eq!(
                        node.properties.get("batch"),
                        Some(&PropertyValue::from("v"))
                    );
                }
            }
        }));
    }
    for h in handles {
        h.join().expect("thread");
    }

    let final_hits = sorted(db.find_nodes_by_property("Item", "batch", &PropertyValue::from("v")));
    assert_eq!(final_hits.len(), THREADS * PER);
}

// ---------------------------------------------------------------------------
// 7. Float handling: enabled index falls back to scan; NaN/-0.0 behave like ==
// ---------------------------------------------------------------------------
#[test]
fn float_falls_back_to_scan() {
    let db = AletheiaDB::new().expect("db");

    let pos_zero = db
        .create_node("M", PropertyMapBuilder::new().insert("v", 0.0f64).build())
        .expect("create +0");
    let neg_zero = db
        .create_node("M", PropertyMapBuilder::new().insert("v", -0.0f64).build())
        .expect("create -0");
    let nan = db
        .create_node("M", PropertyMapBuilder::new().insert("v", f64::NAN).build())
        .expect("create nan");

    // Oracle (scan) before enabling.
    let scan_zero = sorted(db.find_nodes_by_property("M", "v", &PropertyValue::Float(0.0)));
    let scan_nan = sorted(db.find_nodes_by_property("M", "v", &PropertyValue::Float(f64::NAN)));

    // Enable the index — Float is not indexable, so lookups still scan.
    db.property_index("M", "v").enable().expect("enable");
    assert!(db.has_property_index("M", "v"));

    // -0.0 == 0.0 under PropertyValue eq, so both zero nodes match; NaN != NaN.
    let idx_zero = sorted(db.find_nodes_by_property("M", "v", &PropertyValue::Float(0.0)));
    let idx_nan = sorted(db.find_nodes_by_property("M", "v", &PropertyValue::Float(f64::NAN)));

    assert_eq!(idx_zero, scan_zero);
    assert_eq!(idx_nan, scan_nan);
    assert_eq!(idx_zero, sorted(vec![pos_zero, neg_zero]));
    assert!(idx_nan.is_empty());
    let _ = nan;
}

// ---------------------------------------------------------------------------
// 8. Missing property: node without the indexed key never returned
// ---------------------------------------------------------------------------
#[test]
fn missing_property_never_returned() {
    let db = AletheiaDB::new().expect("db");
    db.property_index("Person", "email")
        .enable()
        .expect("enable");

    let with = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("email", "a@x").build(),
        )
        .expect("create with");
    let _without = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "NoEmail").build(),
        )
        .expect("create without");

    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "email", &PropertyValue::from("a@x"))),
        vec![with.as_u64()]
    );
    // No value lookup ever surfaces the node lacking the key.
    assert!(
        db.find_nodes_by_property("Person", "email", &PropertyValue::from(""))
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// 9. Transparent fallback when not covered + mid-life backfill on enable()
// ---------------------------------------------------------------------------
#[test]
fn fallback_when_not_covered_and_backfill_on_enable() {
    let db = AletheiaDB::new().expect("db");

    for i in 0..50i64 {
        db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("dept", if i % 2 == 0 { "eng" } else { "ops" })
                .build(),
        )
        .expect("create");
    }

    // No index enabled: pure scan.
    let before = sorted(db.find_nodes_by_property("Person", "dept", &PropertyValue::from("eng")));
    assert!(!db.has_property_index("Person", "dept"));
    assert_eq!(before.len(), 25);

    // Enabling backfills existing nodes; result identical.
    db.property_index("Person", "dept")
        .enable()
        .expect("enable");
    let after = sorted(db.find_nodes_by_property("Person", "dept", &PropertyValue::from("eng")));
    assert_eq!(before, after);

    // A different, non-indexed property still scans and returns correctly.
    let ops = sorted(db.find_nodes_by_property("Person", "dept", &PropertyValue::from("ops")));
    assert_eq!(ops.len(), 25);
}

// ---------------------------------------------------------------------------
// 10. Restart / WAL replay: state rebuilds; re-enable backfills from replay
// ---------------------------------------------------------------------------
#[test]
fn restart_wal_replay_rebuild() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let data_dir = tempdir.path().join("db");

    let ids: Vec<u64>;
    {
        let db = AletheiaDB::open(&data_dir).expect("open");
        db.property_index("Person", "email")
            .enable()
            .expect("enable");
        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("email", "a@x").build(),
            )
            .expect("create a");
        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("email", "a@x").build(),
            )
            .expect("create b");
        ids = sorted(vec![a, b]);
        // Deliberately NO persist_indexes(): force WAL replay on reopen.
        drop(db);
    }

    // Reopen: nodes replay via insert_node; opt-in state is in-memory only.
    let db = AletheiaDB::open(&data_dir).expect("reopen");

    // Scan fallback (index not re-enabled yet) still returns correct results —
    // proving no divergence.
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "email", &PropertyValue::from("a@x"))),
        ids
    );

    // Re-enabling backfills the index from the WAL-replayed current state.
    db.property_index("Person", "email")
        .enable()
        .expect("re-enable");
    assert!(db.has_property_index("Person", "email"));
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "email", &PropertyValue::from("a@x"))),
        ids
    );
}

// ---------------------------------------------------------------------------
// 11. Interned-key parity: never-interned label/prop => empty, no panic
// ---------------------------------------------------------------------------
#[test]
fn interned_key_parity() {
    let db = AletheiaDB::new().expect("db");
    db.property_index("Person", "email")
        .enable()
        .expect("enable");
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("email", "a@x").build(),
    )
    .expect("create");

    // Never-interned label and never-interned property both return empty.
    assert!(
        db.find_nodes_by_property("NeverLabel", "email", &PropertyValue::from("a@x"))
            .is_empty()
    );
    assert!(
        db.find_nodes_by_property("Person", "never_key", &PropertyValue::from("a@x"))
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// 12. Value-interning parity: distinct Arc<str> "Alice" key identically; drop
// ---------------------------------------------------------------------------
#[test]
fn value_key_parity_and_drop() {
    let db = AletheiaDB::new().expect("db");
    db.property_index("Person", "name")
        .enable()
        .expect("enable");

    // Two nodes whose "Alice" strings are built independently (distinct Arcs).
    let a = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", String::from("Alice"))
                .build(),
        )
        .expect("a");
    let b = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("b");

    // A freshly-built lookup value keys to the same bucket.
    assert_eq!(
        sorted(db.find_nodes_by_property(
            "Person",
            "name",
            &PropertyValue::from(String::from("Alice"))
        )),
        sorted(vec![a, b])
    );

    // list reflects the enabled index.
    let list = db.list_property_indexes();
    assert!(
        list.iter()
            .any(|i| i.label == "Person" && i.property == "name")
    );

    // Dropping removes it; lookups fall back to scan (still correct); has=false.
    db.drop_property_index("Person", "name").expect("drop");
    assert!(!db.has_property_index("Person", "name"));
    assert_eq!(
        sorted(db.find_nodes_by_property("Person", "name", &PropertyValue::from("Alice"))),
        sorted(vec![a, b])
    );
    // Dropping again is an error (nothing enabled).
    assert!(db.drop_property_index("Person", "name").is_err());
}
