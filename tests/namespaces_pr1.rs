//! Namespaces PR1 — core model integration tests (Issue #3349).
//!
//! Covers the design doc's T1–T3, T9 (registry + ride-along replay), T10, and
//! the hard-requirement reserved-key elision SWEEP.

use aletheiadb::core::namespace::{NAMESPACE_KEY, is_reserved_property_key};
use aletheiadb::{AletheiaDB, Namespace, NamespaceError, PropertyMapBuilder, PropertyValue};

fn props(name: &str) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

// ============================================================================
// T1 — back-compat: omitted namespace resolves to `default`.
// ============================================================================

#[test]
fn t1_omitted_namespace_resolves_to_default() {
    let db = AletheiaDB::new().unwrap();
    let id = db.create_node("Person", props("Alice")).unwrap();
    let node = db.get_node(id).unwrap();

    // A node created with no namespace lives in `default`...
    assert_eq!(node.namespace(), Namespace::default());
    // ...and carries NO ride-along key (byte-identical to legacy data).
    assert!(!node.properties.contains_key(NAMESPACE_KEY));
    // The user-facing view is unchanged from pre-namespace behavior.
    assert_eq!(
        node.user_properties()
            .get("name")
            .and_then(PropertyValue::as_str),
        Some("Alice")
    );
    assert_eq!(node.user_properties().len(), node.properties.len());
}

#[test]
fn t1_create_in_default_namespace_equals_plain_create() {
    let db = AletheiaDB::new().unwrap();
    let plain = db.create_node("Person", props("A")).unwrap();
    let explicit = db
        .create_node_in_namespace("Person", props("B"), "default")
        .unwrap();

    // Neither carries the ride-along key; both resolve to default.
    assert!(
        !db.get_node(plain)
            .unwrap()
            .properties
            .contains_key(NAMESPACE_KEY)
    );
    assert!(
        !db.get_node(explicit)
            .unwrap()
            .properties
            .contains_key(NAMESPACE_KEY)
    );
    assert_eq!(
        db.get_node(explicit).unwrap().namespace(),
        Namespace::default()
    );
}

// ============================================================================
// T2 — reserved key: user writes carrying a reserved key are rejected + elided.
// ============================================================================

#[test]
fn t2_reserved_key_rejected_on_create() {
    let db = AletheiaDB::new().unwrap();
    for bad_key in [NAMESPACE_KEY, "__aletheia_foo", "__shred_x"] {
        let p = PropertyMapBuilder::new().insert(bad_key, "x").build();
        let err = db.create_node("Person", p).unwrap_err();
        assert!(
            matches!(
                err,
                aletheiadb::Error::Namespace(NamespaceError::ReservedPropertyKey { .. })
            ),
            "expected ReservedPropertyKey for {bad_key}, got {err:?}"
        );
    }
}

#[test]
fn t2_reserved_key_rejected_on_update_and_edge() {
    let db = AletheiaDB::new().unwrap();
    let a = db.create_node("Person", props("A")).unwrap();
    let b = db.create_node("Person", props("B")).unwrap();

    // update PATCH carrying a reserved key -> rejected.
    let bad = PropertyMapBuilder::new()
        .insert("__aletheia_ns", "forged")
        .build();
    assert!(matches!(
        db.update_node_with_valid_time(a, bad, None).unwrap_err(),
        aletheiadb::Error::Namespace(NamespaceError::ReservedPropertyKey { .. })
    ));

    // create_edge carrying a reserved key -> rejected.
    let bad_edge = PropertyMapBuilder::new().insert("__shred_y", 1).build();
    assert!(matches!(
        db.create_edge(a, b, "KNOWS", bad_edge).unwrap_err(),
        aletheiadb::Error::Namespace(NamespaceError::ReservedPropertyKey { .. })
    ));
}

#[test]
fn t2_reserved_key_elided_from_views() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node_in_namespace("Person", props("Alice"), "agent:planner")
        .unwrap();
    let node = db.get_node(id).unwrap();

    // Raw storage carries the ride-along key...
    assert!(node.properties.contains_key(NAMESPACE_KEY));
    // ...but it is elided from the user-facing view and surfaced as `namespace`.
    let view = node.user_properties();
    assert!(!view.contains_key(NAMESPACE_KEY));
    assert_eq!(node.namespace().as_str(), "agent:planner");
    for (key, _) in view.iter() {
        let name = aletheiadb::GLOBAL_INTERNER
            .resolve_with(*key, |s| s.to_string())
            .unwrap();
        assert!(
            !is_reserved_property_key(&name),
            "reserved key {name} leaked"
        );
    }
}

// ============================================================================
// T3 — immutability / re-stamp across update PATCH, replace, retract.
// ============================================================================

#[test]
fn t3_update_patch_preserves_namespace() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node_in_namespace("Person", props("Alice"), "agent:planner")
        .unwrap();

    // A PATCH that does not (and cannot) touch the namespace key preserves it.
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("age", 30).build(),
        None,
    )
    .unwrap();
    let node = db.get_node(id).unwrap();
    assert_eq!(node.namespace().as_str(), "agent:planner");
    assert_eq!(
        node.get_property("age").and_then(PropertyValue::as_int),
        Some(30)
    );
}

#[test]
fn t3_replace_node_preserves_namespace() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node_in_namespace("Person", props("Alice"), "agent:planner")
        .unwrap();

    // A full overwrite (new label + entirely new map, no ns key) must NOT drop
    // the immutable namespace.
    db.replace_node(
        id,
        "Human",
        PropertyMapBuilder::new().insert("name", "Alice2").build(),
    )
    .unwrap();
    let node = db.get_node(id).unwrap();
    assert_eq!(node.namespace().as_str(), "agent:planner");
    assert!(node.has_label_str("Human"));
}

#[test]
fn t3_retract_preserves_namespace_in_history() {
    use aletheiadb::core::temporal::time;
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node_in_namespace("Person", props("Alice"), "agent:planner")
        .unwrap();

    // Capture a valid-time coordinate within the node's validity, then close
    // its valid interval strictly after it. A point-in-time read at the earlier
    // coordinate must still reconstruct the node with its original namespace —
    // retraction never moves an entity between namespaces (it lives in history).
    let vt = time::now();
    let retract_at = time::from_secs(vt.wallclock() / 1_000_000 + 3_600);
    db.retract_node(id, retract_at).unwrap();
    let node = db.get_node_at_valid_time(id, vt).unwrap();
    assert_eq!(node.namespace().as_str(), "agent:planner");
}

#[test]
fn t3_no_api_moves_entity_between_namespaces() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    // Even a user PATCH attempting to set the key is rejected (T2), so there is
    // no path to change the namespace. Confirm after a benign update it is
    // still agent:a.
    db.update_node_with_valid_time(id, PropertyMapBuilder::new().insert("k", "v").build(), None)
        .unwrap();
    assert_eq!(db.get_node(id).unwrap().namespace().as_str(), "agent:a");
}

// ============================================================================
// T9 — registry durability + ride-along survives WAL replay.
// ============================================================================

#[test]
fn t9_registry_and_ride_along_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");

    let node_id;
    {
        let db = AletheiaDB::open(&path).unwrap();
        db.create_namespace("agent:planner", Some("planner scope".to_string()))
            .unwrap();
        db.create_namespace("agent:researcher", None).unwrap();
        // Auto-register on write to a brand-new namespace.
        node_id = db
            .create_node_in_namespace("Person", props("Alice"), "session:run1")
            .unwrap();
    }

    // Reopen: registry entries survive AND the ride-along namespace resolves
    // after WAL replay / index load.
    {
        let db = AletheiaDB::open(&path).unwrap();
        let names: Vec<String> = db.list_namespaces().into_iter().map(|i| i.name).collect();
        assert!(names.contains(&"default".to_string()));
        assert!(names.contains(&"agent:planner".to_string()));
        assert!(names.contains(&"agent:researcher".to_string()));
        // Auto-registered namespace persisted too.
        assert!(names.contains(&"session:run1".to_string()));
        assert_eq!(
            db.describe_namespace("agent:planner").unwrap().description,
            Some("planner scope".to_string())
        );
        // Ride-along survived replay.
        assert_eq!(
            db.get_node(node_id).unwrap().namespace().as_str(),
            "session:run1"
        );
    }
}

// ============================================================================
// T10 — structured errors with the offending value.
// ============================================================================

#[test]
fn t10_invalid_and_empty_names_are_invalid_argument() {
    let db = AletheiaDB::new().unwrap();
    for bad in ["", "has space", "all", "comma,x"] {
        let err = db.create_namespace(bad, None).unwrap_err();
        match err {
            aletheiadb::Error::Namespace(NamespaceError::InvalidName { name, .. }) => {
                assert_eq!(name, bad, "offending value carried in error");
            }
            other => panic!("expected InvalidName for {bad:?}, got {other:?}"),
        }
    }
    // Namespaced create with a bad name is INVALID_ARGUMENT too.
    assert!(matches!(
        db.create_node_in_namespace("Person", props("A"), "bad space")
            .unwrap_err(),
        aletheiadb::Error::Namespace(NamespaceError::InvalidName { .. })
    ));
}

#[test]
fn t10_unknown_describe_and_delete_are_not_found() {
    let db = AletheiaDB::new().unwrap();
    match db.describe_namespace("nope").unwrap_err() {
        aletheiadb::Error::Namespace(NamespaceError::NotFound { namespace }) => {
            assert_eq!(namespace, "nope");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    match db.delete_namespace("nope").unwrap_err() {
        aletheiadb::Error::Namespace(NamespaceError::NotFound { namespace }) => {
            assert_eq!(namespace, "nope");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    // default cannot be created (CONFLICT) or deleted (INVALID_ARGUMENT).
    assert!(matches!(
        db.create_namespace("default", None).unwrap_err(),
        aletheiadb::Error::Namespace(NamespaceError::AlreadyExists { .. })
    ));
    assert!(matches!(
        db.delete_namespace("default").unwrap_err(),
        aletheiadb::Error::Namespace(NamespaceError::InvalidName { .. })
    ));
}

// ============================================================================
// SWEEP — the reserved ride-along key never leaks into any user-facing payload.
// ============================================================================

/// Assert a serialized property map contains no engine-reserved key bytes.
fn assert_no_reserved_bytes(map: &aletheiadb::PropertyMap, context: &str) {
    let bytes = map.serialize().expect("serialize");
    let needle = NAMESPACE_KEY.as_bytes();
    let leaked = bytes.windows(needle.len()).any(|w| w == needle);
    assert!(!leaked, "reserved key leaked into serialized {context}");
}

#[test]
fn sweep_reserved_key_never_leaks_user_facing() {
    let db = AletheiaDB::new().unwrap();

    // Nodes and edges in non-default namespaces.
    let a = db
        .create_node_in_namespace("Person", props("Alice"), "agent:planner")
        .unwrap();
    let b = db
        .create_node_in_namespace("Person", props("Bob"), "agent:researcher")
        .unwrap();
    let e = db
        .create_edge_in_namespace(a, b, "KNOWS", props("edge"), "shared")
        .unwrap();

    // Every Rust-level user-facing conversion elides the ride-along key.
    let na = db.get_node(a).unwrap();
    let nb = db.get_node(b).unwrap();
    let ee = db.get_edge(e).unwrap();

    for (view, ns) in [
        (na.user_properties(), "agent:planner"),
        (nb.user_properties(), "agent:researcher"),
    ] {
        assert!(!view.contains_key(NAMESPACE_KEY));
        assert_no_reserved_bytes(&view, "node view");
        let _ = ns;
    }
    assert_eq!(na.namespace().as_str(), "agent:planner");
    assert_eq!(nb.namespace().as_str(), "agent:researcher");
    assert_eq!(ee.namespace().as_str(), "shared");
    assert!(!ee.user_properties().contains_key(NAMESPACE_KEY));
    assert_no_reserved_bytes(&ee.user_properties(), "edge view");

    // The public user-facing accessor is airtight too.
    let stripped = na.user_properties();
    assert!(!stripped.contains_key(NAMESPACE_KEY));
}

#[test]
fn sweep_backup_restore_preserves_namespace_and_elides() {
    let dir = tempfile::tempdir().unwrap();
    let backup_path = dir.path().join("snap.albk");

    let (a, b, e);
    {
        let db = AletheiaDB::new().unwrap();
        a = db
            .create_node_in_namespace("Person", props("Alice"), "agent:planner")
            .unwrap();
        b = db
            .create_node_in_namespace("Person", props("Bob"), "agent:researcher")
            .unwrap();
        e = db
            .create_edge_in_namespace(a, b, "KNOWS", props("edge"), "shared")
            .unwrap();
        db.backup(&backup_path).unwrap();
    }

    // Restore into a fresh database: namespaces survive (internal round-trip)
    // AND user-facing views still elide the ride-along key.
    let restored = AletheiaDB::restore(&backup_path).unwrap();
    let na = restored.get_node(a).unwrap();
    let nb = restored.get_node(b).unwrap();
    let ee = restored.get_edge(e).unwrap();

    assert_eq!(na.namespace().as_str(), "agent:planner");
    assert_eq!(nb.namespace().as_str(), "agent:researcher");
    assert_eq!(ee.namespace().as_str(), "shared");

    assert!(!na.user_properties().contains_key(NAMESPACE_KEY));
    assert!(!ee.user_properties().contains_key(NAMESPACE_KEY));
    assert_no_reserved_bytes(&na.user_properties(), "restored node view");
    assert_no_reserved_bytes(&ee.user_properties(), "restored edge view");
}

// ============================================================================
// Reserved-key rejection across EVERY write seam (T2, exhaustive).
// ============================================================================

fn reserved_map(key: &str) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new().insert(key, "x").build()
}

fn is_reserved_err(err: &aletheiadb::Error) -> bool {
    matches!(
        err,
        aletheiadb::Error::Namespace(NamespaceError::ReservedPropertyKey { .. })
    )
}

#[test]
fn reserved_key_rejected_on_every_property_map_seam() {
    use aletheiadb::PropertyValue;
    use aletheiadb::core::temporal::time;

    for forged in ["__aletheia_ns", "__shred_x"] {
        let db = AletheiaDB::new().unwrap();
        let a = db.create_node("Person", props("A")).unwrap();
        let b = db.create_node("Person", props("B")).unwrap();
        let e = db.create_edge(a, b, "KNOWS", props("edge")).unwrap();

        // update_edge (PATCH)
        assert!(
            is_reserved_err(
                &db.update_edge_with_valid_time(e, reserved_map(forged), None)
                    .unwrap_err()
            ),
            "update_edge must reject {forged}"
        );
        // replace_node (full overwrite)
        assert!(
            is_reserved_err(
                &db.replace_node(a, "Person", reserved_map(forged))
                    .unwrap_err()
            ),
            "replace_node must reject {forged}"
        );
        // replace_edge (full overwrite)
        assert!(
            is_reserved_err(&db.replace_edge(e, reserved_map(forged)).unwrap_err()),
            "replace_edge must reject {forged}"
        );
        // compare_and_set_node
        let nv = db.get_node(a).unwrap().current_version;
        assert!(
            is_reserved_err(
                &db.compare_and_set_node(a, nv, reserved_map(forged))
                    .unwrap_err()
            ),
            "compare_and_set_node must reject {forged}"
        );
        // compare_and_set_edge
        let ev = db.get_edge(e).unwrap().current_version;
        assert!(
            is_reserved_err(
                &db.compare_and_set_edge(e, ev, reserved_map(forged))
                    .unwrap_err()
            ),
            "compare_and_set_edge must reject {forged}"
        );
        // claim_with_lease
        let nv2 = db.get_node(a).unwrap().current_version;
        let lease_until = time::from_secs(time::now().wallclock() / 1_000_000 + 3_600);
        assert!(
            is_reserved_err(
                &db.claim_with_lease(
                    a,
                    nv2,
                    "lease_owner",
                    "lease_until",
                    PropertyValue::from("worker-1"),
                    lease_until,
                    reserved_map(forged),
                )
                .unwrap_err()
            ),
            "claim_with_lease must reject {forged}"
        );
        // remove_node_property / remove_edge_property (key is the forged one)
        assert!(
            is_reserved_err(&db.remove_node_property(a, forged).unwrap_err()),
            "remove_node_property must reject {forged}"
        );
        assert!(
            is_reserved_err(&db.remove_edge_property(e, forged).unwrap_err()),
            "remove_edge_property must reject {forged}"
        );
    }
}

// ============================================================================
// Edge + CAS immutability re-stamp: every non-create path preserves the ns.
// ============================================================================

#[test]
fn edge_and_cas_paths_preserve_immutable_namespace() {
    use aletheiadb::PropertyValue;
    use aletheiadb::core::temporal::time;

    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node_in_namespace("Person", props("Alice"), "agent:planner")
        .unwrap();
    let b = db
        .create_node_in_namespace("Person", props("Bob"), "agent:planner")
        .unwrap();
    let e = db
        .create_edge_in_namespace(a, b, "KNOWS", props("edge"), "agent:planner")
        .unwrap();

    // update_edge PATCH
    db.update_edge_with_valid_time(e, PropertyMapBuilder::new().insert("k", 1).build(), None)
        .unwrap();
    assert_eq!(
        db.get_edge(e).unwrap().namespace().as_str(),
        "agent:planner"
    );

    // replace_edge full overwrite (no ns key in the new map)
    db.replace_edge(e, PropertyMapBuilder::new().insert("since", 2021).build())
        .unwrap();
    assert_eq!(
        db.get_edge(e).unwrap().namespace().as_str(),
        "agent:planner"
    );

    // compare_and_set_node
    let nv = db.get_node(a).unwrap().current_version;
    db.compare_and_set_node(
        a,
        nv,
        PropertyMapBuilder::new().insert("name", "Alice3").build(),
    )
    .unwrap();
    assert_eq!(
        db.get_node(a).unwrap().namespace().as_str(),
        "agent:planner"
    );

    // compare_and_set_edge
    let ev = db.get_edge(e).unwrap().current_version;
    db.compare_and_set_edge(
        e,
        ev,
        PropertyMapBuilder::new().insert("since", 2022).build(),
    )
    .unwrap();
    assert_eq!(
        db.get_edge(e).unwrap().namespace().as_str(),
        "agent:planner"
    );

    // claim_with_lease
    let nv2 = db.get_node(a).unwrap().current_version;
    let lease_until = time::from_secs(time::now().wallclock() / 1_000_000 + 3_600);
    db.claim_with_lease(
        a,
        nv2,
        "lease_owner",
        "lease_until",
        PropertyValue::from("worker-1"),
        lease_until,
        PropertyMapBuilder::new().insert("name", "Alice4").build(),
    )
    .unwrap();
    assert_eq!(
        db.get_node(a).unwrap().namespace().as_str(),
        "agent:planner"
    );
}

// ============================================================================
// Adversarial "cannot move": forged ns rejected; clean re-stamp keeps original;
// explicit namespace on a non-create op is INVALID_ARGUMENT (never a silent no-op).
// ============================================================================

#[test]
fn forged_namespace_on_replace_and_cas_is_rejected() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    let b = db
        .create_node_in_namespace("Person", props("Bob"), "agent:a")
        .unwrap();
    let e = db
        .create_edge_in_namespace(a, b, "KNOWS", props("edge"), "agent:a")
        .unwrap();

    // A forged `__aletheia_ns="other"` in the overwrite map is rejected as a
    // reserved key — it can never be applied to move the entity.
    let forged = PropertyMapBuilder::new()
        .insert("name", "Alice2")
        .insert(NAMESPACE_KEY, "other")
        .build();
    assert!(is_reserved_err(
        &db.replace_node(a, "Person", forged.clone()).unwrap_err()
    ));
    let forged_edge = PropertyMapBuilder::new()
        .insert(NAMESPACE_KEY, "other")
        .build();
    assert!(is_reserved_err(
        &db.replace_edge(e, forged_edge).unwrap_err()
    ));
    let nv = db.get_node(a).unwrap().current_version;
    assert!(is_reserved_err(
        &db.compare_and_set_node(a, nv, forged).unwrap_err()
    ));
    // Still in the original namespace after all rejected attempts.
    assert_eq!(db.get_node(a).unwrap().namespace().as_str(), "agent:a");
}

#[test]
fn explicit_namespace_on_non_create_is_invalid_argument() {
    use aletheiadb::api::transaction::WriteRequestOptions;
    use aletheiadb::core::temporal::time;
    use aletheiadb::{PropertyValue, core::namespace::Namespace as Ns};

    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    let b = db
        .create_node_in_namespace("Person", props("Bob"), "agent:a")
        .unwrap();
    let e = db
        .create_edge_in_namespace(a, b, "KNOWS", props("edge"), "agent:a")
        .unwrap();

    let other = Ns::new("agent:b").unwrap();
    let is_immutable = |err: &aletheiadb::Error| {
        matches!(err, aletheiadb::Error::Namespace(NamespaceError::Immutable))
    };

    // update_node
    let opts = WriteRequestOptions::new().with_namespace(other.clone());
    assert!(is_immutable(
        &db.update_node_with_options(a, props("x"), opts)
            .unwrap_err()
    ));
    // update_edge
    let opts = WriteRequestOptions::new().with_namespace(other.clone());
    assert!(is_immutable(
        &db.update_edge_with_options(e, props("x"), opts)
            .unwrap_err()
    ));
    // replace_node
    let opts = WriteRequestOptions::new().with_namespace(other.clone());
    assert!(is_immutable(
        &db.replace_node_with_options(a, "Person", props("x"), opts)
            .unwrap_err()
    ));
    // replace_edge
    let opts = WriteRequestOptions::new().with_namespace(other.clone());
    assert!(is_immutable(
        &db.replace_edge_with_options(e, props("x"), opts)
            .unwrap_err()
    ));
    // compare_and_set_node
    let nv = db.get_node(a).unwrap().current_version;
    let opts = WriteRequestOptions::new().with_namespace(other.clone());
    assert!(is_immutable(
        &db.compare_and_set_node_with_options(a, nv, props("x"), opts)
            .unwrap_err()
    ));
    // compare_and_set_edge
    let ev = db.get_edge(e).unwrap().current_version;
    let opts = WriteRequestOptions::new().with_namespace(other.clone());
    assert!(is_immutable(
        &db.compare_and_set_edge_with_options(e, ev, props("x"), opts)
            .unwrap_err()
    ));
    // claim_with_lease
    let nv2 = db.get_node(a).unwrap().current_version;
    let lease_until = time::from_secs(time::now().wallclock() / 1_000_000 + 3_600);
    let opts = WriteRequestOptions::new().with_namespace(other);
    assert!(is_immutable(
        &db.claim_with_lease_with_options(
            a,
            nv2,
            "lease_owner",
            "lease_until",
            PropertyValue::from("worker-1"),
            lease_until,
            props("x"),
            opts,
        )
        .unwrap_err()
    ));

    // Nothing moved: still agent:a.
    assert_eq!(db.get_node(a).unwrap().namespace().as_str(), "agent:a");
    assert_eq!(db.get_edge(e).unwrap().namespace().as_str(), "agent:a");
}

// ============================================================================
// SHOULD-FIX #3 — a create that fails validation leaves NO registered namespace.
// ============================================================================

#[test]
fn failed_create_does_not_leave_namespace_registered() {
    let db = AletheiaDB::new().unwrap();
    // A forged reserved key makes the write fail AFTER namespace validation but
    // BEFORE (now) any registration; the namespace must not be registered.
    let bad = PropertyMapBuilder::new().insert("__shred_x", 1).build();
    let err = db
        .create_node_in_namespace("Person", bad, "agent:doomed")
        .unwrap_err();
    assert!(is_reserved_err(&err));

    let names: Vec<String> = db.list_namespaces().into_iter().map(|i| i.name).collect();
    assert!(
        !names.contains(&"agent:doomed".to_string()),
        "a failed write must not durably register its namespace: {names:?}"
    );
}

// ============================================================================
// Delete-namespace-then-write: writing to a deleted namespace re-auto-registers.
// ============================================================================

#[test]
fn delete_namespace_then_write_reregisters() {
    let db = AletheiaDB::new().unwrap();
    db.create_namespace("agent:x", None).unwrap();
    db.delete_namespace("agent:x").unwrap();
    assert!(db.describe_namespace("agent:x").is_err());

    // A write to the now-absent namespace auto-registers it again.
    db.create_node_in_namespace("Person", props("Alice"), "agent:x")
        .unwrap();
    let names: Vec<String> = db.list_namespaces().into_iter().map(|i| i.name).collect();
    assert!(
        names.contains(&"agent:x".to_string()),
        "writing to a deleted namespace must re-auto-register it: {names:?}"
    );
}

// ============================================================================
// Historical-read view elides the ride-along key (T3 extension).
// ============================================================================

#[test]
fn historical_read_view_elides_namespace_key() {
    use aletheiadb::core::temporal::time;
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node_in_namespace("Person", props("Alice"), "agent:planner")
        .unwrap();
    // Capture a valid-time coordinate at/after creation so the node is valid there.
    let vt = time::now();

    // Reconstruct at a valid-time within the node's validity: the point-in-time
    // view still carries the namespace as a first-class field, elided from props.
    let node = db.get_node_at_valid_time(id, vt).unwrap();
    assert_eq!(node.namespace().as_str(), "agent:planner");
    assert!(!node.user_properties().contains_key(NAMESPACE_KEY));
    assert_no_reserved_bytes(&node.user_properties(), "historical node view");
}
