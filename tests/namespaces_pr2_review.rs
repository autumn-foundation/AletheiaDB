//! Namespaces PR2 — review-pass test additions (Issue #3349, PR2 review).
//!
//! Block C from the adversarial-review fix list. These live in their own test
//! binary (separate process) rather than in `namespaces_pr2_threading.rs` so
//! their write/backup/restore load does not run in-process alongside that
//! binary's 2×2K interleaved isolation soak — the soak hammers the process-wide
//! string interners, and piling additional concurrent interning load into the
//! same process surfaces a pre-existing global-interner race unrelated to
//! namespaces. Isolating these tests in a separate binary keeps both stable.
//!
//! Coverage:
//! - C1  traverse_scoped_as_of honors the TIME coordinate (not just boundary)
//! - C2  out-of-scope traversal start => NodeNotFound / empty (A4)
//! - C3  scoped LIMIT/count operate on in-scope rows (A1)
//! - C4  scoped edge reads (current + point-in-time)
//! - C5  edge membership after delete + after restart
//! - C6  scoped k-NN order/scores match filtered-unscoped
//! - C7  union scope follows within AND between namespaces
//! - C8  find_nodes_by_property_at_scoped
//! - C9  empty in_namespaces([]) is INVALID_ARGUMENT, not unscoped-all (A2)
//! - C10 QueryBuilder unknown namespace => NOT_FOUND (A3)
//! - C11 backup/restore: node+edge namespace + scoped reads survive
//! - B1  interned-ns id consistent across all hydration paths

use aletheiadb::core::error::StorageError;
use aletheiadb::core::namespace::NamespaceError;
use aletheiadb::core::temporal::time;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::{AletheiaDB, Error, Namespace, NamespaceScope, PropertyMapBuilder, PropertyValue};
use std::collections::BTreeSet;

fn ns(name: &str) -> Namespace {
    Namespace::new(name).unwrap()
}

fn vec_props(name: &str, embedding: &[f32]) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", name)
        .insert_vector("embedding", embedding)
        .build()
}

fn props(name: &str) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

fn result_node_ids(db: &AletheiaDB, query: aletheiadb::query::Query) -> BTreeSet<u64> {
    db.execute_query(query)
        .unwrap()
        .collect_all()
        .unwrap()
        .into_iter()
        .filter_map(|row| row.entity.node_id())
        .map(|id| id.as_u64())
        .collect()
}

// ============================================================================
// Block C — review-pass test additions (Issue #3349, PR2 review).
// ============================================================================

/// C1: `traverse_scoped_as_of` honors the *time* coordinate, not just the
/// namespace boundary. An edge/target created AFTER the captured coordinate is
/// excluded when anchoring at `t`; a later coordinate `t2` includes it.
#[test]
fn c1_traverse_scoped_as_of_honors_time() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    db.create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
        .unwrap();
    // Capture t BEFORE the second edge/target exist.
    let t = time::now();

    // Now add a later in-scope target a3 and edge a1->a3.
    let a3 = db
        .create_node_in_namespace("Person", props("a3"), "agent:a")
        .unwrap();
    db.create_edge_in_namespace(a1, a3, "KNOWS", props(""), "agent:a")
        .unwrap();
    let t2 = time::now();

    let scope_a = NamespaceScope::single(ns("agent:a"));

    // As-of t: only a2 is reachable; a3 and its edge did not exist yet.
    let at_t: BTreeSet<_> = db
        .traverse_scoped_as_of(a1, Some("KNOWS"), 5, t, t, &scope_a)
        .unwrap()
        .into_iter()
        .collect();
    assert!(at_t.contains(&a2), "a2 reachable as-of t");
    assert!(
        !at_t.contains(&a3),
        "a3 created AFTER t must be excluded (temporal restriction, not just boundary)"
    );

    // As-of t2: both a2 and a3 are reachable.
    let at_t2: BTreeSet<_> = db
        .traverse_scoped_as_of(a1, Some("KNOWS"), 5, t2, t2, &scope_a)
        .unwrap()
        .into_iter()
        .collect();
    assert!(at_t2.contains(&a2));
    assert!(
        at_t2.contains(&a3),
        "a3 reachable as-of t2 (after its creation)"
    );
}

/// C2: an out-of-scope start node is `NodeNotFound` for `traverse_scoped`, and
/// yields an empty result set through the QueryBuilder executor (A4).
#[test]
fn c2_out_of_scope_start_is_not_found() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    // Register agent:b so scoping to it is valid (not an unknown-ns NOT_FOUND).
    db.create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();

    let scope_b = NamespaceScope::single(ns("agent:b"));
    // traverse_scoped: out-of-scope start (a1 ∈ agent:a) => NodeNotFound.
    assert!(matches!(
        db.traverse_scoped(a1, Some("KNOWS"), 3, &scope_b),
        Err(Error::Storage(StorageError::NodeNotFound(_)))
    ));

    // Builder: scoping the a1 traversal to agent:b drops the start => empty.
    let q = db
        .query()
        .start(a1)
        .traverse("KNOWS")
        .in_namespace(ns("agent:b"))
        .build();
    assert!(
        result_node_ids(&db, q).is_empty(),
        "out-of-scope start yields no traversal rows via the builder"
    );
}

/// C3: a scoped QueryBuilder scan applies the scope BELOW `LIMIT` and `count()`
/// (A1). `LIMIT n` returns up to `n` IN-SCOPE rows (not `n` pre-filter rows), and
/// a scoped count reports the SCOPED cardinality, not the global one.
#[test]
fn c3_scoped_limit_and_count_operate_on_in_scope_rows() {
    let db = AletheiaDB::new().unwrap();
    // Create the out-of-scope (agent:b) nodes FIRST so they have the lowest ids
    // and would be taken first by an unscoped scan — the pre-fix outermost
    // post-filter would then LIMIT to those and drop them, under-counting.
    for i in 0..5 {
        db.create_node_in_namespace("Doc", props(&format!("b{i}")), "agent:b")
            .unwrap();
    }
    let mut a_ids = BTreeSet::new();
    for i in 0..3 {
        a_ids.insert(
            db.create_node_in_namespace("Doc", props(&format!("a{i}")), "agent:a")
                .unwrap()
                .as_u64(),
        );
    }

    let scope_a = || ns("agent:a");

    // LIMIT 2 under scope A: exactly 2 rows, all agent:a.
    let q = db
        .query()
        .scan_label("Doc")
        .in_namespace(scope_a())
        .limit(2)
        .build();
    let ids = result_node_ids(&db, q);
    assert_eq!(
        ids.len(),
        2,
        "LIMIT 2 must yield 2 IN-SCOPE rows, not fewer"
    );
    assert!(
        ids.iter().all(|id| a_ids.contains(id)),
        "every LIMIT-ed row must be in agent:a"
    );

    // Scoped count() == 3 (the agent:a cardinality), not 8 (global).
    let scoped_count = db.query().scan_label("Doc").in_namespace(scope_a()).build();
    assert_eq!(
        db.execute_query(scoped_count).unwrap().count_all().unwrap(),
        3,
        "scoped count() must report the scoped cardinality, not the global one"
    );
}

/// C4: scoped edge reads — an edge in agent:a is visible under scope {a} and
/// NotFound under scope {b}, for both current-state and point-in-time reads.
#[test]
fn c4_scoped_edge_reads() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    // Register agent:b.
    db.create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();
    let e = db
        .create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
        .unwrap();
    let t = time::now();

    let scope_a = NamespaceScope::single(ns("agent:a"));
    let scope_b = NamespaceScope::single(ns("agent:b"));

    assert_eq!(db.get_edge_scoped(e, &scope_a).unwrap().id, e);
    assert!(matches!(
        db.get_edge_scoped(e, &scope_b),
        Err(Error::Storage(StorageError::EdgeNotFound(_)))
    ));

    assert_eq!(db.get_edge_at_time_scoped(e, t, t, &scope_a).unwrap().id, e);
    assert!(matches!(
        db.get_edge_at_time_scoped(e, t, t, &scope_b),
        Err(Error::Storage(StorageError::EdgeNotFound(_)))
    ));
}

/// C5: edge membership index is correct after a delete and is rebuilt after a
/// restart (WAL replay / index load).
#[test]
fn c5_edge_membership_after_delete_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");

    let (a1, a2, ea) = {
        let db = AletheiaDB::open(&path).unwrap();
        let a1 = db
            .create_node_in_namespace("Person", props("a1"), "agent:a")
            .unwrap();
        let a2 = db
            .create_node_in_namespace("Person", props("a2"), "agent:a")
            .unwrap();
        let b1 = db
            .create_node_in_namespace("Person", props("b1"), "agent:b")
            .unwrap();
        let b2 = db
            .create_node_in_namespace("Person", props("b2"), "agent:b")
            .unwrap();
        let ea = db
            .create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
            .unwrap();
        let _eb = db
            .create_edge_in_namespace(b1, b2, "KNOWS", props(""), "agent:b")
            .unwrap();

        let edge_count = |name: &str| {
            db.namespace_counts()
                .into_iter()
                .find(|c| c.name == name)
                .map(|c| c.edge_count)
                .unwrap_or(0)
        };
        assert_eq!(edge_count("agent:a"), 1);
        assert_eq!(edge_count("agent:b"), 1);

        // Delete the agent:a edge: its membership entry drains to zero.
        db.delete_edge_with_valid_time(ea, None).unwrap();
        assert_eq!(
            edge_count("agent:a"),
            0,
            "deleted edge leaves the a-set empty"
        );
        assert_eq!(edge_count("agent:b"), 1);
        drop(db);
        (a1, a2, ea)
    };
    let _ = (a1, a2, ea);

    // Reopen: edge membership rebuilt from the durable ride-along. The surviving
    // agent:b edge is scoped correctly; the deleted agent:a edge stays gone.
    let db = AletheiaDB::open(&path).unwrap();
    let edge_count = |name: &str| {
        db.namespace_counts()
            .into_iter()
            .find(|c| c.name == name)
            .map(|c| c.edge_count)
            .unwrap_or(0)
    };
    assert_eq!(
        edge_count("agent:a"),
        0,
        "deleted edge stays gone after restart"
    );
    assert_eq!(
        edge_count("agent:b"),
        1,
        "surviving edge rebuilt after restart"
    );
}

/// C6: scoped k-NN returns the NEAREST in-scope neighbors in similarity order
/// with scores identical to the unscoped ranking filtered to the scope.
#[test]
fn c6_scoped_knn_order_and_scores_match_filtered_unscoped() {
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    let q = db
        .create_node_in_namespace("Doc", vec_props("q", &[1.0, 0.0, 0.0, 0.0]), "agent:a")
        .unwrap();
    // Interleave a and b nodes at varying distances.
    let mut a_nodes = BTreeSet::new();
    for i in 0..12 {
        let spread = 0.01 + i as f32 * 0.01;
        let id = db
            .create_node_in_namespace(
                "Doc",
                vec_props(&format!("a{i}"), &[1.0 - spread, spread, 0.0, 0.0]),
                "agent:a",
            )
            .unwrap();
        a_nodes.insert(id);
        db.create_node_in_namespace(
            "Doc",
            vec_props(
                &format!("b{i}"),
                &[1.0 - spread * 0.5, 0.0, spread * 0.5, 0.0],
            ),
            "agent:b",
        )
        .unwrap();
    }

    let k = 5;
    let scope_a = NamespaceScope::single(ns("agent:a"));
    let scoped = db.find_similar_scoped(q, k, &scope_a).unwrap();

    // Expected: the full unscoped ranking, filtered to agent:a, top-k.
    let expected: Vec<(u64, f32)> = db
        .find_similar_scoped(q, 1_000, &NamespaceScope::all())
        .unwrap()
        .into_iter()
        .filter(|(id, _)| a_nodes.contains(id))
        .take(k)
        .map(|(id, s)| (id.as_u64(), s))
        .collect();

    let got: Vec<(u64, f32)> = scoped.iter().map(|(id, s)| (id.as_u64(), *s)).collect();
    assert_eq!(got.len(), k, "scoped k-NN returns k in-scope results");
    for ((gid, gs), (eid, es)) in got.iter().zip(expected.iter()) {
        assert_eq!(
            gid, eid,
            "scoped k-NN order matches filtered unscoped order"
        );
        assert!(
            (gs - es).abs() < 1e-6,
            "scoped score {gs} matches unscoped score {es} for node {gid}"
        );
    }
}

/// C7: a union scope follows edges WITHIN each listed namespace (and between
/// them), not only cross-namespace edges.
#[test]
fn c7_union_scope_follows_within_and_between() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    let s1 = db
        .create_node_in_namespace("Person", props("s1"), "shared")
        .unwrap();
    let s2 = db
        .create_node_in_namespace("Person", props("s2"), "shared")
        .unwrap();
    // within-A edge, within-shared edge, and a between edge a2 -> s1.
    db.create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
        .unwrap();
    db.create_edge_in_namespace(s1, s2, "KNOWS", props(""), "shared")
        .unwrap();
    db.create_edge_in_namespace(a2, s1, "KNOWS", props(""), "shared")
        .unwrap();

    let union = NamespaceScope::list(vec![ns("agent:a"), ns("shared")]).unwrap();
    let reachable: BTreeSet<_> = db
        .traverse_scoped(a1, Some("KNOWS"), 5, &union)
        .unwrap()
        .into_iter()
        .collect();
    // within-A (a1->a2), between (a2->s1), within-shared (s1->s2) all followed.
    assert!(reachable.contains(&a2), "within-A edge followed");
    assert!(reachable.contains(&s1), "between-namespace edge followed");
    assert!(reachable.contains(&s2), "within-shared edge followed");
}

/// C8: `find_nodes_by_property_at_scoped` reconstructs point-in-time nodes then
/// filters by (immutable) namespace.
#[test]
fn c8_find_nodes_by_property_at_scoped() {
    let db = AletheiaDB::new().unwrap();
    let a_alice = db
        .create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    let _b_alice = db
        .create_node_in_namespace("Person", props("Alice"), "agent:b")
        .unwrap();
    let t = time::now();

    let scope_a = NamespaceScope::single(ns("agent:a"));
    let alice = PropertyValue::from("Alice");
    let found = db
        .find_nodes_by_property_at_scoped("Person", "name", &alice, t, t, &scope_a)
        .unwrap();
    let ids: BTreeSet<_> = found.nodes.iter().map(|n| n.id).collect();
    assert!(ids.contains(&a_alice), "agent:a's Alice is found");
    assert_eq!(
        ids.len(),
        1,
        "agent:b's Alice is filtered out by scope {{a}}"
    );
}

/// C9 (A2): an empty `in_namespaces([])` is NOT unscoped-all — it is rejected as
/// INVALID_ARGUMENT at execution, never silently matching everything.
#[test]
fn c9_empty_in_namespaces_is_invalid_argument_not_unscoped() {
    let db = AletheiaDB::new().unwrap();
    db.create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    db.create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();

    let q = db
        .query()
        .scan_label("Person")
        .in_namespaces(Vec::<Namespace>::new())
        .build();
    match db.execute_query(q) {
        Err(Error::Namespace(NamespaceError::InvalidName { .. })) => {}
        Err(other) => panic!("empty in_namespaces([]) must be INVALID_ARGUMENT, got {other:?}"),
        Ok(_) => panic!("empty in_namespaces([]) must be INVALID_ARGUMENT, not unscoped-all Ok"),
    }
}

/// C10 (A3): scoping a QueryBuilder query to an unknown namespace is NOT_FOUND,
/// not a silently-empty result.
#[test]
fn c10_querybuilder_unknown_namespace_is_not_found() {
    let db = AletheiaDB::new().unwrap();
    db.create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();

    let q = db
        .query()
        .scan_label("Person")
        .in_namespace(ns("agent:does-not-exist"))
        .build();
    match db.execute_query(q) {
        Err(Error::Namespace(NamespaceError::NotFound { namespace })) => {
            assert_eq!(namespace, "agent:does-not-exist");
        }
        Err(other) => panic!("unknown namespace must be NOT_FOUND, got {other:?}"),
        Ok(_) => panic!("unknown namespace must be NOT_FOUND, not a silently-empty Ok"),
    }
}

/// C11 (T9): a backup→restore round-trip preserves node AND edge namespaces and
/// rebuilds the membership index, so scoped reads still isolate after restore.
#[test]
fn c11_backup_restore_scoped_reads_survive() {
    let dir = tempfile::tempdir().unwrap();
    let backup_path = dir.path().join("snap.albk");

    let (a1, a2, e) = {
        let db = AletheiaDB::new().unwrap();
        let a1 = db
            .create_node_in_namespace("Person", props("a1"), "agent:a")
            .unwrap();
        let a2 = db
            .create_node_in_namespace("Person", props("a2"), "agent:a")
            .unwrap();
        let _b1 = db
            .create_node_in_namespace("Person", props("b1"), "agent:b")
            .unwrap();
        let e = db
            .create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
            .unwrap();
        db.backup(&backup_path).unwrap();
        (a1, a2, e)
    };

    let restored = AletheiaDB::restore(&backup_path).unwrap();
    let scope_a = NamespaceScope::single(ns("agent:a"));
    let scope_b = NamespaceScope::single(ns("agent:b"));

    // Node namespace survives + membership index rebuilt: scoped list isolates.
    let listed: BTreeSet<_> = restored
        .list_nodes_scoped(Some("Person"), &scope_a)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(
        listed,
        BTreeSet::from([a1, a2]),
        "agent:a nodes visible after restore"
    );

    // Edge namespace survives + membership index rebuilt: scoped edge read +
    // scoped traversal both work post-restore.
    assert_eq!(restored.get_edge_scoped(e, &scope_a).unwrap().id, e);
    assert!(matches!(
        restored.get_edge_scoped(e, &scope_b),
        Err(Error::Storage(StorageError::EdgeNotFound(_)))
    ));
    let reachable = restored
        .traverse_scoped(a1, Some("KNOWS"), 3, &scope_a)
        .unwrap();
    assert!(
        reachable.contains(&a2),
        "scoped traversal works after restore"
    );
}

/// B1 guard: a namespace's interned membership id is derived identically on
/// EVERY hydration path, so a node/edge hydrated via a fresh write, via WAL
/// replay + index load (restart), and via backup→restore all yield the same
/// scoped-read result.
#[test]
fn b1_interned_ns_consistent_across_hydration_paths() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db");
    let backup_path = dir.path().join("snap.albk");

    let (a1, a2, e) = {
        let db = AletheiaDB::open(&db_path).unwrap();
        let a1 = db
            .create_node_in_namespace("Person", props("a1"), "agent:planner")
            .unwrap();
        let a2 = db
            .create_node_in_namespace("Person", props("a2"), "agent:planner")
            .unwrap();
        let e = db
            .create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:planner")
            .unwrap();
        db.backup(&backup_path).unwrap();
        drop(db);
        (a1, a2, e)
    };

    let scope = NamespaceScope::single(ns("agent:planner"));
    let check = |db: &AletheiaDB, label: &str| {
        // Same scoped-read outcome on every hydration path.
        let mut listed = db.list_nodes_scoped(Some("Person"), &scope).unwrap();
        listed.sort_unstable();
        assert_eq!(listed, vec![a1, a2], "{label}: nodes scoped-visible");
        assert_eq!(
            db.get_edge_scoped(e, &scope).unwrap().id,
            e,
            "{label}: edge scoped-visible"
        );
        assert!(
            db.traverse_scoped(a1, Some("KNOWS"), 2, &scope)
                .unwrap()
                .contains(&a2),
            "{label}: traversal honors membership"
        );
    };

    // Path 1: WAL replay + index load (reopen the durable db).
    let reopened = AletheiaDB::open(&db_path).unwrap();
    check(&reopened, "restart/WAL-replay");

    // Path 2: backup → restore into a fresh database.
    let restored = AletheiaDB::restore(&backup_path).unwrap();
    check(&restored, "backup/restore");
}
