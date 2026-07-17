//! Namespaces PR2 — vector / temporal / QueryBuilder threading + isolation soak
//! (Issue #3349). Complements `namespaces_pr2.rs` (T4/T5/T8) with:
//!
//! - **T6** filter-complete namespace-scoped vector k-NN (`find_similar_scoped`).
//! - **T7** scope × as_of bi-temporal composition (`*_at_time_scoped`,
//!   `find_nodes_at_time_scoped`, `traverse_scoped_as_of`).
//! - **QueryBuilder** `.in_namespace()` / `.in_namespaces()` /
//!   `.in_all_namespaces()` executor threading.
//! - a **gating** 2×2K interleaved isolation soak.
//! - **informational** (`#[ignore]`d) T11 soak and T12 perf harnesses that can be
//!   run on demand (`cargo test --release -- --ignored`).

use aletheiadb::core::error::StorageError;
use aletheiadb::core::temporal::time;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::{AletheiaDB, Error, Namespace, NamespaceScope, PropertyMapBuilder, PropertyValue};
use std::collections::BTreeSet;
use std::sync::Arc;

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

// ============================================================================
// T6 — filter-complete namespace-scoped vector k-NN.
// ============================================================================

/// Scoped k-NN returns exactly `k` in-scope results, none from another
/// namespace, even when the closest neighbors overall live out of scope — which
/// is only possible if the search over-fetches (filter-complete), rather than
/// taking `k` then dropping the out-of-scope ones.
#[test]
fn t6_scoped_knn_is_filter_complete_and_never_leaks() {
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Query anchor lives in agent:a with embedding pointing "east".
    let q = db
        .create_node_in_namespace("Doc", vec_props("q", &[1.0, 0.0, 0.0, 0.0]), "agent:a")
        .unwrap();

    // Seed MANY out-of-scope (agent:b) near-neighbors that are *closer* to q
    // than most in-scope nodes — these occupy the top of an unscoped ranking, so
    // a naive "take k then filter" would return < k in-scope results.
    let mut b_nodes = BTreeSet::new();
    for i in 0..20 {
        let jitter = i as f32 * 0.0005;
        let id = db
            .create_node_in_namespace(
                "Doc",
                vec_props(&format!("b{i}"), &[1.0 - jitter, jitter, 0.0, 0.0]),
                "agent:b",
            )
            .unwrap();
        b_nodes.insert(id);
    }

    // In-scope (agent:a) nodes, deliberately a bit further from q than the b's.
    let mut a_nodes = BTreeSet::new();
    for i in 0..10 {
        let spread = 0.02 + i as f32 * 0.01;
        let id = db
            .create_node_in_namespace(
                "Doc",
                vec_props(&format!("a{i}"), &[1.0 - spread, 0.0, spread, 0.0]),
                "agent:a",
            )
            .unwrap();
        a_nodes.insert(id);
    }

    let k = 5;
    let scope_a = NamespaceScope::single(ns("agent:a"));
    let results = db.find_similar_scoped(q, k, &scope_a).unwrap();

    // Filter-complete: exactly k results despite the closer out-of-scope crowd.
    assert_eq!(
        results.len(),
        k,
        "scoped k-NN must over-fetch to return k in-scope results"
    );
    // Never leaks: every result is in agent:a, none from agent:b, never q.
    for (id, _score) in &results {
        assert!(a_nodes.contains(id), "result {id} must be an agent:a node");
        assert!(!b_nodes.contains(id), "result {id} leaked from agent:b");
        assert_ne!(*id, q, "query node must be excluded");
    }

    // Prove over-fetch happened: an unscoped search of the same k is dominated by
    // agent:b, so the scoped result set is disjoint from it.
    let unscoped = db
        .find_similar_scoped(q, k, &NamespaceScope::all())
        .unwrap();
    let unscoped_has_b = unscoped.iter().any(|(id, _)| b_nodes.contains(id));
    assert!(
        unscoped_has_b,
        "sanity: unscoped top-k should include the closer agent:b nodes"
    );
}

#[test]
fn t6_scoped_knn_by_embedding_scopes_and_over_fetches() {
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    let mut a_nodes = BTreeSet::new();
    for i in 0..8 {
        let spread = 0.02 + i as f32 * 0.01;
        a_nodes.insert(
            db.create_node_in_namespace(
                "Doc",
                vec_props(&format!("a{i}"), &[1.0 - spread, 0.0, spread, 0.0]),
                "agent:a",
            )
            .unwrap(),
        );
    }
    for i in 0..20 {
        let jitter = i as f32 * 0.0005;
        db.create_node_in_namespace(
            "Doc",
            vec_props(&format!("b{i}"), &[1.0 - jitter, jitter, 0.0, 0.0]),
            "agent:b",
        )
        .unwrap();
    }

    let query = [1.0f32, 0.0, 0.0, 0.0];
    let k = 4;
    let results = db
        .find_similar_by_embedding_scoped(&query, k, &NamespaceScope::single(ns("agent:a")))
        .unwrap();
    assert_eq!(results.len(), k, "must return k in-scope results");
    for (id, _) in &results {
        assert!(a_nodes.contains(id), "result {id} must be agent:a");
    }
}

// ============================================================================
// T7 — scope × as_of bi-temporal composition.
// ============================================================================

#[test]
fn t7_scoped_point_in_time_reads_compose_and_past_is_stable() {
    let db = AletheiaDB::new().unwrap();

    // t0: agent:a has Alice; agent:b has its own Alice.
    let a_alice = db
        .create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    let _b_alice = db
        .create_node_in_namespace("Person", props("Alice"), "agent:b")
        .unwrap();
    let t1 = time::now();

    // Later writes must NOT change the answer to a query anchored at t1.
    let _a_bob = db
        .create_node_in_namespace("Person", props("Bob"), "agent:a")
        .unwrap();

    let scope_a = NamespaceScope::single(ns("agent:a"));

    // get_node_at_time_scoped: a_alice visible in scope A at t1.
    let got = db
        .get_node_at_time_scoped(a_alice, t1, t1, &scope_a)
        .unwrap();
    assert_eq!(got.id, a_alice);

    // Point-in-time find scoped to A at t1: only Alice existed then in A (Bob was
    // created after t1), and B's Alice is never in scope A.
    let found = db
        .find_nodes_at_time_scoped("Person", t1, t1, &scope_a)
        .unwrap();
    let names: BTreeSet<String> = found
        .nodes
        .iter()
        .map(|n| {
            n.properties
                .get("name")
                .and_then(PropertyValue::as_str)
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["Alice".to_string()]),
        "as-of t1 + scope A sees only A's Alice; later Bob and B's Alice excluded"
    );
    let ids: BTreeSet<_> = found.nodes.iter().map(|n| n.id).collect();
    assert!(ids.contains(&a_alice));

    // Namespace filter runs after reconstruction: B's node is NotFound in scope A.
    let b_alice2 = db
        .create_node_in_namespace("Person", props("Carol"), "agent:b")
        .unwrap();
    let t2 = time::now();
    assert!(matches!(
        db.get_node_at_time_scoped(b_alice2, t2, t2, &scope_a),
        Err(Error::Storage(StorageError::NodeNotFound(_)))
    ));
}

#[test]
fn t7_traverse_scoped_as_of_honors_boundary_and_time() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    let b1 = db
        .create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();
    // In-scope edge a1->a2 (agent:a) and cross-namespace edge a1->b1 (agent:a).
    db.create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
        .unwrap();
    db.create_edge_in_namespace(a1, b1, "KNOWS", props(""), "agent:a")
        .unwrap();
    let t = time::now();

    // As-of t, scope {a}: a2 reachable, b1's namespace ∉ scope ⇒ not crossed.
    let scope_a = NamespaceScope::single(ns("agent:a"));
    let reachable = db
        .traverse_scoped_as_of(a1, None, 5, t, t, &scope_a)
        .unwrap();
    assert!(
        reachable.contains(&a2),
        "a2 reachable as-of t under scope a"
    );
    assert!(
        !reachable.contains(&b1),
        "b1 not crossed (target ns ∉ scope)"
    );

    // Union scope {a,b}: now b1 is reachable too.
    let scope_ab = NamespaceScope::list(vec![ns("agent:a"), ns("agent:b")]).unwrap();
    let reachable = db
        .traverse_scoped_as_of(a1, None, 5, t, t, &scope_ab)
        .unwrap();
    assert!(reachable.contains(&a2));
    assert!(reachable.contains(&b1));
}

// ============================================================================
// QueryBuilder namespace surface.
// ============================================================================

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

#[test]
fn querybuilder_in_namespace_filters_start_and_traversal() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    let b1 = db
        .create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();
    db.create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
        .unwrap();
    // Cross-namespace edge a1 -> b1, living in agent:a.
    db.create_edge_in_namespace(a1, b1, "KNOWS", props(""), "agent:a")
        .unwrap();

    // Traverse from a1 scoped to agent:a: reaches a2, never bridges to b1.
    let q = db
        .query()
        .start(a1)
        .traverse("KNOWS")
        .in_namespace(ns("agent:a"))
        .build();
    let ids = result_node_ids(&db, q);
    assert!(ids.contains(&a2.as_u64()), "a2 in scope a is returned");
    assert!(
        !ids.contains(&b1.as_u64()),
        "b1 (agent:b) must not be returned under scope a"
    );

    // in_all_namespaces: both neighbors returned.
    let q = db
        .query()
        .start(a1)
        .traverse("KNOWS")
        .in_all_namespaces()
        .build();
    let ids = result_node_ids(&db, q);
    assert!(ids.contains(&a2.as_u64()));
    assert!(ids.contains(&b1.as_u64()), "all-namespaces sees b1 too");
}

#[test]
fn querybuilder_in_namespace_filters_start_node() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();

    // Starting from a1 but scoping to agent:b returns nothing (a1 ∉ b).
    let q = db.query().start(a1).in_namespace(ns("agent:b")).build();
    let ids = result_node_ids(&db, q);
    assert!(ids.is_empty(), "a1 is not visible under scope agent:b");

    // Scoping to agent:a returns a1.
    let q = db.query().start(a1).in_namespace(ns("agent:a")).build();
    let ids = result_node_ids(&db, q);
    assert_eq!(ids, BTreeSet::from([a1.as_u64()]));
}

#[test]
fn querybuilder_in_namespaces_union_scan() {
    let db = AletheiaDB::new().unwrap();
    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let b1 = db
        .create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();
    let _c1 = db
        .create_node_in_namespace("Person", props("c1"), "agent:c")
        .unwrap();

    let q = db
        .query()
        .scan_label("Person")
        .in_namespaces([ns("agent:a"), ns("agent:b")])
        .build();
    let ids = result_node_ids(&db, q);
    assert_eq!(ids, BTreeSet::from([a1.as_u64(), b1.as_u64()]));
}

// ============================================================================
// GATING soak — 2×2K interleaved writes, zero cross-namespace contamination.
// ============================================================================

#[test]
fn soak_2x2k_interleaved_writes_zero_contamination() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let per_thread = 2_000usize;

    let clients = [("agent:planner", "P"), ("agent:researcher", "R")];
    let handles: Vec<_> = clients
        .iter()
        .map(|(namespace, tag)| {
            let db = Arc::clone(&db);
            let namespace = namespace.to_string();
            let tag = tag.to_string();
            std::thread::spawn(move || {
                for i in 0..per_thread {
                    db.create_node_in_namespace("Memory", props(&format!("{tag}-{i}")), &namespace)
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    // Post-hoc scoped reads: each namespace holds exactly its own 2000, none from
    // the other.
    for (namespace, _tag) in &clients {
        let scope = NamespaceScope::single(ns(namespace));
        let listed = db.list_nodes_scoped(Some("Memory"), &scope).unwrap();
        assert_eq!(
            listed.len(),
            per_thread,
            "namespace {namespace} must hold exactly {per_thread} nodes"
        );
        // Every listed node actually belongs to this namespace.
        for id in listed {
            assert_eq!(db.get_node(id).unwrap().namespace().as_str(), *namespace);
        }
    }

    // Counts corroborate via the membership index.
    let count = |name: &str| {
        db.namespace_counts()
            .into_iter()
            .find(|c| c.name == name)
            .map(|c| c.node_count)
            .unwrap_or(0)
    };
    assert_eq!(count("agent:planner"), per_thread);
    assert_eq!(count("agent:researcher"), per_thread);
}

// ============================================================================
// INFORMATIONAL (not gating) — T11 larger soak + T12 perf harness.
// Run with: cargo test --release -- --ignored --nocapture
// ============================================================================

#[test]
#[ignore = "informational T11 soak (AC8): run on demand with --release --ignored"]
fn t11_soak_20k_interleaved_writes_informational() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let per_thread = 20_000usize;
    let clients = ["agent:a", "agent:b"];
    let handles: Vec<_> = clients
        .iter()
        .map(|namespace| {
            let db = Arc::clone(&db);
            let namespace = namespace.to_string();
            std::thread::spawn(move || {
                for i in 0..per_thread {
                    db.create_node_in_namespace("Memory", props(&format!("{i}")), &namespace)
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    for namespace in &clients {
        let scope = NamespaceScope::single(ns(namespace));
        let listed = db.list_nodes_scoped(Some("Memory"), &scope).unwrap();
        assert_eq!(listed.len(), per_thread, "{namespace} isolation at 20K");
    }
    eprintln!("T11 soak: 2x{per_thread} interleaved writes, 0 contamination");
}

#[test]
#[ignore = "informational T12 perf: run on demand with --release --ignored --nocapture"]
fn t12_scoped_single_hop_p99_informational() {
    use std::time::Instant;

    let p99 = |mut v: Vec<u128>| -> f64 {
        v.sort_unstable();
        v[((v.len() as f64 * 0.99) as usize).min(v.len() - 1)] as f64
    };

    // Like-for-like: the SAME traversal method with vs without a restricting
    // scope. `All` runs the identical BFS machinery (dedup set, queue, adjacency
    // scan, zero-copy target lookup) but short-circuits the namespace check;
    // `single` adds `validate_scope` + two O(1) membership probes per edge. Their
    // ratio isolates the *cost of scoping* (not the traversal API vs a raw loop).
    let scope_a = NamespaceScope::single(ns("agent:a"));
    let scope_all = NamespaceScope::all();

    // Measure the scoping overhead at two fan-outs: the canonical single-edge
    // hop (the literal "<1µs single-hop" target) and a wide 1000-edge fan-out
    // (amplifies per-edge probe cost).
    for fanout in [1usize, 1_000usize] {
        let db = AletheiaDB::new().unwrap();
        let a1 = db
            .create_node_in_namespace("Person", props("a1"), "agent:a")
            .unwrap();
        for i in 0..fanout {
            let n = db
                .create_node_in_namespace("Person", props(&format!("n{i}")), "agent:a")
                .unwrap();
            db.create_edge_in_namespace(a1, n, "KNOWS", props(""), "agent:a")
                .unwrap();
        }

        let iters = 5_000usize;
        let mut unscoped = Vec::with_capacity(iters);
        let mut scoped = Vec::with_capacity(iters);
        for _ in 0..500 {
            let _ = db.traverse_scoped(a1, Some("KNOWS"), 1, &scope_all);
            let _ = db.traverse_scoped(a1, Some("KNOWS"), 1, &scope_a);
        }
        for _ in 0..iters {
            let t = Instant::now();
            std::hint::black_box(db.traverse_scoped(a1, Some("KNOWS"), 1, &scope_all)).ok();
            unscoped.push(t.elapsed().as_nanos());
            let t = Instant::now();
            std::hint::black_box(db.traverse_scoped(a1, Some("KNOWS"), 1, &scope_a)).ok();
            scoped.push(t.elapsed().as_nanos());
        }
        let u = p99(unscoped);
        let s = p99(scoped);
        eprintln!(
            "T12 like-for-like p99 (fan-out {fanout}): unscoped(All)={u:.0}ns \
             scoped(single)={s:.0}ns ratio={:.3} (target <= 1.20)",
            s / u
        );
    }
}
