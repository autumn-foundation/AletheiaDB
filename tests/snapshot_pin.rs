//! Integration tests for named snapshots / reproducible reads (Issue #3370).
//!
//! Named snapshots pin a human name to a bi-temporal coordinate; reads through
//! the resulting handle are deterministic and reproducible regardless of any
//! writes that land afterward.

#[cfg(feature = "simulation")]
use aletheiadb::WriteOps;
use aletheiadb::core::error::Error;
use aletheiadb::core::graph::{Edge, Node};
use aletheiadb::core::id::{EdgeId, NodeId};
use aletheiadb::core::temporal::time;
use aletheiadb::db::snapshot::Snapshot;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use serde_json::json;

/// The canonical durable config places the snapshot sidecar inside the index
/// persistence dir: `{data_dir}/indexes/snapshots.json` (Issue #3370, review
/// M6 — never a sibling written outside the data dir).
fn sidecar_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("indexes").join("snapshots.json")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn person(name: &str, age: i64) -> aletheiadb::core::PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", name)
        .insert("age", age)
        .build()
}

/// Canonical, stable JSON for a node (property order normalized).
fn node_json(n: &Node) -> serde_json::Value {
    let mut props: Vec<(String, String)> = n
        .properties
        .iter()
        .map(|(k, v)| (format!("{k:?}"), format!("{v:?}")))
        .collect();
    props.sort();
    json!({
        "id": format!("{}", n.id),
        "label": format!("{}", n.label),
        "props": props,
    })
}

/// Canonical, stable JSON for an edge.
fn edge_json(e: &Edge) -> serde_json::Value {
    let mut props: Vec<(String, String)> = e
        .properties
        .iter()
        .map(|(k, v)| (format!("{k:?}"), format!("{v:?}")))
        .collect();
    props.sort();
    json!({
        "id": format!("{}", e.id),
        "source": format!("{}", e.source),
        "target": format!("{}", e.target),
        "label": format!("{}", e.label),
        "props": props,
    })
}

/// Serialize a batch of reads through the snapshot handle to a byte-stable
/// string. This is the reproducibility fingerprint the determinism test
/// compares across arbitrary interleaved writes.
fn read_batch(s: &Snapshot, node_ids: &[NodeId], edge_ids: &[EdgeId], labels: &[&str]) -> String {
    let mut nodes = serde_json::Map::new();
    for id in node_ids {
        let v = match s.get_node(*id) {
            Ok(n) => node_json(&n),
            Err(_) => json!("absent"),
        };
        nodes.insert(format!("{id}"), v);
    }

    let mut edges = serde_json::Map::new();
    for id in edge_ids {
        let v = match s.get_edge(*id) {
            Ok(e) => edge_json(&e),
            Err(_) => json!("absent"),
        };
        edges.insert(format!("{id}"), v);
    }

    let mut finds = serde_json::Map::new();
    for label in labels {
        let result = s.find_nodes(label).expect("find_nodes should not error");
        let arr: Vec<serde_json::Value> = result.nodes.iter().map(node_json).collect();
        finds.insert(
            (*label).to_string(),
            json!({ "sampled": result.sampled, "nodes": arr }),
        );
    }

    serde_json::to_string(&json!({
        "nodes": nodes,
        "edges": edges,
        "finds": finds,
    }))
    .expect("serialization must succeed")
}

// ---------------------------------------------------------------------------
// 1. create default -> both coords ~= now; a read through the handle works
// ---------------------------------------------------------------------------

#[test]
fn create_default_snapshot_reflects_committed_state() {
    let db = AletheiaDB::new().unwrap();
    let before = time::now().wallclock();
    let alice = db.create_node("Person", person("Alice", 30)).unwrap();

    let snap = db
        .create_snapshot("now", Some("default coord".to_string()))
        .unwrap();
    let after = time::now().wallclock();

    // Both coordinates land within [before, after].
    let (vt, tt) = snap.coordinate();
    assert!(
        vt.wallclock() >= before && vt.wallclock() <= after,
        "vt in range"
    );
    assert!(
        tt.wallclock() >= before && tt.wallclock() <= after,
        "tt in range"
    );
    assert_eq!(snap.description(), Some("default coord"));

    // A read through the handle returns the node.
    let handle = db.snapshot("now").unwrap();
    let node = handle.get_node(alice).unwrap();
    assert_eq!(
        node.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

// ---------------------------------------------------------------------------
// 2. explicit backdated coords within extent -> handle reflects that world
// ---------------------------------------------------------------------------

#[test]
fn create_snapshot_at_explicit_backdated_coord() {
    let db = AletheiaDB::new().unwrap();
    let alice = db.create_node("Person", person("Alice", 30)).unwrap();

    // Capture the world-with-only-Alice via a default snapshot.
    let world_a = db.create_snapshot("world_a", None).unwrap();
    let (vt_a, tt_a) = world_a.coordinate();

    // Now add Bob.
    let bob = db.create_node("Person", person("Bob", 40)).unwrap();

    // A backdated snapshot pinned to Alice-only coords must NOT see Bob.
    let back = db
        .create_snapshot_at(
            "backdated",
            vt_a,
            tt_a,
            Some("as of Alice-only".to_string()),
        )
        .unwrap();
    let handle = db.snapshot(back.name()).unwrap();

    assert!(
        handle.get_node(alice).is_ok(),
        "Alice visible at backdated pin"
    );
    assert!(
        handle.get_node(bob).is_err(),
        "Bob invisible at backdated pin"
    );

    let persons = handle.find_nodes("Person").unwrap();
    assert_eq!(persons.nodes.len(), 1, "only Alice at backdated pin");
}

// ---------------------------------------------------------------------------
// 3. duplicate name -> CONFLICT structured error
// ---------------------------------------------------------------------------

#[test]
fn duplicate_name_is_conflict() {
    let db = AletheiaDB::new().unwrap();
    db.create_node("Person", person("Alice", 30)).unwrap();
    db.create_snapshot("dup", None).unwrap();

    let err = db.create_snapshot("dup", None).unwrap_err();
    // Reuses StorageError::DuplicateId, which maps to the #3234 CONFLICT code.
    match err {
        Error::Storage(aletheiadb::core::error::StorageError::DuplicateId { ref id, ref kind }) => {
            assert_eq!(id, "dup");
            assert_eq!(kind, "snapshot");
        }
        other => panic!("expected DuplicateId conflict, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 4. list returns name/coord/created_at/description in stable order
// ---------------------------------------------------------------------------

#[test]
fn list_returns_metadata_in_stable_order() {
    let db = AletheiaDB::new().unwrap();
    db.create_node("Person", person("Alice", 30)).unwrap();

    // Explicit created-at ordering via distinct coordinates + names.
    let s_a = db
        .create_snapshot_at("a", time::now(), time::now(), Some("first".into()))
        .unwrap();
    let s_b = db
        .create_snapshot_at("b", time::now(), time::now(), None)
        .unwrap();
    let s_c = db
        .create_snapshot_at("c", time::now(), time::now(), Some("third".into()))
        .unwrap();

    let list = db.list_snapshots();
    assert_eq!(list.len(), 3);

    // Stable order: created_at, then name. All fields are present.
    let names: Vec<&str> = list.iter().map(|s| s.name()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
    assert_eq!(list[0].description(), Some("first"));
    assert_eq!(list[1].description(), None);
    assert_eq!(list[2].description(), Some("third"));
    assert_eq!(list[0].coordinate(), s_a.coordinate());
    assert_eq!(list[1].coordinate(), s_b.coordinate());
    assert_eq!(list[2].coordinate(), s_c.coordinate());
    // created_at is populated and monotonic non-decreasing.
    assert!(list[0].created_at().wallclock() <= list[2].created_at().wallclock());

    // Listing is repeatable (same order every call).
    let names2: Vec<String> = db
        .list_snapshots()
        .iter()
        .map(|s| s.name().to_string())
        .collect();
    assert_eq!(names2, vec!["a", "b", "c"]);
}

// ---------------------------------------------------------------------------
// 5. delete removes it; subsequent resolve -> NOT_FOUND with name in details
// ---------------------------------------------------------------------------

#[test]
fn delete_removes_and_resolve_is_not_found() {
    let db = AletheiaDB::new().unwrap();
    db.create_node("Person", person("Alice", 30)).unwrap();
    db.create_snapshot("temp", None).unwrap();

    db.delete_snapshot("temp").unwrap();

    // snapshot(name) -> NOT_FOUND, name in message.
    let err = db.snapshot("temp").unwrap_err();
    assert_not_found_with_name(&err, "temp");

    // get_snapshot -> NOT_FOUND, name in message.
    let err = db.get_snapshot("temp").unwrap_err();
    assert_not_found_with_name(&err, "temp");

    // second delete -> NOT_FOUND.
    let err = db.delete_snapshot("temp").unwrap_err();
    assert_not_found_with_name(&err, "temp");
}

// ---------------------------------------------------------------------------
// 6. resolve nonexistent -> NOT_FOUND with name in details
// ---------------------------------------------------------------------------

#[test]
fn resolve_nonexistent_is_not_found() {
    let db = AletheiaDB::new().unwrap();
    let err = db.snapshot("nope").unwrap_err();
    assert_not_found_with_name(&err, "nope");
    let err = db.get_snapshot("nope").unwrap_err();
    assert_not_found_with_name(&err, "nope");
}

fn assert_not_found_with_name(err: &Error, name: &str) {
    // Reuses StorageError::PropertyNotFound, which maps to the #3234 NOT_FOUND
    // code; the name is carried in the message ("the details").
    match err {
        Error::Storage(aletheiadb::core::error::StorageError::PropertyNotFound(msg)) => {
            assert!(msg.contains(name), "name '{name}' missing from '{msg}'");
        }
        other => panic!("expected NOT_FOUND (PropertyNotFound) for '{name}', got {other:?}"),
    }
    // And the Display carries the name too.
    assert!(
        err.to_string().contains(name),
        "Display should contain the name"
    );
}

// ---------------------------------------------------------------------------
// 7. DETERMINISM CORE (the falsifiable AC)
// ---------------------------------------------------------------------------

fn run_determinism(iterations: usize) {
    let db = AletheiaDB::new().unwrap();

    // Seed a small graph.
    let n1 = db.create_node("Person", person("Alice", 30)).unwrap();
    let n2 = db.create_node("Person", person("Bob", 40)).unwrap();
    let e1 = db
        .create_edge(
            n1,
            n2,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020).build(),
        )
        .unwrap();

    let s = db.create_snapshot("run1", None).unwrap();
    let handle = db.snapshot("run1").unwrap();

    let labels = ["Person", "Other"];
    let r0 = read_batch(&handle, &[n1, n2], &[e1], &labels);

    // Interleave arbitrary writes; the snapshot reads must never budge.
    let mut n2_deleted = false;
    let mut n1_updated = false;
    for i in 0..iterations {
        match i % 7 {
            0 => {
                // create a node that appears after the pin (must stay invisible)
                db.create_node("Person", person(&format!("New{i}"), i as i64))
                    .unwrap();
            }
            1 => {
                // update n1 after the pin (pin must keep the old version)
                db.update_node_with_valid_time(n1, person("AliceChanged", 99), None)
                    .unwrap();
                n1_updated = true;
            }
            2 => {
                // create then update a throwaway edge (exercise the edge
                // write/update path without touching the pinned e1, whose
                // endpoint n2 may already be deleted below)
                let a = db
                    .create_node("Other", person(&format!("Ea{i}"), 1))
                    .unwrap();
                let b = db
                    .create_node("Other", person(&format!("Eb{i}"), 2))
                    .unwrap();
                let te = db
                    .create_edge(
                        a,
                        b,
                        "LINK",
                        PropertyMapBuilder::new().insert("w", 1).build(),
                    )
                    .unwrap();
                db.update_edge_with_valid_time(
                    te,
                    PropertyMapBuilder::new().insert("w", 2 + i as i64).build(),
                    None,
                )
                .unwrap();
            }
            3 => {
                // delete n2 (once) after the pin (pin must keep it visible)
                if !n2_deleted {
                    db.delete_node_with_valid_time(n2, None).unwrap();
                    n2_deleted = true;
                }
            }
            4 => {
                // create + retract an isolated throwaway node
                let tmp = db
                    .create_node("Ghost", person(&format!("G{i}"), 1))
                    .unwrap();
                db.retract_node(tmp, time::now()).unwrap();
            }
            5 => {
                // create a throwaway edge between two fresh nodes
                let a = db
                    .create_node("Other", person(&format!("A{i}"), 1))
                    .unwrap();
                let b = db
                    .create_node("Other", person(&format!("B{i}"), 2))
                    .unwrap();
                db.create_edge(a, b, "LINK", PropertyMapBuilder::new().build())
                    .unwrap();
            }
            _ => {
                // create + delete a throwaway node
                let tmp = db
                    .create_node("Other", person(&format!("T{i}"), 3))
                    .unwrap();
                db.delete_node_with_valid_time(tmp, None).unwrap();
            }
        }

        let ri = read_batch(&handle, &[n1, n2], &[e1], &labels);
        assert_eq!(ri, r0, "snapshot reads diverged at iteration {i}");
    }

    // We really exercised the three required scenarios.
    assert!(n1_updated, "n1 was updated after the pin");
    assert!(n2_deleted, "n2 was deleted after the pin");

    // Explicitly: a node created after the pin is invisible through it.
    let post = db.create_node("Person", person("AfterPin", 1)).unwrap();
    assert!(handle.get_node(post).is_err(), "post-pin node invisible");
    let persons = handle.find_nodes("Person").unwrap();
    assert_eq!(
        persons.nodes.len(),
        2,
        "only the two pre-pin Persons at the pin"
    );

    // A node updated after the pin reads as-of the pin (original value).
    let alice_at_pin = handle.get_node(n1).unwrap();
    assert_eq!(
        alice_at_pin.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    // A node deleted after the pin is still visible through the pin.
    assert!(
        handle.get_node(n2).is_ok(),
        "deleted node still visible at the pin"
    );

    // The coordinate is what we claim.
    let (vt, tt) = s.coordinate();
    assert_eq!(handle.valid_time(), vt);
    assert_eq!(handle.transaction_time(), tt);
}

#[test]
fn determinism_core_60_iterations() {
    run_determinism(60);
}

#[test]
fn determinism_heavy_1000_iterations() {
    run_determinism(1000);
}

// ---------------------------------------------------------------------------
// 8. Restart durability
// ---------------------------------------------------------------------------

#[test]
fn snapshot_survives_restart() {
    let dir = tempfile::tempdir().unwrap();

    let (coord, node_id) = {
        let db = AletheiaDB::open(dir.path()).unwrap();
        let node_id = db.create_node("Person", person("Alice", 30)).unwrap();
        let snap = db
            .create_snapshot("persisted", Some("across restart".to_string()))
            .unwrap();
        (snap.coordinate(), node_id)
    };

    // A sidecar file was actually written (inside the data dir, M6).
    assert!(sidecar_path(dir.path()).exists(), "sidecar exists");
    // And nothing was written OUTSIDE the data dir as a sibling.
    assert!(
        !dir.path().join("snapshots.json").exists(),
        "no sibling sidecar outside the data dir"
    );

    // Reopen the same directory.
    let db = AletheiaDB::open(dir.path()).unwrap();
    let reloaded = db.get_snapshot("persisted").unwrap();
    assert_eq!(reloaded.coordinate(), coord, "coordinate survives restart");
    assert_eq!(reloaded.description(), Some("across restart"));

    // Reads through the reopened snapshot still work (history is available).
    let handle = db.snapshot("persisted").unwrap();
    let node = handle.get_node(node_id).unwrap();
    assert_eq!(
        node.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

// ---------------------------------------------------------------------------
// 9. Registry persistence is atomic; ephemeral new() keeps it in-memory
// ---------------------------------------------------------------------------

#[test]
fn ephemeral_db_writes_no_sidecar_but_resolves_in_process() {
    let db = AletheiaDB::new().unwrap();
    db.create_node("Person", person("Alice", 30)).unwrap();
    let snap = db.create_snapshot("mem", None).unwrap();

    // Resolves within the process.
    assert_eq!(
        db.get_snapshot("mem").unwrap().coordinate(),
        snap.coordinate()
    );
    // But nothing was persisted anywhere reachable (ephemeral tempdir has no
    // snapshots.json at its root; there is no configured data dir).
    assert_eq!(db.list_snapshots().len(), 1);
}

#[test]
fn persisted_registry_file_is_valid_json_shape() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.create_node("Person", person("Alice", 30)).unwrap();
        db.create_snapshot("s1", Some("one".to_string())).unwrap();
        db.create_snapshot("s2", None).unwrap();
    }

    let path = sidecar_path(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&contents).unwrap();

    // v2 persists the full HLC (wallclock + logical), not bare micros (M1).
    assert_eq!(value["version"], 2);
    let arr = value["snapshots"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Timestamps are {wallclock, logical} objects (lossless HLC).
    assert!(
        arr[0]["valid_time"].is_object(),
        "valid_time is a {{wallclock, logical}} object"
    );
    assert!(arr[0]["valid_time"]["wallclock"].is_i64());
    assert!(arr[0]["valid_time"]["logical"].is_u64());
    assert!(arr[0]["transaction_time"]["wallclock"].is_i64());
    assert!(arr[0]["created_at"]["wallclock"].is_i64());
    let names: Vec<&str> = arr.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"s1") && names.contains(&"s2"));

    // No leftover temp file after the atomic rename.
    assert!(!path.with_extension("tmp").exists(), "no stale temp file");
}

#[test]
fn deletion_persists_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.create_node("Person", person("Alice", 30)).unwrap();
        db.create_snapshot("keep", None).unwrap();
        db.create_snapshot("drop", None).unwrap();
        db.delete_snapshot("drop").unwrap();
    }
    let db = AletheiaDB::open(dir.path()).unwrap();
    assert!(db.get_snapshot("keep").is_ok());
    assert!(
        db.get_snapshot("drop").is_err(),
        "deletion survived restart"
    );
}

// ---------------------------------------------------------------------------
// 10. No write-path overhead: creating a snapshot does not change writes
// ---------------------------------------------------------------------------

#[test]
fn snapshot_creation_does_not_disturb_writes() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db.create_node("Person", person("Alice", 30)).unwrap();

    // Interleave snapshot creation with writes; ids/counts are unaffected.
    db.create_snapshot("s0", None).unwrap();
    let n2 = db.create_node("Person", person("Bob", 40)).unwrap();
    db.create_snapshot("s1", None).unwrap();
    let e1 = db
        .create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // The graph is exactly what the writes produced.
    assert!(db.get_node(n1).is_ok());
    assert!(db.get_node(n2).is_ok());
    assert!(db.get_edge(e1).is_ok());
    assert_ne!(format!("{n1}"), format!("{n2}"));
}

// ---------------------------------------------------------------------------
// Pinned adjacency at the snapshot coordinate
// ---------------------------------------------------------------------------

#[test]
fn pinned_adjacency_reflects_the_coordinate() {
    let db = AletheiaDB::new().unwrap();
    let a = db.create_node("Person", person("A", 1)).unwrap();
    let b = db.create_node("Person", person("B", 2)).unwrap();
    let e = db
        .create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    let handle_name = "adj";
    db.create_snapshot(handle_name, None).unwrap();

    // Add another edge AFTER the pin.
    let c = db.create_node("Person", person("C", 3)).unwrap();
    let _e2 = db
        .create_edge(a, c, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    let handle = db.snapshot(handle_name).unwrap();
    let out = handle.get_outgoing_edges(a);
    assert_eq!(out, vec![e], "only the pre-pin outgoing edge at the pin");
    let inc = handle.get_incoming_edges(b);
    assert_eq!(inc, vec![e], "pre-pin incoming edge at the pin");
}

// ---------------------------------------------------------------------------
// M3. Retraction AFTER the pin of a PRE-pin tracked entity must not disturb it
// ---------------------------------------------------------------------------

#[test]
fn retraction_after_pin_preserves_pre_pin_tracked_entity() {
    let db = AletheiaDB::new().unwrap();
    // Seed n1 (+ edge e1 to n2) BEFORE the pin so they are genuinely in the
    // tracked read set (the review noted the determinism loop only retracted a
    // throwaway node created AFTER the pin, which proves nothing).
    let n1 = db.create_node("Person", person("Alice", 30)).unwrap();
    let n2 = db.create_node("Person", person("Bob", 40)).unwrap();
    let e1 = db
        .create_edge(
            n1,
            n2,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020).build(),
        )
        .unwrap();

    // Pin now — after seeding, before any retraction.
    db.create_snapshot("pre", None).unwrap();
    let handle = db.snapshot("pre").unwrap();

    let n1_before = handle.get_node(n1).unwrap();
    let persons_before = handle.find_nodes("Person").unwrap().nodes.len();
    assert_eq!(persons_before, 2);

    // Retract n1 (detach co-retracts the connected edge e1) AFTER the pin.
    let res = db.retract_node_detach(n1, time::now()).unwrap();
    assert_eq!(res.edges_retracted, 1, "e1 co-retracted with n1");

    // The pin STILL returns n1's pre-retraction version, byte-identical.
    let n1_after = handle.get_node(n1).unwrap();
    assert_eq!(
        n1_after.properties, n1_before.properties,
        "pinned n1 unchanged by post-pin retraction"
    );
    assert_eq!(
        n1_after.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    // find_nodes count unchanged through the pin, and e1 still visible.
    assert_eq!(
        handle.find_nodes("Person").unwrap().nodes.len(),
        persons_before,
        "find count unchanged through the pin after retraction"
    );
    assert!(
        handle.get_edge(e1).is_ok(),
        "co-retracted edge still visible at the pin"
    );
}

// ---------------------------------------------------------------------------
// m8. The pre-pinned Snapshot::query() builder traverses AS OF the pin (AC4)
// ---------------------------------------------------------------------------

#[test]
fn pinned_query_builder_traverses_at_the_coordinate() {
    use aletheiadb::query::QueryBuilder;

    let db = AletheiaDB::new().unwrap();
    let a = db.create_node("Person", person("A", 1)).unwrap();
    let b = db.create_node("Person", person("B", 2)).unwrap();
    db.create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_snapshot("q", None).unwrap();

    // A post-pin edge to a fresh node C (must be excluded by the pinned query).
    let c = db.create_node("Person", person("C", 3)).unwrap();
    db.create_edge(a, c, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    let handle = db.snapshot("q").unwrap();
    // The builder returned by handle.query() is already `as_of(pin)`.
    let builder: QueryBuilder<_> = handle.query();
    let nodes = builder
        .start(a)
        .traverse("KNOWS")
        .execute(&db)
        .unwrap()
        .collect_nodes()
        .unwrap();
    let ids: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
    assert!(
        ids.contains(&b),
        "pinned traversal reaches the pre-pin neighbor B, got {ids:?}"
    );
    assert!(
        !ids.contains(&c),
        "pinned traversal excludes the post-pin neighbor C, got {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// m9. Future-valid exclusion by a default (valid=now) snapshot; included when
//     pinned to the future valid coord (ties to M2: default valid = now())
// ---------------------------------------------------------------------------

#[test]
fn future_valid_fact_excluded_by_default_snapshot_included_when_pinned_to_future() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now();
    // A fact that only becomes valid 10s in the future.
    let future: aletheiadb::core::temporal::Timestamp = (now.wallclock() + 10_000_000).into();
    let fnode = db
        .create_node_with_valid_time("Person", person("Future", 1), Some(future))
        .unwrap();

    // Default snapshot pins valid-time = wallclock-now (M2), so the future-dated
    // fact is invisible through it.
    db.create_snapshot("now", None).unwrap();
    let now_handle = db.snapshot("now").unwrap();
    assert!(
        now_handle.get_node(fnode).is_err(),
        "future-valid fact excluded by the default (valid=now) snapshot"
    );
    assert!(
        now_handle
            .find_nodes("Person")
            .unwrap()
            .nodes
            .iter()
            .all(|n| n.id != fnode),
        "future-valid fact not in find_nodes at the default pin"
    );

    // A snapshot pinned to the fact's future valid coordinate DOES see it.
    let tt = db.get_snapshot("now").unwrap().transaction_time();
    db.create_snapshot_at("fut", future, tt, None).unwrap();
    let fut_handle = db.snapshot("fut").unwrap();
    assert!(
        fut_handle.get_node(fnode).is_ok(),
        "future-valid fact visible when the snapshot valid coord is the future"
    );
    assert_eq!(
        fut_handle
            .get_node(fnode)
            .unwrap()
            .properties
            .get("name")
            .and_then(|v| v.as_str()),
        Some("Future")
    );
}

// ---------------------------------------------------------------------------
// m7. Multithreaded determinism: a writer thread mutates the graph while the
//     main thread repeatedly reads through the handle; every read batch must be
//     byte-identical to the pre-write fingerprint.
// ---------------------------------------------------------------------------

#[test]
fn determinism_under_concurrent_writer() {
    let db = AletheiaDB::new().unwrap();

    let n1 = db.create_node("Person", person("Alice", 30)).unwrap();
    let n2 = db.create_node("Person", person("Bob", 40)).unwrap();
    let e1 = db
        .create_edge(
            n1,
            n2,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020).build(),
        )
        .unwrap();

    // Create the snapshot FIRST, before spawning the writer, so creation does
    // not race a commit (this keeps the test deterministic and sidesteps the
    // known committed-but-not-yet-applied visibility window).
    db.create_snapshot("mt", None).unwrap();

    let labels = ["Person", "Other"];
    let fingerprint = {
        let handle = db.snapshot("mt").unwrap();
        read_batch(&handle, &[n1, n2], &[e1], &labels)
    };

    std::thread::scope(|scope| {
        // Writer thread: many create/update/delete ops against the live db.
        scope.spawn(|| {
            for i in 0..300 {
                match i % 4 {
                    0 => {
                        db.create_node("Person", person(&format!("W{i}"), i as i64))
                            .unwrap();
                    }
                    1 => {
                        db.update_node_with_valid_time(n1, person("Mutated", 99), None)
                            .unwrap();
                    }
                    2 => {
                        let tmp = db
                            .create_node("Other", person(&format!("T{i}"), 1))
                            .unwrap();
                        db.delete_node_with_valid_time(tmp, None).unwrap();
                    }
                    _ => {
                        let a = db
                            .create_node("Other", person(&format!("A{i}"), 1))
                            .unwrap();
                        let b = db
                            .create_node("Other", person(&format!("B{i}"), 2))
                            .unwrap();
                        db.create_edge(a, b, "LINK", PropertyMapBuilder::new().build())
                            .unwrap();
                    }
                }
            }
        });

        // Reader: hammer the handle; every batch must match the pre-write value.
        let handle = db.snapshot("mt").unwrap();
        for _ in 0..500 {
            let batch = read_batch(&handle, &[n1, n2], &[e1], &labels);
            assert_eq!(
                batch, fingerprint,
                "snapshot read diverged under a concurrent writer"
            );
        }
    });

    // A final read after all writes are joined must still match.
    let handle = db.snapshot("mt").unwrap();
    assert_eq!(
        read_batch(&handle, &[n1, n2], &[e1], &labels),
        fingerprint,
        "snapshot read diverged after the writer finished"
    );
}

// ---------------------------------------------------------------------------
// M1. Same-microsecond supersession must resolve the correct version AFTER a
//     restart: the pin's HLC logical counter must round-trip through the
//     sidecar. With the logical counter dropped, the pin at (W, L2) collapses
//     to (W, 0) and resolves the WRONG (superseded / absent) version.
// ---------------------------------------------------------------------------

#[cfg(feature = "simulation")]
#[test]
fn snapshot_resolves_correct_version_across_restart_same_microsecond() {
    use aletheiadb::simulation::SimulatedClock;

    let dir = tempfile::tempdir().unwrap();

    // Freeze the clock so both commits land in ONE wallclock microsecond and
    // differ only in their HLC logical counter.
    let clock = SimulatedClock::new(1_700_000_000_000_000);

    let (node_id, vt2, tt2) = {
        let _guard = clock.inject();
        let db = AletheiaDB::open(dir.path()).unwrap();

        // v1
        let node_id = {
            let mut tx = db.write_transaction().unwrap();
            let id = tx.create_node("Person", person("V1", 1)).unwrap();
            tx.commit_with_timestamp().unwrap();
            id
        };
        // v2 supersedes v1 in the SAME frozen microsecond.
        let ts2 = {
            let mut tx = db.write_transaction().unwrap();
            tx.update_node(node_id, person("V2", 2)).unwrap();
            tx.commit_with_timestamp().unwrap()
        };
        assert!(
            ts2.logical() > 0,
            "v2 tx-time must carry a nonzero logical counter, got {ts2:?}"
        );

        // Pin exactly at v2's bi-temporal coordinate.
        let snap = db.create_snapshot_at("pin", ts2, ts2, None).unwrap();

        // BEFORE restart: the handle resolves v2.
        let handle = db.snapshot("pin").unwrap();
        assert_eq!(
            handle
                .get_node(node_id)
                .unwrap()
                .properties
                .get("name")
                .and_then(|v| v.as_str()),
            Some("V2"),
            "pre-restart handle resolves v2"
        );
        (node_id, snap.valid_time(), snap.transaction_time())
    };

    assert!(tt2.logical() > 0, "pinned tx-time has a nonzero logical");

    // Reopen from the same data dir (clock guard dropped — reads use the pinned
    // coordinate, not wallclock now).
    let db = AletheiaDB::open(dir.path()).unwrap();

    // The FULL HLC coordinate survives the sidecar round-trip.
    let reloaded = db.get_snapshot("pin").unwrap();
    assert_eq!(
        reloaded.transaction_time(),
        tt2,
        "tx-time logical counter survives restart"
    );
    assert_eq!(
        reloaded.valid_time(),
        vt2,
        "valid-time logical counter survives restart"
    );

    // AND a read through the reopened handle STILL returns v2 — not the
    // superseded v1, not absent (which is what a collapsed (W,0) pin yields).
    let handle = db.snapshot("pin").unwrap();
    assert_eq!(
        handle
            .get_node(node_id)
            .unwrap()
            .properties
            .get("name")
            .and_then(|v| v.as_str()),
        Some("V2"),
        "restart resolves v2 via the full-HLC pin, not v1"
    );
}
