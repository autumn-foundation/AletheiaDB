//! Issue #3549 — replace / tombstone (non-PATCH) node & edge write API.
//!
//! `update_node`/`update_edge` are PATCH-merge: keys absent from the incoming
//! map are preserved. The replace API is a full overwrite — the resulting
//! property map is *exactly* the supplied map (any prior key not present is
//! removed from current state, while history is preserved), and for nodes the
//! label is overwritten too. `remove_node_property`/`remove_edge_property` are
//! read-modify-replace conveniences that drop a single key.
//!
//! These are end-to-end API tests against the public `AletheiaDB` surface.

use aletheiadb::api::transaction::{ReadOps, WriteOps};
use aletheiadb::core::GLOBAL_INTERNER;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::time;
use aletheiadb::core::{EdgeId, NodeId};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

fn node_label(n: &aletheiadb::core::graph::Node) -> Option<String> {
    GLOBAL_INTERNER.resolve_with(n.label, |s| s.to_string())
}

fn edge_label(e: &aletheiadb::core::graph::Edge) -> Option<String> {
    GLOBAL_INTERNER.resolve_with(e.label, |s| s.to_string())
}

fn props3(name: &str, age: i64, city: &str) -> aletheiadb::core::PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", name)
        .insert("age", age)
        .insert("city", city)
        .build()
}

/// Case 1: replace overwrites the whole map — a key present before but absent
/// from the replacement is gone from current state.
#[test]
fn replace_node_removes_absent_key_current_state() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("create");

    // Replace with only {name} — age and city must disappear.
    db.replace_node(
        id,
        "Person",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .expect("replace");

    let node = db.get_node(id).expect("get");
    assert!(node.get_property("name").is_some(), "name kept");
    assert!(
        node.get_property("age").is_none(),
        "age removed by replace, got {:?}",
        node.get_property("age")
    );
    assert!(
        node.get_property("city").is_none(),
        "city removed by replace"
    );
}

/// Case 2: history is preserved — anchoring BOTH bi-temporal dimensions before
/// the replace still shows the removed keys.
#[test]
fn replace_node_preserves_history_as_of_earlier() {
    let db = AletheiaDB::new().expect("db");

    // Backdate the create so valid-time anchoring is unambiguous.
    let create_valid =
        HybridTimestamp::new(time::now().wallclock() - 3_600_000_000, 0).expect("ts");
    let id = db
        .create_node_with_valid_time("Person", props3("Alice", 30, "London"), Some(create_valid))
        .expect("create");

    // Capture a transaction-time coordinate strictly between the create commit
    // and the replace commit.
    let tx_before_replace = time::now();

    db.replace_node(
        id,
        "Person",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .expect("replace");

    // Anchor BOTH dimensions before the replace: the old version is recalled
    // with all its keys.
    let old = db
        .get_node_at_time(id, create_valid, tx_before_replace)
        .expect("as-of read");
    assert_eq!(
        old.get_property("age").and_then(|v| v.as_int()),
        Some(30),
        "age recalled as-of earlier"
    );
    assert!(
        old.get_property("city").is_some(),
        "city recalled as-of earlier"
    );

    // Current state still lacks them.
    let now = db.get_node(id).expect("get");
    assert!(now.get_property("age").is_none(), "age gone now");
}

/// Case 4 (critical risk): a replace-with-key-removed stored as a **Delta**
/// (2nd version, well under the default anchor interval of 10) must reconstruct
/// to a map WITHOUT the removed key when replayed through the anchor+delta
/// chain (`get_node_at_version` reconstructs from historical, not current).
#[test]
fn replace_removal_survives_delta_reconstruction() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("create"); // v1 = anchor

    db.replace_node(
        id,
        "Person",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .expect("replace"); // v2 = delta (removes age, city)

    // Prove the version really was stored as a delta (not an anchor snapshot).
    let stats = db.historical_stats().expect("stats");
    assert!(
        stats.node_delta_count >= 1,
        "the replace must be stored as a delta to exercise delta removal, delta_count={}",
        stats.node_delta_count
    );

    // Clear the property reconstruction cache FIRST. `HistoricalStorage`
    // populates `node_property_cache` with the fully-materialized map for every
    // version at write time, so a cached `get_node_at_version` would return
    // that map WITHOUT ever running `reconstruct_node_properties_iterative` /
    // `PropertyDelta::apply`'s removed-set subtraction — a broken delta
    // encoding would still pass. Clearing forces genuine reconstruction from
    // the anchor+delta chain.
    db.__test_historical_storage()
        .read()
        .__test_clear_property_cache();

    // Reconstruct v2 THROUGH the delta chain (anchor v1 + delta v2).
    let v2 = db.get_node_at_version(id, 2).expect("version 2");
    assert!(
        v2.get_property("age").is_none(),
        "delta must encode key REMOVAL: age still present after reconstruction"
    );
    assert!(
        v2.get_property("city").is_none(),
        "delta must encode key REMOVAL: city still present after reconstruction"
    );
    assert!(v2.get_property("name").is_some(), "name retained");
}

/// Case 5: node replace can mutate the label; the old label survives as-of
/// earlier and current-state label indexing is consistent.
#[test]
fn replace_node_mutates_label() {
    let db = AletheiaDB::new().expect("db");

    let create_valid =
        HybridTimestamp::new(time::now().wallclock() - 3_600_000_000, 0).expect("ts");
    let id = db
        .create_node_with_valid_time(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            Some(create_valid),
        )
        .expect("create");
    let tx_before = time::now();

    // Replace with a NEW label.
    db.replace_node(
        id,
        "Employee",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .expect("replace");

    // Current-state label index reflects the new label.
    let employees = db.get_nodes_by_label("Employee");
    assert_eq!(employees.len(), 1, "one Employee now");
    assert_eq!(employees[0].id, id);
    assert!(
        db.get_nodes_by_label("Person").is_empty(),
        "no Person in current state after relabel"
    );

    let node = db.get_node(id).expect("get");
    assert_eq!(
        node_label(&node).as_deref(),
        Some("Employee"),
        "current label"
    );

    // Old label survives as-of earlier (both dimensions anchored).
    let old = db
        .get_node_at_time(id, create_valid, tx_before)
        .expect("as-of");
    assert_eq!(
        node_label(&old).as_deref(),
        Some("Person"),
        "old label recalled as-of earlier"
    );
}

/// Case 6: edge replace overwrites properties only; source, target, and edge
/// type remain immutable.
#[test]
fn replace_edge_properties_only_endpoints_immutable() {
    let db = AletheiaDB::new().expect("db");
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .expect("a");
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "B").build())
        .expect("b");
    let e = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new()
                .insert("since", 2020i64)
                .insert("weight", 5i64)
                .build(),
        )
        .expect("edge");

    // Replace with only {since: 2024}. weight must vanish; endpoints/type stay.
    db.replace_edge(
        e,
        PropertyMapBuilder::new().insert("since", 2024i64).build(),
    )
    .expect("replace edge");

    let edge = db.get_edge(e).expect("get edge");
    assert_eq!(edge.source, alice, "source immutable");
    assert_eq!(edge.target, bob, "target immutable");
    assert_eq!(
        edge_label(&edge).as_deref(),
        Some("KNOWS"),
        "type immutable"
    );
    assert_eq!(
        edge.get_property("since").and_then(|v| v.as_int()),
        Some(2024),
        "since overwritten"
    );
    assert!(
        edge.get_property("weight").is_none(),
        "weight removed by replace"
    );
}

/// Case 7: removing an absent key is a no-op success that records NO new
/// version.
#[test]
fn remove_absent_property_is_noop_success() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create");

    let versions_before = db.get_node_history(id).expect("hist").version_count();

    // "age" is not present — must succeed and record no version.
    db.remove_node_property(id, "age").expect("noop remove");

    let versions_after = db.get_node_history(id).expect("hist").version_count();
    assert_eq!(
        versions_before, versions_after,
        "no-op remove must not record a new version"
    );

    // The node is untouched.
    let node = db.get_node(id).expect("get");
    assert!(node.get_property("name").is_some());
}

/// Case 7b: removing a present key drops it and records a version.
#[test]
fn remove_present_property_drops_key() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("create");

    let before = db.get_node_history(id).expect("hist").version_count();
    db.remove_node_property(id, "age").expect("remove age");
    let after = db.get_node_history(id).expect("hist").version_count();
    assert_eq!(after, before + 1, "removal records exactly one new version");

    let node = db.get_node(id).expect("get");
    assert!(node.get_property("age").is_none(), "age dropped");
    assert!(node.get_property("name").is_some(), "name kept");
    assert!(
        node.get_property("city").is_some(),
        "city kept (single-key remove)"
    );
}

/// Case 7c: `remove_edge_property` mirror.
#[test]
fn remove_edge_property_drops_key() {
    let db = AletheiaDB::new().expect("db");
    let a = db
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("a");
    let b = db
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("b");
    let e = db
        .create_edge(
            a,
            b,
            "R",
            PropertyMapBuilder::new()
                .insert("x", 1i64)
                .insert("y", 2i64)
                .build(),
        )
        .expect("edge");

    db.remove_edge_property(e, "y").expect("remove y");
    let edge = db.get_edge(e).expect("get");
    assert_eq!(edge.get_property("x").and_then(|v| v.as_int()), Some(1));
    assert!(edge.get_property("y").is_none(), "y removed");

    // Absent-key no-op.
    let before = db.get_edge_history(e).expect("hist").version_count();
    db.remove_edge_property(e, "zzz").expect("noop");
    let after = db.get_edge_history(e).expect("hist").version_count();
    assert_eq!(before, after, "no-op edge remove records nothing");
}

/// Case 8: replace with an empty map removes ALL properties; the node still
/// exists.
#[test]
fn replace_node_empty_map_removes_all_props_but_keeps_node() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("create");

    db.replace_node(id, "Person", PropertyMapBuilder::new().build())
        .expect("replace empty");

    let node = db.get_node(id).expect("node still exists");
    assert_eq!(node.id, id);
    assert!(node.get_property("name").is_none(), "name removed");
    assert!(node.get_property("age").is_none(), "age removed");
    assert!(node.get_property("city").is_none(), "city removed");
}

/// Case 9 (regression): PATCH `update_node`/`update_edge` semantics are
/// UNCHANGED — keys not in the incoming map are preserved.
#[test]
fn update_still_patches_not_replaces() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("create");

    // PATCH: only age changes; name & city preserved.
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("age", 31i64).build(),
        None,
    )
    .expect("update");

    let node = db.get_node(id).expect("get");
    assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
    assert!(node.get_property("name").is_some(), "PATCH preserves name");
    assert!(node.get_property("city").is_some(), "PATCH preserves city");
}

/// Case 12: valid_time + provenance options are honored on replace, at parity
/// with update.
#[test]
fn replace_honors_valid_time_and_provenance() {
    use aletheiadb::Provenance;
    use aletheiadb::api::transaction::WriteRequestOptions;

    let db = AletheiaDB::new().expect("db");

    let create_valid =
        HybridTimestamp::new(time::now().wallclock() - 7_200_000_000, 0).expect("ts");
    let id = db
        .create_node_with_valid_time("Person", props3("Alice", 30, "London"), Some(create_valid))
        .expect("create");

    // A transaction-time coordinate strictly between the create commit and the
    // replace commit — needed to recall the pre-replace valid history (both
    // bi-temporal dimensions must be anchored before the superseding write).
    let tx_before_replace = time::now();

    // Replace effective from 1 hour ago (after creation) with provenance.
    let replace_valid =
        HybridTimestamp::new(time::now().wallclock() - 3_600_000_000, 0).expect("ts");
    let prov = Provenance::builder()
        .source("hr-system")
        .confidence(0.9)
        .build()
        .expect("prov");

    db.replace_node_with_options(
        id,
        "Person",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
        WriteRequestOptions::new()
            .with_valid_from(replace_valid)
            .with_provenance(prov),
    )
    .expect("replace with options");

    // Provenance recorded on the current (replacement) version.
    let got = db.get_node_provenance(id).expect("prov read");
    assert_eq!(
        got.and_then(|p| p.source().map(|s| s.to_string())),
        Some("hr-system".to_string()),
        "provenance stamped on replace version"
    );

    // valid_from honored on the NEW version: at (valid = 30 min ago, tx = now)
    // — after replace_valid (1h ago) — only {name} remains (age gone).
    let after_replace =
        HybridTimestamp::new(time::now().wallclock() - 1_800_000_000, 0).expect("ts");
    let new_ver = db
        .get_node_at_time(id, after_replace, time::now())
        .expect("as-of after replace_valid");
    assert!(
        new_ver.get_property("age").is_none(),
        "at/after replace_valid the replacement map (no age) is valid"
    );

    // Half-open interval [valid_from, valid_to) boundary exactness: AT EXACTLY
    // replace_valid the NEW replacement map holds (the interval start is
    // inclusive), so age is already gone at the boundary itself.
    let at_boundary = db
        .get_node_at_time(id, replace_valid, time::now())
        .expect("as-of exactly replace_valid");
    assert!(
        at_boundary.get_property("age").is_none(),
        "at EXACTLY replace_valid the replacement map (no age) holds — half-open interval start is inclusive"
    );

    // Before replace_valid the OLD map (with age) is valid — anchor BOTH
    // dimensions before the replace commit.
    let mid = HybridTimestamp::new(time::now().wallclock() - 5_400_000_000, 0).expect("ts"); // 1.5h ago
    let old = db
        .get_node_at_time(id, mid, tx_before_replace)
        .expect("as-of mid");
    assert_eq!(
        old.get_property("age").and_then(|v| v.as_int()),
        Some(30),
        "before replace_valid the old map (with age) is valid"
    );
}

/// Case 10: concurrent replace of the same node — snapshot isolation
/// first-committer-wins still fires (`SerializationFailure`).
#[test]
fn concurrent_replace_first_committer_wins() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .expect("create");

    // Two transactions both snapshot the committed base version.
    let mut tx1 = db.write_transaction().expect("tx1");
    tx1.replace_node(
        id,
        "Person",
        PropertyMapBuilder::new().insert("age", 31i64).build(),
    )
    .expect("tx1 replace");

    let mut tx2 = db.write_transaction().expect("tx2");
    tx2.replace_node(
        id,
        "Person",
        PropertyMapBuilder::new().insert("age", 32i64).build(),
    )
    .expect("tx2 replace");

    // tx2 commits first — wins.
    tx2.commit().expect("tx2 commit wins");

    // tx1 must abort with SerializationFailure (write-write conflict).
    let err = tx1.commit().expect_err("tx1 must conflict");
    assert!(
        format!("{err:?}").contains("SerializationFailure"),
        "expected SerializationFailure, got: {err:?}"
    );

    // First committer's value stands.
    let node = db.get_node(id).expect("get");
    assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(32));
}

/// Case 11: replacing away a vector/embedding property updates the vector index
/// so the node is no longer returned by similarity search on that property
/// (while an untouched node remains indexed).
#[test]
fn replace_removes_vector_property_updates_index() {
    use aletheiadb::db::SimilarityQuery;
    use aletheiadb::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().expect("db");
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
        .enable()
        .expect("enable vector index");

    let target = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "target")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .expect("create target");
    let decoy = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "decoy")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                .build(),
        )
        .expect("create decoy");

    // Before removal: searching [1,0,0] finds the target.
    let before = db
        .similarity_search(SimilarityQuery::from_embedding(vec![1.0f32, 0.0, 0.0]).k(10))
        .expect("search before");
    assert!(
        before.iter().any(|(nid, _)| *nid == target),
        "target indexed before replace"
    );

    // Replace away the embedding (keep only name).
    db.replace_node(
        target,
        "Doc",
        PropertyMapBuilder::new().insert("name", "target").build(),
    )
    .expect("replace removes vector");

    let node = db.get_node(target).expect("get");
    assert!(
        node.get_property("embedding").is_none(),
        "embedding removed from current state"
    );

    // After removal: target is gone from the index; decoy still present.
    let after = db
        .similarity_search(SimilarityQuery::from_embedding(vec![1.0f32, 0.0, 0.0]).k(10))
        .expect("search after");
    assert!(
        !after.iter().any(|(nid, _)| *nid == target),
        "target must be removed from the vector index after replace, got {after:?}"
    );
    assert!(
        after.iter().any(|(nid, _)| *nid == decoy),
        "untouched decoy remains indexed"
    );
}

// -------------------------------------------------------------------------
// MAJOR 3 — a removed key cannot resurface when a fresh anchor is rebuilt.
// -------------------------------------------------------------------------

/// After a key is removed, applying enough further updates to force a fresh
/// ANCHOR (default `anchor_interval` = 10) to be materialized from a delta
/// chain that includes the removal must NOT resurrect the removed key: a
/// version reconstructed from/through the rebuilt anchor still lacks it.
#[test]
fn removal_survives_anchor_rebuild() {
    let db = AletheiaDB::new().expect("db");

    // v1 anchor: {a, b, c}
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("a", 1i64)
                .insert("b", 2i64)
                .insert("c", 3i64)
                .build(),
        )
        .expect("create");

    // v2 delta: drop b -> {a, c}
    db.remove_node_property(id, "b").expect("remove b");

    // Apply 10 more PATCH updates (v3..v12), none re-adding b, to force a fresh
    // ANCHOR to be MATERIALIZED past the removal (v11 becomes the 2nd anchor).
    for i in 0..10i64 {
        db.update_node_with_valid_time(
            id,
            PropertyMapBuilder::new().insert("a", 10 + i).build(),
            None,
        )
        .expect("update");
    }

    // Confirm a second anchor was actually built past the removal.
    let stats = db.historical_stats().expect("stats");
    assert!(
        stats.node_anchor_count >= 2,
        "expected a rebuilt anchor past the removal, anchor_count={}",
        stats.node_anchor_count
    );

    // Defeat the write-time property cache so reconstruction genuinely runs
    // the anchor+delta chain (see `replace_removal_survives_delta_reconstruction`).
    db.__test_historical_storage()
        .read()
        .__test_clear_property_cache();

    // Reconstruct the LATEST version (from the rebuilt anchor) — b must be gone.
    let latest = db.get_node_history(id).expect("hist").version_count() as u64;
    let node = db
        .get_node_at_version(id, latest)
        .expect("reconstruct latest version");
    assert!(
        node.get_property("b").is_none(),
        "removed key 'b' must not resurface when a new anchor is materialized"
    );
    assert!(node.get_property("a").is_some(), "a still present");
    assert_eq!(
        node.get_property("c").and_then(|v| v.as_int()),
        Some(3),
        "untouched c still present after anchor rebuild"
    );
}

// -------------------------------------------------------------------------
// MAJOR 4 — error paths return Err (never panic) on missing / deleted ids.
// -------------------------------------------------------------------------

/// `replace_node` errors (not panics) on a never-created id and on a
/// previously-deleted id.
#[test]
fn replace_node_errors_on_missing_and_deleted() {
    let db = AletheiaDB::new().expect("db");

    // (a) never existed
    let missing = NodeId::new(999_999).expect("nid");
    let err = db
        .replace_node(missing, "Person", PropertyMapBuilder::new().build())
        .expect_err("replace on nonexistent node must error");
    assert!(
        format!("{err:?}").contains("NodeNotFound"),
        "expected NodeNotFound, got {err:?}"
    );

    // (b) created then deleted
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create");
    db.delete_node_with_valid_time(id, None).expect("delete");
    let err = db
        .replace_node(id, "Person", PropertyMapBuilder::new().build())
        .expect_err("replace on deleted node must error");
    assert!(
        format!("{err:?}").contains("NodeNotFound"),
        "expected NodeNotFound on deleted node, got {err:?}"
    );
}

/// `replace_edge` errors on a never-created id and on a previously-deleted id.
#[test]
fn replace_edge_errors_on_missing_and_deleted() {
    let db = AletheiaDB::new().expect("db");

    let missing = EdgeId::new(999_999).expect("eid");
    let err = db
        .replace_edge(missing, PropertyMapBuilder::new().build())
        .expect_err("replace on nonexistent edge must error");
    assert!(
        format!("{err:?}").contains("EdgeNotFound"),
        "expected EdgeNotFound, got {err:?}"
    );

    let a = db
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("a");
    let b = db
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("b");
    let e = db
        .create_edge(
            a,
            b,
            "R",
            PropertyMapBuilder::new().insert("x", 1i64).build(),
        )
        .expect("edge");
    db.delete_edge_with_valid_time(e, None)
        .expect("delete edge");
    let err = db
        .replace_edge(e, PropertyMapBuilder::new().build())
        .expect_err("replace on deleted edge must error");
    assert!(
        format!("{err:?}").contains("EdgeNotFound"),
        "expected EdgeNotFound on deleted edge, got {err:?}"
    );
}

/// `remove_node_property` errors on a never-created id and on a deleted id.
#[test]
fn remove_node_property_errors_on_missing_and_deleted() {
    let db = AletheiaDB::new().expect("db");

    let missing = NodeId::new(999_999).expect("nid");
    let err = db
        .remove_node_property(missing, "age")
        .expect_err("remove on nonexistent node must error");
    assert!(
        format!("{err:?}").contains("NodeNotFound"),
        "expected NodeNotFound, got {err:?}"
    );

    let id = db
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("create");
    db.delete_node_with_valid_time(id, None).expect("delete");
    let err = db
        .remove_node_property(id, "age")
        .expect_err("remove on deleted node must error");
    assert!(
        format!("{err:?}").contains("NodeNotFound"),
        "expected NodeNotFound on deleted node, got {err:?}"
    );
}

/// `remove_edge_property` errors on a never-created id and on a deleted id.
#[test]
fn remove_edge_property_errors_on_missing_and_deleted() {
    let db = AletheiaDB::new().expect("db");

    let missing = EdgeId::new(999_999).expect("eid");
    let err = db
        .remove_edge_property(missing, "x")
        .expect_err("remove on nonexistent edge must error");
    assert!(
        format!("{err:?}").contains("EdgeNotFound"),
        "expected EdgeNotFound, got {err:?}"
    );

    let a = db
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("a");
    let b = db
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("b");
    let e = db
        .create_edge(
            a,
            b,
            "R",
            PropertyMapBuilder::new().insert("x", 1i64).build(),
        )
        .expect("edge");
    db.delete_edge_with_valid_time(e, None)
        .expect("delete edge");
    let err = db
        .remove_edge_property(e, "x")
        .expect_err("remove on deleted edge must error");
    assert!(
        format!("{err:?}").contains("EdgeNotFound"),
        "expected EdgeNotFound on deleted edge, got {err:?}"
    );
}

// -------------------------------------------------------------------------
// MINOR — same-transaction read-your-own-writes (buffer-aware, Issue #3417).
// -------------------------------------------------------------------------

/// Inside ONE transaction: create a node then replace it before commit. The
/// replace must see the buffered (still-uncommitted) create, not 404 against
/// committed storage.
#[test]
fn same_tx_create_then_replace_node_ryow() {
    let db = AletheiaDB::new().expect("db");
    let mut tx = db.write_transaction().expect("tx");

    let id = tx
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("buffered create");

    // Replace the still-uncommitted node (full overwrite: only {name}).
    tx.replace_node(
        id,
        "Employee",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .expect("replace must see the buffered create");

    // Read-your-own-writes reflects the replacement immediately.
    let buffered = tx.get_node(id).expect("read own");
    assert!(buffered.get_property("age").is_none(), "age gone in buffer");
    assert_eq!(node_label(&buffered).as_deref(), Some("Employee"));

    tx.commit().expect("commit");

    let node = db.get_node(id).expect("get after commit");
    assert!(node.get_property("name").is_some());
    assert!(node.get_property("age").is_none(), "age removed by replace");
    assert!(node.get_property("city").is_none());
    assert_eq!(node_label(&node).as_deref(), Some("Employee"));
}

/// Inside ONE transaction: create a node then `remove_node_property` on it
/// before commit — the remove must see the buffered create.
#[test]
fn same_tx_create_then_remove_property_node_ryow() {
    let db = AletheiaDB::new().expect("db");
    let mut tx = db.write_transaction().expect("tx");

    let id = tx
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("buffered create");

    tx.remove_node_property(id, "age")
        .expect("remove must see the buffered create");

    let buffered = tx.get_node(id).expect("read own");
    assert!(
        buffered.get_property("age").is_none(),
        "age removed in buffer"
    );
    assert!(buffered.get_property("name").is_some());

    tx.commit().expect("commit");

    let node = db.get_node(id).expect("get after commit");
    assert!(node.get_property("age").is_none());
    assert!(node.get_property("name").is_some());
    assert!(node.get_property("city").is_some());
}

/// Inside ONE transaction: create an edge then replace / remove-property on it
/// before commit — both must see the buffered create.
#[test]
fn same_tx_create_then_replace_edge_ryow() {
    let db = AletheiaDB::new().expect("db");
    let mut tx = db.write_transaction().expect("tx");

    let a = tx
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("a");
    let b = tx
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("b");
    let e = tx
        .create_edge(
            a,
            b,
            "R",
            PropertyMapBuilder::new()
                .insert("x", 1i64)
                .insert("y", 2i64)
                .build(),
        )
        .expect("buffered edge");

    // Replace the still-uncommitted edge (keep only {x: 9}).
    tx.replace_edge(e, PropertyMapBuilder::new().insert("x", 9i64).build())
        .expect("replace must see the buffered edge");

    let buffered = tx.get_edge(e).expect("read own edge");
    assert_eq!(buffered.get_property("x").and_then(|v| v.as_int()), Some(9));
    assert!(buffered.get_property("y").is_none(), "y gone in buffer");
    assert_eq!(buffered.source, a, "endpoints preserved through buffer");
    assert_eq!(buffered.target, b);

    // remove_edge_property on the same buffered edge.
    tx.remove_edge_property(e, "x")
        .expect("remove must see the buffered edge");

    tx.commit().expect("commit");

    let edge = db.get_edge(e).expect("get after commit");
    assert!(edge.get_property("x").is_none(), "x removed");
    assert!(edge.get_property("y").is_none(), "y removed");
    assert_eq!(edge.source, a);
    assert_eq!(edge.target, b);
}

// -------------------------------------------------------------------------
// MINOR — replace on a node with connected edges leaves the edges intact.
// -------------------------------------------------------------------------

/// Replacing (and relabeling) a node with connected edges leaves the edge, its
/// endpoints, and its type unchanged, and the node stays traversable.
#[test]
fn replace_node_with_connected_edges_keeps_edges() {
    let db = AletheiaDB::new().expect("db");
    let a = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .expect("a");
    let b = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "B").build())
        .expect("b");
    let e = db
        .create_edge(
            a,
            b,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .expect("edge");

    // Replace A, including a relabel to Employee.
    db.replace_node(
        a,
        "Employee",
        PropertyMapBuilder::new().insert("n", "A2").build(),
    )
    .expect("replace a");

    // The edge is unchanged: endpoints and type intact.
    let edge = db.get_edge(e).expect("get edge");
    assert_eq!(edge.source, a, "edge source unchanged");
    assert_eq!(edge.target, b, "edge target unchanged");
    assert_eq!(
        edge_label(&edge).as_deref(),
        Some("KNOWS"),
        "type unchanged"
    );
    assert_eq!(
        edge.get_property("since").and_then(|v| v.as_int()),
        Some(2020),
        "edge properties unchanged"
    );

    // A -> B still traversable.
    let outs = db.get_outgoing_edges(a);
    assert!(outs.contains(&e), "A->B edge still present after replace");

    // Endpoints intact; A relabeled, B untouched.
    let na = db.get_node(a).expect("a");
    assert_eq!(node_label(&na).as_deref(), Some("Employee"));
    let nb = db.get_node(b).expect("b");
    assert_eq!(node_label(&nb).as_deref(), Some("Person"));
}

// -------------------------------------------------------------------------
// MINOR — self-loop edge (source == target) replace / remove.
// -------------------------------------------------------------------------

/// `replace_edge` and `remove_edge_property` behave correctly on a self-loop
/// edge (source == target).
#[test]
fn replace_and_remove_on_self_loop_edge() {
    let db = AletheiaDB::new().expect("db");
    let a = db
        .create_node("N", PropertyMapBuilder::new().build())
        .expect("a");
    let e = db
        .create_edge(
            a,
            a,
            "SELF",
            PropertyMapBuilder::new()
                .insert("x", 1i64)
                .insert("y", 2i64)
                .build(),
        )
        .expect("self-loop edge");

    // Replace: keep only {x: 9}; y must vanish, endpoints stay (a, a).
    db.replace_edge(e, PropertyMapBuilder::new().insert("x", 9i64).build())
        .expect("replace self-loop");
    let edge = db.get_edge(e).expect("get");
    assert_eq!(edge.source, a, "self-loop source stays");
    assert_eq!(edge.target, a, "self-loop target stays");
    assert_eq!(edge.get_property("x").and_then(|v| v.as_int()), Some(9));
    assert!(edge.get_property("y").is_none(), "y removed on self-loop");

    // remove_edge_property on the self-loop.
    db.remove_edge_property(e, "x").expect("remove x");
    let edge = db.get_edge(e).expect("get2");
    assert!(edge.get_property("x").is_none(), "x removed on self-loop");
    assert_eq!(edge.source, a);
    assert_eq!(edge.target, a);
}

// -------------------------------------------------------------------------
// MINOR — remove a present key twice: first records a version, second no-ops.
// -------------------------------------------------------------------------

/// Removing a present key records exactly one new version; removing it a second
/// time (now absent) is a no-op that records no further version.
#[test]
fn remove_present_property_twice_second_is_noop() {
    let db = AletheiaDB::new().expect("db");
    let id = db
        .create_node("Person", props3("Alice", 30, "London"))
        .expect("create");

    let v0 = db.get_node_history(id).expect("hist").version_count();

    // First removal of a PRESENT key records +1 version.
    db.remove_node_property(id, "age").expect("remove age #1");
    let v1 = db.get_node_history(id).expect("hist").version_count();
    assert_eq!(
        v1,
        v0 + 1,
        "first removal of present key records one version"
    );

    // Second removal of the now-ABSENT key is a no-op: no new version.
    db.remove_node_property(id, "age")
        .expect("remove age #2 noop");
    let v2 = db.get_node_history(id).expect("hist").version_count();
    assert_eq!(v2, v1, "second (absent) removal records no new version");

    let node = db.get_node(id).expect("get");
    assert!(node.get_property("age").is_none(), "age stays removed");
    assert!(node.get_property("name").is_some());
}

// -------------------------------------------------------------------------
// MINOR — removal delta survives index-persistence save + reload
// (complements the WAL-replay recovery test).
// -------------------------------------------------------------------------

/// A delta-encoded key removal reconstructs correctly after an
/// index-persistence save + reopen (the persisted temporal index restores the
/// anchor+delta chain).
#[test]
fn removal_delta_survives_index_persistence_reload() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let data_dir = tempdir.path().join("db");

    let node_id: u64;
    {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");
        let id = db
            .create_node("Person", props3("Alice", 30, "London"))
            .expect("create"); // v1 anchor
        node_id = id.as_u64();

        db.replace_node(
            id,
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("replace"); // v2 delta (removes age, city)

        // Explicitly persist the indexes (including the temporal chain).
        db.persist_indexes().expect("persist indexes");
        drop(db);
    }

    // Reopen — loads the persisted indexes (current + historical chain).
    let db = AletheiaDB::open(&data_dir).expect("reopen durable db");
    let id = NodeId::new(node_id).expect("nid");

    // Current state carries the reduced map.
    let node = db.get_node(id).expect("get after reload");
    assert!(
        node.get_property("age").is_none(),
        "age must stay removed after index-persistence reload"
    );
    assert!(node.get_property("name").is_some(), "name retained");

    // The delta-encoded v2 reconstructs through the restored chain. Clear the
    // property cache first so reconstruction genuinely runs.
    db.__test_historical_storage()
        .read()
        .__test_clear_property_cache();
    let v2 = db
        .get_node_at_version(id, 2)
        .expect("reconstruct v2 after reload");
    assert!(
        v2.get_property("age").is_none(),
        "delta removal must reconstruct (age absent) after reload"
    );
    assert!(
        v2.get_property("city").is_none(),
        "delta removal must reconstruct (city absent) after reload"
    );
    assert!(v2.get_property("name").is_some(), "name retained in v2");
}

// NOTE (temporal vector index removal — deliberately NOT tested here):
// replacing away a vector property does NOT de-index the *temporal* vector
// index. The temporal-index write path (`CurrentStorage::update_node_direct`
// -> `try_index_temporal_vector`, src/storage/current/mod.rs) only ADDS
// vectors present in the new property map; it never calls
// `try_remove_temporal_vector` when a property is dropped (only
// `delete_node_direct` removes). This add-only behavior is shared with PATCH
// `update_node` and predates Issue #3549 — the replace path correctly buffers
// an `UpdateNode` carrying the reduced map (the CURRENT vector index DOES
// de-index it, see `replace_removes_vector_property_updates_index`). Asserting
// temporal de-indexing would test behavior that does not exist; wiring a
// removal into the temporal update path is a separate follow-up.
