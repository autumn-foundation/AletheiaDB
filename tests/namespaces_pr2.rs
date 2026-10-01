//! Namespaces PR2 — storage/query threading integration tests (Issue #3349).
//!
//! Covers the design doc's T4 (traversal isolation), T5 (read isolation), T8
//! (per-namespace stats/schema), membership-index integrity across
//! create/delete + restart/replay, and omitted-scope back-compat.

use aletheiadb::core::error::StorageError;
use aletheiadb::{
    AletheiaDB, Error, Namespace, NamespaceError, NamespaceScope, PropertyMapBuilder, PropertyValue,
};
use std::collections::BTreeMap;

fn props(name: &str) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

fn ns(name: &str) -> Namespace {
    Namespace::new(name).unwrap()
}

/// Group current nodes by namespace via a full scan (the ground truth the
/// membership index must match).
fn node_counts_by_scan(db: &AletheiaDB) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for id in db.get_all_node_ids() {
        if let Ok(node) = db.get_node(id) {
            *out.entry(node.namespace().into_string()).or_insert(0) += 1;
        }
    }
    out
}

// ============================================================================
// T4 — traversal isolation (edge.ns ∈ S AND target.ns ∈ S).
// ============================================================================

#[test]
fn t4_edge_to_other_namespace_not_followed_under_single_scope() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node_in_namespace("Person", props("A"), "agent:a")
        .unwrap();
    let b = db
        .create_node_in_namespace("Person", props("B"), "agent:b")
        .unwrap();
    // Cross-namespace edge (legal); edge itself lives in agent:a.
    db.create_edge_in_namespace(a, b, "KNOWS", props(""), "agent:a")
        .unwrap();

    // Scope {a}: target b's namespace ∉ scope ⇒ not followed.
    let reachable = db
        .traverse_scoped(a, None, 5, &NamespaceScope::single(ns("agent:a")))
        .unwrap();
    assert!(
        !reachable.contains(&b),
        "b must not be reachable under scope {{a}}"
    );

    // Scope {a, b}: now followed.
    let scope_ab = NamespaceScope::list(vec![ns("agent:a"), ns("agent:b")]).unwrap();
    let reachable = db.traverse_scoped(a, None, 5, &scope_ab).unwrap();
    assert!(
        reachable.contains(&b),
        "b must be reachable under scope {{a,b}}"
    );
}

#[test]
fn t4_edge_whose_namespace_out_of_scope_never_crossed() {
    let db = AletheiaDB::new().unwrap();
    // Both endpoints in agent:a, but the EDGE lives in `shared`.
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    db.create_edge_in_namespace(a1, a2, "KNOWS", props(""), "shared")
        .unwrap();

    // Scope {a}: edge.ns == shared ∉ scope ⇒ never crossed even though the
    // target node IS in scope.
    let reachable = db
        .traverse_scoped(a1, None, 5, &NamespaceScope::single(ns("agent:a")))
        .unwrap();
    assert!(
        !reachable.contains(&a2),
        "a2 unreachable when the connecting edge's namespace is out of scope"
    );

    // Scope {a, shared}: edge.ns ∈ scope AND target.ns ∈ scope ⇒ crossed.
    let scope = NamespaceScope::list(vec![ns("agent:a"), ns("shared")]).unwrap();
    let reachable = db.traverse_scoped(a1, None, 5, &scope).unwrap();
    assert!(
        reachable.contains(&a2),
        "a2 reachable once the edge namespace is in scope"
    );
}

#[test]
fn t4_union_scope_follows_within_and_between() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let sh = db
        .create_node_in_namespace("Doc", props("sh"), "shared")
        .unwrap();
    // Edge from a to shared, living in `shared`.
    db.create_edge_in_namespace(a1, sh, "USES", props(""), "shared")
        .unwrap();

    let scope = NamespaceScope::list(vec![ns("agent:a"), ns("shared")]).unwrap();
    let reachable = db.traverse_scoped(a1, None, 5, &scope).unwrap();
    assert!(
        reachable.contains(&sh),
        "union scope follows between a and shared"
    );
}

// ============================================================================
// T5 — read isolation (scoped get/list/find never return out-of-scope).
// ============================================================================

#[test]
fn t5_scoped_reads_never_return_other_namespace() {
    let db = AletheiaDB::new().unwrap();
    let a_alice = db
        .create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    let b_alice = db
        .create_node_in_namespace("Person", props("Alice"), "agent:b")
        .unwrap();

    let scope_a = NamespaceScope::single(ns("agent:a"));

    // get: B's Alice is NotFound under scope A.
    assert!(db.get_node_scoped(a_alice, &scope_a).is_ok());
    assert!(matches!(
        db.get_node_scoped(b_alice, &scope_a),
        Err(Error::Storage(StorageError::NodeNotFound(_)))
    ));

    // list: only A's node.
    let listed = db.list_nodes_scoped(Some("Person"), &scope_a).unwrap();
    assert!(listed.contains(&a_alice));
    assert!(!listed.contains(&b_alice));

    // find_by_property: only A's Alice.
    let found = db
        .find_nodes_by_property_scoped("Person", "name", &PropertyValue::from("Alice"), &scope_a)
        .unwrap();
    assert_eq!(found, vec![a_alice]);
}

#[test]
fn t5_unknown_namespace_scope_is_not_found() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node_in_namespace("Person", props("A"), "agent:a")
        .unwrap();
    // "ghost" was never registered nor written.
    let scope = NamespaceScope::single(ns("ghost"));
    assert!(matches!(
        db.get_node_scoped(a, &scope),
        Err(Error::Namespace(NamespaceError::NotFound { .. }))
    ));
    assert!(matches!(
        db.list_nodes_scoped(None, &scope),
        Err(Error::Namespace(NamespaceError::NotFound { .. }))
    ));
}

#[test]
fn t5_empty_scope_list_is_invalid_argument() {
    // Constructor rejects an empty list.
    assert!(matches!(
        NamespaceScope::list(vec![]),
        Err(NamespaceError::InvalidName { .. })
    ));
    // And validation rejects a manually-built empty List.
    let db = AletheiaDB::new().unwrap();
    let empty = NamespaceScope::List(vec![]);
    assert!(matches!(
        db.list_nodes_scoped(None, &empty),
        Err(Error::Namespace(NamespaceError::InvalidName { .. }))
    ));
}

// ============================================================================
// T8 — per-namespace stats & schema counts.
// ============================================================================

#[test]
fn t8_per_namespace_counts_in_stats_and_schema() {
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
    let _d1 = db.create_node("Person", props("d1")).unwrap();
    db.create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
        .unwrap();

    let by_name = |v: &[aletheiadb::NamespaceCount], name: &str| -> (usize, usize) {
        v.iter()
            .find(|c| c.name == name)
            .map(|c| (c.node_count, c.edge_count))
            .unwrap_or((0, 0))
    };

    let stats = db.stats();
    assert_eq!(by_name(&stats.namespaces, "agent:a"), (2, 1));
    assert_eq!(by_name(&stats.namespaces, "agent:b"), (1, 0));
    assert_eq!(by_name(&stats.namespaces, "default"), (1, 0));

    let schema = db.schema().unwrap();
    assert_eq!(by_name(&schema.namespaces, "agent:a"), (2, 1));
    assert_eq!(by_name(&schema.namespaces, "agent:b"), (1, 0));
    assert_eq!(by_name(&schema.namespaces, "default"), (1, 0));
}

// ============================================================================
// Membership-index integrity vs full scan (create + delete).
// ============================================================================

#[test]
fn membership_index_matches_scan_after_creates_and_deletes() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let _a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    let b1 = db
        .create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();
    let _d1 = db.create_node("Person", props("d1")).unwrap();

    // Delete one from agent:a and one from agent:b.
    db.delete_node_with_valid_time(a1, None).unwrap();
    db.delete_node_with_valid_time(b1, None).unwrap();

    let scan = node_counts_by_scan(&db);
    let index: BTreeMap<String, usize> = db
        .namespace_counts()
        .into_iter()
        .filter(|c| c.node_count > 0)
        .map(|c| (c.name, c.node_count))
        .collect();
    assert_eq!(index, scan, "membership index must match a full scan");
}

// ============================================================================
// Membership-index rebuilt after restart / WAL replay.
// ============================================================================

#[test]
fn membership_index_rebuilt_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");

    let a = {
        let db = AletheiaDB::open(&path).unwrap();
        let a = db
            .create_node_in_namespace("Person", props("A"), "agent:a")
            .unwrap();
        let _b = db
            .create_node_in_namespace("Person", props("B"), "agent:b")
            .unwrap();
        drop(db);
        a
    };

    // Reopen: the membership index is rebuilt from the durable ride-along
    // property during load, so scoped reads still isolate correctly.
    let db = AletheiaDB::open(&path).unwrap();
    let scope_a = NamespaceScope::single(ns("agent:a"));
    let listed = db.list_nodes_scoped(Some("Person"), &scope_a).unwrap();
    assert_eq!(
        listed,
        vec![a],
        "only agent:a's node visible under scope {{a}}"
    );

    let count = |name: &str| {
        db.namespace_counts()
            .into_iter()
            .find(|c| c.name == name)
            .map(|c| c.node_count)
            .unwrap_or(0)
    };
    assert_eq!(count("agent:a"), 1);
    assert_eq!(count("agent:b"), 1);
}

// ============================================================================
// Back-compat: omitted-scope reads over default data are unchanged.
// ============================================================================

#[test]
fn back_compat_default_scope_matches_unscoped() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db.create_node("Person", props("one")).unwrap();
    let n2 = db.create_node("Person", props("two")).unwrap();

    let default_scope = NamespaceScope::default();
    let mut scoped = db.list_nodes_scoped(None, &default_scope).unwrap();
    scoped.sort_unstable();
    let mut all = db.get_all_node_ids();
    all.sort_unstable();
    assert_eq!(scoped, all, "default-scope list equals the full node set");

    // Scoped get over default data equals plain get.
    assert_eq!(
        db.get_node_scoped(n1, &default_scope).unwrap().id,
        db.get_node(n1).unwrap().id
    );
    assert_eq!(
        db.get_node_scoped(n2, &default_scope).unwrap().id,
        db.get_node(n2).unwrap().id
    );
}
