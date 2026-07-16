//! Integration tests for property-type and required-key schema constraints
//! (Issue #3378).

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::EntityKind;
use aletheiadb::core::constraint::DeclaredType;
use aletheiadb::core::error::{ConstraintError, Error};
use aletheiadb::core::property::PropertyValue;
// All tests share the process-global `GLOBAL_INTERNER`, which the `.albk`
// restore path clears (`materialize_to_dir`). Run every test serially so a
// concurrent restore never wipes another test's interned label/property ids
// mid-run (mirrors `tests/backup_restore.rs`).
use serial_test::serial;

fn is_type_violation(e: &Error) -> bool {
    matches!(e, Error::Constraint(ConstraintError::TypeViolation { .. }))
}
fn is_missing_key(e: &Error) -> bool {
    matches!(
        e,
        Error::Constraint(ConstraintError::MissingRequiredKey { .. })
    )
}
fn is_non_conforming(e: &Error) -> bool {
    matches!(
        e,
        Error::Constraint(ConstraintError::NonConformingOnEnable { .. })
    )
}

fn person(name: &str, age: i64) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", name)
        .insert("age", age)
        .build()
}

// --- 1. reject wrong type on create_node and create_edge -------------------

#[test]
#[serial]
fn create_node_wrong_type_is_rejected() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();

    let bad = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", "thirty") // string, not int
        .build();
    let err = db.create_node("Person", bad).unwrap_err();
    assert!(is_type_violation(&err), "got {err:?}");

    // Conforming write still works and unrelated labels are unaffected.
    db.create_node("Person", person("Bob", 42)).unwrap();
    db.create_node(
        "Widget",
        PropertyMapBuilder::new().insert("age", "x").build(),
    )
    .unwrap();
}

#[test]
#[serial]
fn create_edge_wrong_type_is_rejected() {
    let db = AletheiaDB::new().unwrap();
    let a = db.create_node("Person", person("A", 1)).unwrap();
    let b = db.create_node("Person", person("B", 2)).unwrap();

    db.schema_constraint(EntityKind::Edge, "KNOWS")
        .require_typed("since", DeclaredType::Integer)
        .enable()
        .unwrap();

    let bad = PropertyMapBuilder::new()
        .insert("since", "yesterday")
        .build();
    let err = db.create_edge(a, b, "KNOWS", bad).unwrap_err();
    assert!(is_type_violation(&err), "got {err:?}");
}

// --- 2. reject missing required key on create_node and create_edge ---------

#[test]
#[serial]
fn create_node_missing_required_key_is_rejected() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require("name")
        .enable()
        .unwrap();

    let err = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap_err();
    assert!(is_missing_key(&err), "got {err:?}");
}

#[test]
#[serial]
fn create_edge_missing_required_key_is_rejected() {
    let db = AletheiaDB::new().unwrap();
    let a = db.create_node("Person", person("A", 1)).unwrap();
    let b = db.create_node("Person", person("B", 2)).unwrap();
    db.schema_constraint(EntityKind::Edge, "KNOWS")
        .require("since")
        .enable()
        .unwrap();

    let err = db
        .create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap_err();
    assert!(is_missing_key(&err), "got {err:?}");
}

// --- 3. accept conforming writes (node + edge) -----------------------------

#[test]
#[serial]
fn conforming_node_and_edge_are_accepted() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require("name")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();
    db.schema_constraint(EntityKind::Edge, "KNOWS")
        .require_typed("since", DeclaredType::Integer)
        .enable()
        .unwrap();

    let a = db.create_node("Person", person("Alice", 30)).unwrap();
    let b = db.create_node("Person", person("Bob", 25)).unwrap();
    db.create_edge(
        a,
        b,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020i64).build(),
    )
    .unwrap();
}

// --- 4. UPDATE (PATCH) effective-map semantics -----------------------------

#[test]
#[serial]
fn update_patch_effective_map_semantics() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require("name")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();
    let id = db.create_node("Person", person("Alice", 30)).unwrap();

    // Patch that leaves required keys intact (only touches age) passes.
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("age", 31i64).build(),
        None,
    )
    .unwrap();

    // Patch that changes a typed key to the wrong type fails.
    let err = db
        .update_node_with_valid_time(
            id,
            PropertyMapBuilder::new().insert("age", "oops").build(),
            None,
        )
        .unwrap_err();
    assert!(is_type_violation(&err), "got {err:?}");

    // Patch that nulls a required key fails (effective map has name = Null).
    let err = db
        .update_node_with_valid_time(
            id,
            PropertyMapBuilder::new()
                .insert("name", PropertyValue::Null)
                .build(),
            None,
        )
        .unwrap_err();
    assert!(is_missing_key(&err), "got {err:?}");

    // The node is still intact at age 31 (aborted updates changed nothing).
    let node = db.get_node(id).unwrap();
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(31)));
}

// --- 5. constraint-on-populated-label SUCCESS ------------------------------

#[test]
#[serial]
fn enable_on_conforming_populated_label_succeeds_then_enforces() {
    let db = AletheiaDB::new().unwrap();
    db.create_node("Person", person("Alice", 30)).unwrap();
    db.create_node("Person", person("Bob", 25)).unwrap();

    let report = db
        .schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();
    assert!(report.conforms);
    assert_eq!(report.total_checked, 2);
    assert_eq!(report.total_non_conforming, 0);

    // A subsequent violating write is now rejected.
    let err = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", "x").build(),
        )
        .unwrap_err();
    assert!(is_type_violation(&err), "got {err:?}");
}

// --- 6. constraint-on-populated-label FAILURE ------------------------------

#[test]
#[serial]
fn enable_on_nonconforming_populated_label_errors_with_report() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db.create_node("Person", person("Alice", 30)).unwrap();
    let _n2 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap(); // missing age

    let err = db
        .schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap_err();
    assert!(is_non_conforming(&err), "got {err:?}");
    match err {
        Error::Constraint(ConstraintError::NonConformingOnEnable {
            total_non_conforming,
            sample_ids,
            violations,
            ..
        }) => {
            assert_eq!(total_non_conforming, 1);
            assert!(!sample_ids.is_empty());
            assert!(!violations.is_empty());
        }
        other => panic!("wrong error: {other:?}"),
    }

    // Nothing was declared: a would-be-violating write still succeeds.
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("name", "Carol").build(),
    )
    .unwrap();
    assert!(db.list_schema_constraints().is_empty());
    let _ = n1;
}

// --- 7. dry_run returns the report without applying ------------------------

#[test]
#[serial]
fn dry_run_reports_without_applying() {
    let db = AletheiaDB::new().unwrap();
    db.create_node("Person", person("Alice", 30)).unwrap();
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("name", "Bob").build(),
    )
    .unwrap(); // missing age

    // Non-conforming dry-run: conforms=false, but nothing declared/enforced.
    let report = db
        .schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .dry_run()
        .enable()
        .unwrap();
    assert!(!report.conforms);
    assert_eq!(report.total_non_conforming, 1);
    assert!(db.list_schema_constraints().is_empty());

    // A later violating write is NOT enforced (dry-run declared nothing).
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("age", "x").build(),
    )
    .unwrap();

    // Conforming dry-run reports conforms=true, still declares nothing.
    let db2 = AletheiaDB::new().unwrap();
    db2.create_node("Person", person("Alice", 30)).unwrap();
    let report2 = db2
        .schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .dry_run()
        .enable()
        .unwrap();
    assert!(report2.conforms);
    assert!(db2.list_schema_constraints().is_empty());
}

// --- 8. restart durability -------------------------------------------------

#[test]
#[serial]
fn constraints_survive_restart_and_drop_persists() {
    let dir = tempfile::tempdir().unwrap();

    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.schema_constraint(EntityKind::Node, "Person")
            .require_typed("age", DeclaredType::Integer)
            .enable()
            .unwrap();
        db.schema_constraint(EntityKind::Node, "Widget")
            .require("sku")
            .enable()
            .unwrap();
        // Drop one; the other must remain after reopen.
        assert!(
            db.drop_schema_constraint(EntityKind::Node, "Widget")
                .unwrap()
        );
    }

    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        // Person constraint still enforced after reopen.
        let err = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("age", "x").build(),
            )
            .unwrap_err();
        assert!(is_type_violation(&err), "got {err:?}");
        // Widget constraint was dropped: no enforcement.
        db.create_node("Widget", PropertyMapBuilder::new().build())
            .unwrap();

        let listed = db.list_schema_constraints();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "Person");
    }
}

// --- 9. .albk backup -> restore round-trip preserves constraints -----------

#[test]
#[serial]
fn backup_restore_preserves_constraints() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .require("name")
        .enable()
        .unwrap();
    db.create_node("Person", person("Alice", 30)).unwrap();

    let backup_dir = tempfile::tempdir().unwrap();
    let backup_path = backup_dir.path().join("snap.albk");
    db.backup(&backup_path).unwrap();

    let restored = AletheiaDB::restore(&backup_path).unwrap();

    // Constraints active after restore.
    let err = restored
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", "x").build(),
        )
        .unwrap_err();
    assert!(is_type_violation(&err), "got {err:?}");

    let listed = restored.list_schema_constraints();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "Person");
    assert_eq!(listed[0].properties.len(), 2);
}

#[test]
#[serial]
fn backup_restore_to_data_dir_preserves_constraints() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Edge, "KNOWS")
        .require_typed("since", DeclaredType::Integer)
        .enable()
        .unwrap();

    let backup_dir = tempfile::tempdir().unwrap();
    let backup_path = backup_dir.path().join("snap.albk");
    db.backup(&backup_path).unwrap();

    let data_dir = tempfile::tempdir().unwrap();
    let restored = AletheiaDB::restore_to_data_dir(&backup_path, data_dir.path()).unwrap();
    drop(restored);

    // Reopen the durable restored dir: constraint must still be present.
    let reopened = AletheiaDB::open(data_dir.path()).unwrap();
    let listed = reopened.list_schema_constraints();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].entity_kind, "edge");
    assert_eq!(listed[0].label, "KNOWS");
}

// --- 10. get_schema reports declared vs observed ---------------------------

#[test]
#[serial]
fn get_schema_distinguishes_declared_and_observed() {
    let db = AletheiaDB::new().unwrap();
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("nickname", "Al")
            .build(),
    )
    .unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer) // declared but not observed
        .require("name") // declared AND observed
        .enable()
        .unwrap_err(); // age missing on the existing node -> non-conforming

    // Declare on a clean db to actually attach declared_constraints.
    let db2 = AletheiaDB::new().unwrap();
    db2.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("nickname", "Al")
            .build(),
    )
    .unwrap();
    db2.schema_constraint(EntityKind::Node, "Person")
        .require("name")
        .typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();

    let schema = db2.schema().unwrap();
    let person = schema
        .node_labels
        .iter()
        .find(|l| l.label == "Person")
        .unwrap();
    // Observed keys come from the stored node.
    assert!(person.property_keys.contains(&"name".to_string()));
    assert!(person.property_keys.contains(&"nickname".to_string()));
    assert!(!person.property_keys.contains(&"age".to_string()));
    // Declared constraints report the declared keys (incl. not-yet-observed age).
    let declared: Vec<&str> = person
        .declared_constraints
        .iter()
        .map(|c| c.property.as_str())
        .collect();
    assert!(declared.contains(&"name"));
    assert!(declared.contains(&"age"));
}

// --- 11. temporal: history not retroactively invalidated -------------------

#[test]
#[serial]
fn history_not_retroactively_invalidated() {
    let db = AletheiaDB::new().unwrap();

    // Old version has a STRING age (would violate an Integer constraint).
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", "thirty").build(),
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mid = aletheiadb::core::temporal::time::now();
    std::thread::sleep(std::time::Duration::from_millis(5));

    // Update to an INTEGER age so current state conforms.
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("age", 30i64).build(),
        None,
    )
    .unwrap();

    // Enable scans CURRENT state only (age=30 int) -> succeeds despite the old
    // string version existing in history.
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();

    // Time-travel read of the OLD version still returns the string (history
    // preserved, never invalidated; reads are never blocked by constraints).
    let old = db.get_node_at_time(id, mid, mid).unwrap();
    assert_eq!(
        old.properties.get("age"),
        Some(&PropertyValue::String("thirty".into()))
    );
}

// --- 12. AC7: backdated write validated against current constraints --------

#[test]
#[serial]
fn backdated_write_validated_against_current_constraints() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();

    // A backdated (valid_time in the past) create still records tx-time = now,
    // so it is validated against the currently-active constraint set.
    let now = aletheiadb::core::temporal::time::now();
    let past = aletheiadb::core::hlc::HybridTimestamp::new(
        now.wallclock() - 86_400_000_000, // ~1 day earlier
        0,
    )
    .unwrap();
    let err = db
        .create_node_with_valid_time(
            "Person",
            PropertyMapBuilder::new().insert("age", "x").build(),
            Some(past),
        )
        .unwrap_err();
    assert!(is_type_violation(&err), "got {err:?}");
}

// --- 13. atomicity: one violating op aborts the whole transaction ----------

#[test]
#[serial]
fn multi_op_transaction_is_atomic_on_violation() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();

    let result: Result<(), Error> = db.write(|tx| {
        tx.create_node("Person", person("Good", 1))?;
        // Second op violates -> whole tx must abort.
        tx.create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", "bad").build(),
        )?;
        Ok(())
    });
    assert!(result.is_err());
    assert!(is_type_violation(&result.unwrap_err()));

    // Zero writes took effect (including the first, valid op).
    assert!(db.get_nodes_by_label("Person").is_empty());
}

// --- 14. bulk import enforces constraints ----------------------------------
// Gated on the `import` feature (the importer is compiled only under it). Run
// with `cargo test --test schema_constraints --features import`.

#[cfg(feature = "import")]
#[test]
#[serial]
fn bulk_import_enforces_constraints() {
    use aletheiadb::api::import::{ColumnType, LabelSource, NodeMapping};

    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("age", DeclaredType::Integer)
        .enable()
        .unwrap();

    // One row is a non-integer age -> the shared commit path rejects it.
    let dir = tempfile::tempdir().unwrap();
    let jsonl = dir.path().join("people.jsonl");
    std::fs::write(
        &jsonl,
        "{\"id\":\"1\",\"age\":30}\n{\"id\":\"2\",\"age\":\"oops\"}\n",
    )
    .unwrap();

    let mapping = NodeMapping::new(LabelSource::fixed("Person"), "id").property(
        "age",
        "age",
        ColumnType::String,
    );
    let mut importer = db.import();
    let result = importer.nodes_from_jsonl(&jsonl, mapping);
    assert!(result.is_err(), "import should fail on the violating row");

    // FIX-6: both rows share one chunk/transaction, so in-chunk atomicity means
    // the VALID row must also not persist — zero writes take effect.
    assert!(
        db.get_nodes_by_label("Person").is_empty(),
        "a rejected import chunk must leave zero Person nodes (in-chunk atomicity)"
    );
}

// --- 15. vector-with-dimension ---------------------------------------------

#[test]
#[serial]
fn vector_dimension_constraint() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Doc")
        .require_typed("embedding", DeclaredType::Vector { dim: Some(3) })
        .enable()
        .unwrap();

    // Right dimension accepted.
    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector([0.1, 0.2, 0.3]))
            .build(),
    )
    .unwrap();

    // Wrong dimension rejected.
    let err = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("embedding", PropertyValue::vector([0.1, 0.2]))
                .build(),
        )
        .unwrap_err();
    assert!(is_type_violation(&err), "got {err:?}");
}

// --- 16. opt-in / zero behavior change -------------------------------------

#[test]
#[serial]
fn schemaless_label_accepts_anything() {
    let db = AletheiaDB::new().unwrap();
    // No declaration for "Anything": any shape is accepted.
    db.create_node(
        "Anything",
        PropertyMapBuilder::new().insert("x", 1i64).build(),
    )
    .unwrap();
    db.create_node(
        "Anything",
        PropertyMapBuilder::new().insert("x", "str").build(),
    )
    .unwrap();
    db.create_node("Anything", PropertyMapBuilder::new().build())
        .unwrap();
    assert_eq!(db.get_nodes_by_label("Anything").len(), 3);
}

// --- 17. corrupt sidecar is quarantined, DB still opens --------------------

#[test]
#[serial]
fn corrupt_sidecar_is_quarantined_and_db_opens() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.schema_constraint(EntityKind::Node, "Person")
            .require("name")
            .enable()
            .unwrap();
    }
    // Corrupt the sidecar file.
    let sidecar = dir.path().join("schema_constraints.dat");
    assert!(sidecar.exists(), "sidecar should have been written");
    std::fs::write(&sidecar, b"not a valid bitcode+crc blob").unwrap();

    // DB still opens; constraints are dropped (empty), file quarantined.
    let db = AletheiaDB::open(dir.path()).unwrap();
    assert!(db.list_schema_constraints().is_empty());
    // A write that would have violated now succeeds (no active constraint).
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("age", 1i64).build(),
    )
    .unwrap();
    assert!(!sidecar.exists(), "corrupt sidecar should be renamed aside");
}

// --- 18. FIX-1: concurrent enable persists ALL constraints to disk ---------

#[test]
#[serial]
fn concurrent_enable_persists_all_constraints() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    const N: usize = 8;

    {
        let db = Arc::new(AletheiaDB::open(dir.path()).unwrap());
        let mut handles = Vec::new();
        for i in 0..N {
            let db = Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                let label = format!("Label{i}");
                db.schema_constraint(EntityKind::Node, &label)
                    .require_typed("id", DeclaredType::Integer)
                    .enable()
                    .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // In-memory: all N present.
        assert_eq!(db.list_schema_constraints().len(), N);
    }

    // Reopen the DB: the on-disk sidecar must carry ALL N constraints. Without
    // the DDL mutex the enable/drop read-modify-write races and the last file
    // write silently drops the other labels — this reload is where that bug
    // would manifest.
    let reopened = AletheiaDB::open(dir.path()).unwrap();
    let listed = reopened.list_schema_constraints();
    assert_eq!(
        listed.len(),
        N,
        "all {N} concurrently-enabled constraints must survive restart, got {}",
        listed.len()
    );
    for i in 0..N {
        let label = format!("Label{i}");
        assert!(
            listed.iter().any(|c| c.label == label),
            "missing {label} on disk after restart"
        );
    }
}

// --- 19. FIX-4: mixed per-key nullability in one chain ----------------------

#[test]
#[serial]
fn mixed_per_key_nullability() {
    let db = AletheiaDB::new().unwrap();
    db.schema_constraint(EntityKind::Node, "Person")
        .typed("nickname", DeclaredType::String) // nullable (builder default)
        .typed_nullable("email", DeclaredType::String, false) // non-nullable
        .enable()
        .unwrap();

    // Present explicit Null on the nullable key is accepted.
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("nickname", PropertyValue::Null)
            .build(),
    )
    .unwrap();

    // Present explicit Null on the non-nullable key is rejected.
    let err = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("email", PropertyValue::Null)
                .build(),
        )
        .unwrap_err();
    assert!(is_missing_key(&err), "got {err:?}");

    // A conforming write (typed string on both, or omitting the optional keys)
    // still succeeds.
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("nickname", "Al")
            .insert("email", "al@example.com")
            .build(),
    )
    .unwrap();
}
