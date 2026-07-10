//! Integration tests for fact-to-fact derivation lineage (Issue #3371).
//!
//! These exercise the public `AletheiaDB` API end-to-end: declaring derivation
//! at write time, querying the upstream/downstream closure in both directions,
//! bi-temporal `AS OF` composition, version pinning across supersession, the
//! retraction blast-radius report, and structured rejection of dangling
//! references. Each test maps to one or more acceptance criteria.

use aletheiadb::core::error::Error;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::lineage::{LineageError, LineageQueryOptions, LineageRef};
use aletheiadb::db::lineage::FactStatus;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

fn db() -> AletheiaDB {
    AletheiaDB::new().expect("db")
}

fn node(db: &AletheiaDB, name: &str) -> NodeId {
    db.create_node(
        "Fact",
        PropertyMapBuilder::new().insert("name", name).build(),
    )
    .expect("create node")
}

fn lref(db: &AletheiaDB, id: NodeId) -> LineageRef {
    db.node_lineage_ref(id).expect("current version ref")
}

// AC1 / AC3: lineage recorded on write and queryable upstream (direct).
#[test]
fn records_and_reads_direct_upstream() {
    let db = db();
    let a = node(&db, "a");
    let b = node(&db, "b");
    let a_ref = lref(&db, a);
    let b_ref = lref(&db, b);

    let derived = db
        .create_node_with_lineage(
            "Merged",
            PropertyMapBuilder::new().insert("name", "c").build(),
            &[a_ref, b_ref],
        )
        .expect("create with lineage");

    let derived_ref = lref(&db, derived);
    let view = db.upstream_lineage(derived_ref, LineageQueryOptions::new());
    assert_eq!(view.count(), 2);
    let refs: Vec<LineageRef> = view.entries.iter().map(|e| e.reference).collect();
    assert!(refs.contains(&a_ref));
    assert!(refs.contains(&b_ref));
    assert!(view.entries.iter().all(|e| e.depth == 1));
    assert!(!view.has_more);
    // Both inputs are still current.
    assert!(view.entries.iter().all(|e| e.status == FactStatus::Current));
}

// AC1: omitting derived_from reproduces today's behavior (empty slice == plain create).
#[test]
fn empty_derived_from_is_plain_create() {
    let db = db();
    let n = db
        .create_node_with_lineage(
            "Fact",
            PropertyMapBuilder::new().insert("name", "x").build(),
            &[],
        )
        .expect("create");
    assert_eq!(db.node_count(), 1);
    let view = db.upstream_lineage(lref(&db, n), LineageQueryOptions::new());
    assert_eq!(view.count(), 0);
    assert!(db.lineage_record(lref(&db, n).version).is_none());
}

// AC2: deriving from a nonexistent reference fails with a structured error and
// commits nothing.
#[test]
fn nonexistent_reference_rejected_no_write() {
    let db = db();
    let before = db.node_count();
    let bogus = LineageRef::new(NodeId::new(1).unwrap(), VersionId::new(9_999_999).unwrap());
    let err = db
        .create_node_with_lineage(
            "Merged",
            PropertyMapBuilder::new().insert("name", "c").build(),
            &[bogus],
        )
        .expect_err("must reject");
    match err {
        Error::Lineage(LineageError::SourceNotFound { reference }) => {
            assert_eq!(reference, bogus);
        }
        other => panic!("expected SourceNotFound, got {other:?}"),
    }
    // No node was created.
    assert_eq!(db.node_count(), before);
}

// AC2: an entity/version mismatch (right version id, wrong entity) is also rejected.
#[test]
fn entity_version_mismatch_rejected() {
    let db = db();
    let a = node(&db, "a");
    let a_ref = lref(&db, a);
    // Same version id, but claim it belongs to a different node.
    let mismatched = LineageRef::new(NodeId::new(a.as_u64() + 1000).unwrap(), a_ref.version);
    let err = db
        .create_node_with_lineage(
            "Merged",
            PropertyMapBuilder::new().insert("name", "c").build(),
            &[mismatched],
        )
        .expect_err("must reject");
    assert!(matches!(
        err,
        Error::Lineage(LineageError::SourceNotFound { .. })
    ));
}

// AC3: transitive upstream closure with depth limiting and has_more.
#[test]
fn transitive_upstream_with_depth_limit() {
    let db = db();
    let a = node(&db, "a");
    let b = db
        .create_node_with_lineage("D", PropertyMapBuilder::new().build(), &[lref(&db, a)])
        .unwrap();
    let c = db
        .create_node_with_lineage("D", PropertyMapBuilder::new().build(), &[lref(&db, b)])
        .unwrap();
    let d = db
        .create_node_with_lineage("D", PropertyMapBuilder::new().build(), &[lref(&db, c)])
        .unwrap();

    let d_ref = lref(&db, d);
    // Full closure: C(1), B(2), A(3).
    let full = db.upstream_lineage(d_ref, LineageQueryOptions::new());
    assert_eq!(full.count(), 3);
    assert_eq!(full.entries[0].reference, lref(&db, c));
    assert_eq!(full.entries[0].depth, 1);
    assert_eq!(full.entries[2].depth, 3);
    assert!(!full.has_more);

    // Depth 1: only C, has_more set (more upstream remains).
    let shallow = db.upstream_lineage(d_ref, LineageQueryOptions::new().with_max_depth(1));
    assert_eq!(shallow.count(), 1);
    assert!(shallow.has_more);
}

// AC5 + Success metric: downstream blast radius over a 3-level derivation tree
// of 50 facts returns exactly the expected set with correct per-hop depth.
#[test]
fn downstream_blast_radius_three_level_fifty_facts() {
    let db = db();
    // Level 0: one poisoned root.
    let root = node(&db, "root");
    let root_ref = lref(&db, root);

    // Level 1: 7 facts each derived from root.
    let mut level1 = Vec::new();
    for i in 0..7 {
        let n = db
            .create_node_with_lineage(
                "L1",
                PropertyMapBuilder::new().insert("i", i).build(),
                &[root_ref],
            )
            .unwrap();
        level1.push(n);
    }
    // Level 2: 14 facts, each derived from a level-1 fact (2 per level-1).
    let mut level2 = Vec::new();
    for (idx, parent) in level1.iter().enumerate() {
        for j in 0..2 {
            let n = db
                .create_node_with_lineage(
                    "L2",
                    PropertyMapBuilder::new()
                        .insert("k", (idx * 2 + j) as i64)
                        .build(),
                    &[lref(&db, *parent)],
                )
                .unwrap();
            level2.push(n);
        }
    }
    // Level 3: remaining facts to reach 50 total derived, each from a level-2 fact.
    // Counts so far: 7 (L1) + 14 (L2) = 21 derived. Add 29 at level 3.
    let mut level3 = Vec::new();
    for m in 0..29 {
        let parent = level2[m % level2.len()];
        let n = db
            .create_node_with_lineage(
                "L3",
                PropertyMapBuilder::new().insert("m", m as i64).build(),
                &[lref(&db, parent)],
            )
            .unwrap();
        level3.push(n);
    }

    let expected_total = level1.len() + level2.len() + level3.len();
    assert_eq!(expected_total, 50, "fixture should have 50 derived facts");

    let view = db.downstream_lineage(
        root_ref,
        LineageQueryOptions::new()
            .with_limit(1000)
            .with_max_depth(10),
    );
    // Exactly the fixture's set: no misses, no extras.
    assert_eq!(view.count(), 50);
    assert!(!view.has_more);

    // Per-hop depth checks.
    let depth_of = |id: NodeId| -> usize {
        let r = lref(&db, id);
        view.entries
            .iter()
            .find(|e| e.reference == r)
            .unwrap_or_else(|| panic!("missing {id} in closure"))
            .depth
    };
    for id in &level1 {
        assert_eq!(depth_of(*id), 1);
    }
    for id in &level2 {
        assert_eq!(depth_of(*id), 2);
    }
    for id in &level3 {
        assert_eq!(depth_of(*id), 3);
    }
}

// AC4: version pinning — after an input is updated, the recorded upstream
// closure still points at the exact version that was read, not the new one.
#[test]
fn upstream_pins_superseded_input_version() {
    let db = db();
    let a = node(&db, "a-v1");
    let a_v1 = lref(&db, a);

    let derived = db
        .create_node_with_lineage("D", PropertyMapBuilder::new().build(), &[a_v1])
        .unwrap();

    // Update A -> new version. The pinned lineage must still reference a_v1.
    db.update_node_with_valid_time(
        a,
        PropertyMapBuilder::new().insert("name", "a-v2").build(),
        None,
    )
    .unwrap();
    let a_v2 = lref(&db, a);
    assert_ne!(a_v1.version, a_v2.version, "update should bump version");

    let view = db.upstream_lineage(lref(&db, derived), LineageQueryOptions::new());
    assert_eq!(view.count(), 1);
    assert_eq!(view.entries[0].reference.version, a_v1.version);
    // The pinned input is now Superseded in current state.
    assert_eq!(view.entries[0].status, FactStatus::Superseded);
}

// AC4: lineage composes with bi-temporality — a lineage read AS OF a
// transaction time before the derivation was recorded sees no lineage.
#[test]
fn as_of_reflects_recording_transaction_time() {
    let db = db();
    let a = node(&db, "a");
    let a_ref = lref(&db, a);

    // Capture a coordinate strictly before the derivation is recorded.
    let before = aletheiadb::time::now();
    // Small spin so the derivation's commit time is strictly after `before`.
    std::thread::sleep(std::time::Duration::from_millis(2));

    let derived = db
        .create_node_with_lineage("D", PropertyMapBuilder::new().build(), &[a_ref])
        .unwrap();
    let derived_ref = lref(&db, derived);

    // AS OF before the recording: empty.
    let past = db.upstream_lineage(derived_ref, LineageQueryOptions::new().with_as_of(before));
    assert_eq!(past.count(), 0, "lineage not yet recorded at `before`");

    // Without AS OF (current): resolves.
    let now = db.upstream_lineage(derived_ref, LineageQueryOptions::new());
    assert_eq!(now.count(), 1);
}

// AC6: lineage is immutable and survives retraction of an input; the
// downstream closure over a retracted input still resolves and marks it Absent.
#[test]
fn retracted_input_still_resolves_downstream() {
    let db = db();
    let a = node(&db, "a");
    let a_ref = lref(&db, a);
    let derived = db
        .create_node_with_lineage("D", PropertyMapBuilder::new().build(), &[a_ref])
        .unwrap();
    let derived_ref = lref(&db, derived);

    // Retract A (no connected edges, so plain retract is allowed).
    db.retract_node(a, aletheiadb::time::now()).unwrap();

    // Downstream closure of the retracted input still finds the derived fact.
    let view = db.downstream_lineage(a_ref, LineageQueryOptions::new());
    assert_eq!(view.count(), 1);
    assert_eq!(view.entries[0].reference, derived_ref);

    // The retracted input's own status resolves to Absent when it appears in a
    // closure (query upstream from the derived fact).
    let up = db.upstream_lineage(derived_ref, LineageQueryOptions::new());
    assert_eq!(up.count(), 1);
    assert_eq!(up.entries[0].status, FactStatus::Absent);
}

// AC1 (edges): an edge fact can be derived from node facts, and node->edge
// lineage is queryable in both directions.
#[test]
fn edge_derived_from_nodes() {
    let db = db();
    let a = node(&db, "a");
    let b = node(&db, "b");
    let a_ref = lref(&db, a);
    let b_ref = lref(&db, b);

    let edge = db
        .create_edge_with_lineage(
            a,
            b,
            "REL",
            PropertyMapBuilder::new().build(),
            &[a_ref, b_ref],
        )
        .unwrap();
    let edge_ref = db.edge_lineage_ref(edge).unwrap();

    let up = db.upstream_lineage(edge_ref, LineageQueryOptions::new());
    assert_eq!(up.count(), 2);

    // Downstream of node A reaches the edge.
    let down = db.downstream_lineage(a_ref, LineageQueryOptions::new());
    assert_eq!(down.count(), 1);
    assert_eq!(down.entries[0].reference, edge_ref);
}

// AC3: update_node_with_lineage records lineage against the new version.
#[test]
fn update_records_lineage_on_new_version() {
    let db = db();
    let source = node(&db, "src");
    let src_ref = lref(&db, source);
    let target = node(&db, "target-v1");

    let new_version = db
        .update_node_with_lineage(
            target,
            PropertyMapBuilder::new()
                .insert("name", "target-v2")
                .build(),
            &[src_ref],
        )
        .unwrap();

    let target_ref = LineageRef::new(target, new_version);
    let view = db.upstream_lineage(target_ref, LineageQueryOptions::new());
    assert_eq!(view.count(), 1);
    assert_eq!(view.entries[0].reference, src_ref);
}
