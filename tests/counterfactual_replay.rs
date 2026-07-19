#![cfg(feature = "semantic-temporal")]
//! Behavioral test suite for counterfactual exclusion replay
//! (Issue #3357) — *"the world without source X"*.
//!
//! These tests drive the real DB write path (with genuine `#3224` write-time
//! provenance and real bi-temporal coordinates) to seed histories, then exercise
//! [`AletheiaDB::counterfactual_replay`] and the resulting [`CounterfactualView`].
//!
//! The materialization is implemented, so every test here asserts a real
//! behavioral contract (the suite is green). Each test encodes an
//! acceptance-criterion contract from
//! `docs/plans/2026-07-19-counterfactual-replay.md`.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::id::{EdgeId, EntityId, NodeId};
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::experimental::temporal::counterfactual::{
    CounterfactualConfig, CounterfactualError, DivergenceReport, ExclusionPredicate,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A minimal source-only provenance bundle (the exclusion-predicate target).
fn prov(source: &str) -> Provenance {
    Provenance::builder()
        .source(source)
        .build()
        .expect("valid provenance")
}

/// A single-string-property map.
fn props(key: &str, value: &str) -> PropertyMap {
    PropertyMapBuilder::new().insert(key, value).build()
}

/// Create an attributed node via the genuine write path, returning its id.
fn create_node_by(db: &AletheiaDB, label: &str, properties: PropertyMap, source: &str) -> NodeId {
    db.create_node_with_options(
        label,
        properties,
        WriteRequestOptions::new().with_provenance(prov(source)),
    )
    .expect("create node")
}

/// Update an attributed node (PATCH) via the genuine write path.
fn update_node_by(db: &AletheiaDB, id: NodeId, properties: PropertyMap, source: &str) {
    db.update_node_with_options(
        id,
        properties,
        WriteRequestOptions::new().with_provenance(prov(source)),
    )
    .expect("update node");
}

/// A canonical, order-stable digest of the *entire* observable real-DB state:
/// every node's and edge's full version history (bi-temporal coordinates,
/// properties, labels, provenance). Compared before/after a view lifecycle for
/// AC4 — in an in-memory engine, byte-identity of the (private) WAL is
/// approximated by identity of all observable state + full histories, which is
/// what every read surface can actually witness.
fn structural_digest(db: &AletheiaDB, node_ids: &[NodeId], edge_ids: &[EdgeId]) -> String {
    let mut out = String::new();
    for &id in node_ids {
        out.push_str(&format!("NODE {id}\n"));
        match db.get_node_history(id) {
            Ok(h) => out.push_str(&format!("{h:#?}\n")),
            Err(e) => out.push_str(&format!("ERR {e}\n")),
        }
    }
    for &id in edge_ids {
        out.push_str(&format!("EDGE {id}\n"));
        match db.get_edge_history(id) {
            Ok(h) => out.push_str(&format!("{h:#?}\n")),
            Err(e) => out.push_str(&format!("ERR {e}\n")),
        }
    }
    out
}

/// Compare two divergence reports on their reported quantities (order-normalized
/// entity id sets). Used by the determinism test, which runs against two
/// *separate* DBs whose internal version ids differ — so only the counts and the
/// entity-id membership are comparable, never raw version ids.
fn reports_equivalent(a: &DivergenceReport, b: &DivergenceReport) -> bool {
    let mut a_changed = a.changed_entities().to_vec();
    let mut b_changed = b.changed_entities().to_vec();
    let mut a_removed = a.removed_entities().to_vec();
    let mut b_removed = b.removed_entities().to_vec();
    a_changed.sort();
    b_changed.sort();
    a_removed.sort();
    b_removed.sort();
    a.excluded_writes() == b.excluded_writes()
        && a.unattributed_writes_encountered() == b.unattributed_writes_encountered()
        && a.orphaned_updates() == b.orphaned_updates()
        && a.entities_changed() == b.entities_changed()
        && a.entities_removed() == b.entities_removed()
        && a_changed == b_changed
        && a_removed == b_removed
}

// ---------------------------------------------------------------------------
// Reusable fixture
// ---------------------------------------------------------------------------

/// Named handles into the general-purpose seeded history.
struct Fixture {
    db: AletheiaDB,
    /// Node created by "X" then updated by "Y" — the orphaned-update case.
    orphan_node: NodeId,
    /// Node created by "Y", updated by "X", updated by "Y" — the subtle
    /// re-introduction (no-leak) case.
    chain_node: NodeId,
    /// Node whose whole chain is attributed to "X" (removed entirely when X is
    /// excluded).
    all_x_node: NodeId,
    /// Node written with NO provenance (unattributed — never excluded, AC7).
    unattributed_node: NodeId,
    /// Independent node from a clean source, unaffected by excluding "X".
    clean_node: NodeId,
    /// Edge created by "X" (excluded).
    x_edge: EdgeId,
    /// Edge created by a clean source (unaffected).
    clean_edge: EdgeId,
    /// All node ids created, for the structural digest.
    all_nodes: Vec<NodeId>,
    /// All edge ids created, for the structural digest.
    all_edges: Vec<EdgeId>,
}

/// Seed a small in-memory bi-temporal history containing attributed writes from
/// several sources plus one unattributed write, covering every orphan / chain /
/// removal / edge case the AC contracts touch.
fn seed_fixture() -> Fixture {
    let db = AletheiaDB::new().expect("db");

    // Orphaned-update case: X creates, Y later updates. Excluding X removes the
    // create; Y's update then targets an entity with no surviving prior version.
    let orphan_node = create_node_by(&db, "Doc", props("title", "x-created"), "X");
    update_node_by(&db, orphan_node, props("title", "y-updated"), "Y");

    // Re-introduction case: Y creates {a}, X adds {secret}, Y patches {a}.
    // Excluding X must leave a surviving Y-chain whose current state does NOT
    // contain X's `secret` property (no leak-back).
    let chain_node = create_node_by(&db, "Doc", props("a", "y1"), "Y");
    update_node_by(&db, chain_node, props("secret", "x-only"), "X");
    update_node_by(&db, chain_node, props("a", "y2"), "Y");

    // Whole chain attributed to X: removed entirely when X excluded.
    let all_x_node = create_node_by(&db, "Doc", props("title", "pure-x"), "X");
    update_node_by(&db, all_x_node, props("title", "pure-x-v2"), "X");

    // Unattributed: never excluded, but counted as encountered (AC7).
    let unattributed_node = db
        .create_node_with_options("Doc", props("title", "anon"), WriteRequestOptions::new())
        .expect("create unattributed node");

    // Clean, independent node — untouched by excluding X.
    let clean_node = create_node_by(&db, "Doc", props("title", "clean"), "clean-1");

    // Edges with provenance (edge exclusion coverage).
    let x_edge = db
        .create_edge_with_options(
            clean_node,
            orphan_node,
            "LINKS",
            props("via", "x"),
            WriteRequestOptions::new().with_provenance(prov("X")),
        )
        .expect("create x edge");
    let clean_edge = db
        .create_edge_with_options(
            clean_node,
            chain_node,
            "LINKS",
            props("via", "clean"),
            WriteRequestOptions::new().with_provenance(prov("clean-2")),
        )
        .expect("create clean edge");

    let all_nodes = vec![
        orphan_node,
        chain_node,
        all_x_node,
        unattributed_node,
        clean_node,
    ];
    let all_edges = vec![x_edge, clean_edge];

    Fixture {
        db,
        orphan_node,
        chain_node,
        all_x_node,
        unattributed_node,
        clean_node,
        x_edge,
        clean_edge,
        all_nodes,
        all_edges,
    }
}

/// Result of seeding the "1-of-5 poisoned feed" success-metric fixture.
struct PoisonedFeed {
    db: AletheiaDB,
    /// Nodes whose current state is derived from the poisoned source and must be
    /// recovered (changed or removed) by exclusion. This is the exact
    /// blast-radius the divergence report must identify with zero false
    /// negatives.
    contaminated: Vec<NodeId>,
    /// Nodes from clean sources that must be untouched by the exclusion.
    clean: Vec<NodeId>,
}

/// Seed the AC-success-metric scenario: five sources, one poisoned, several
/// current-state facts contaminated by it (either created outright by the poison
/// feed, or clean facts later overwritten by it).
fn seed_poisoned_feed() -> PoisonedFeed {
    let db = AletheiaDB::new().expect("db");
    let clean_sources = ["feed-1", "feed-2", "feed-3", "feed-4"];
    let poison = "poison";

    let mut contaminated = Vec::new();
    let mut clean = Vec::new();

    // Three facts created outright by the poisoned feed -> removed entirely.
    for i in 0..3 {
        let id = create_node_by(
            &db,
            "Fact",
            props("v", &format!("poison-created-{i}")),
            poison,
        );
        contaminated.push(id);
    }

    // Two clean facts later overwritten by the poison feed -> current state
    // changes back to the clean value when the poison write is excluded.
    for i in 0..2 {
        let src = clean_sources[i % clean_sources.len()];
        let id = create_node_by(&db, "Fact", props("v", &format!("clean-{i}")), src);
        update_node_by(
            &db,
            id,
            props("v", &format!("poison-overwrote-{i}")),
            poison,
        );
        contaminated.push(id);
    }

    // Five untouched clean facts.
    for i in 0..5 {
        let src = clean_sources[i % clean_sources.len()];
        let id = create_node_by(&db, "Fact", props("v", &format!("pristine-{i}")), src);
        clean.push(id);
    }

    PoisonedFeed {
        db,
        contaminated,
        clean,
    }
}

// ---------------------------------------------------------------------------
// AC1 — create a view by predicate; handle names the view
// ---------------------------------------------------------------------------

#[test]
fn ac1_exclude_single_source_returns_named_handle() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    assert_eq!(view.handle().name(), "no-x");
}

#[test]
fn ac1_exclude_source_set_any_of() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-scrapers",
            ExclusionPredicate::sources(["X".into(), "clean-2".into()]),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    // Both the X-edge and the clean-2 edge should be excluded -> the edge is gone.
    assert!(view.get_edge(fx.x_edge).is_err());
    assert!(view.get_edge(fx.clean_edge).is_err());
}

#[test]
fn ac1_exclude_bounded_by_transaction_time_range() {
    // Bounding the predicate to a tx-time window before any write means the
    // source matches but the tx-time gate rejects it -> nothing is excluded, so
    // the view equals the real DB.
    let fx = seed_fixture();
    let long_ago = Timestamp::new(1, 0).expect("ts");
    let also_ago = Timestamp::new(2, 0).expect("ts");
    let predicate =
        ExclusionPredicate::source("X").within_transaction_time(Some(long_ago), Some(also_ago));
    let view = fx
        .db
        .counterfactual_replay("bounded-noop", predicate, CounterfactualConfig::default())
        .expect("view materializes");
    // X's writes fall outside the [1,2) window, so they are kept: the orphan
    // node still shows X's create lineage / Y's update as in the real DB.
    assert_eq!(view.report().excluded_writes(), 0);
    assert!(view.get_node(fx.orphan_node).is_ok());
}

// ---------------------------------------------------------------------------
// AC2 — exclusion-replay semantics (orphaned update, removal, no-leak)
// ---------------------------------------------------------------------------

#[test]
fn ac2_orphaned_update_dropped_and_counted() {
    // X created the node, Y updated it. Excluding X removes the create; Y's
    // update targets an entity with no surviving prior version -> dropped and
    // counted as an orphaned update, and the node is absent from the view.
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    assert!(
        view.get_node(fx.orphan_node).is_err(),
        "orphaned node must be absent from the view"
    );
    // Exact count: only orphan_node's Y-update is orphaned (chain_node's Y-update
    // has a surviving prior, all_x's writes are all excluded not orphaned).
    assert_eq!(
        view.report().orphaned_updates(),
        1,
        "exactly one orphaned update (orphan_node's Y-update)"
    );
    assert!(
        view.report()
            .removed_entities()
            .contains(&EntityId::Node(fx.orphan_node)),
        "the orphaned node must appear in removed_entities"
    );
}

#[test]
fn ac2_entity_removed_entirely_when_all_versions_excluded() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    assert!(
        view.get_node(fx.all_x_node).is_err(),
        "a node whose whole chain is excluded must be removed entirely"
    );
    assert!(
        view.report()
            .removed_entities()
            .contains(&EntityId::Node(fx.all_x_node)),
        "the removed node must appear in removed_entities"
    );
}

#[test]
fn ac2_surviving_update_applies_without_leaking_excluded_properties() {
    // chain_node: Y creates {a:y1}, X adds {secret:x-only}, Y patches {a:y2}.
    // Excluding X leaves a surviving Y chain. The node must still exist with
    // a == "y2", and must NOT carry X's `secret` property — a surviving update's
    // full-state snapshot would otherwise re-introduce the excluded source's
    // untouched fields (design doc §6, R8: the no-leak-back contract).
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    let node = view
        .get_node(fx.chain_node)
        .expect("chain node survives (it has surviving Y versions)");
    assert_eq!(
        node.get_property("a").and_then(|v| v.as_str()),
        Some("y2"),
        "Y's latest value must be present"
    );
    assert!(
        node.get_property("secret").is_none(),
        "excluded source X's property must not leak back into the view"
    );
}

// ---------------------------------------------------------------------------
// AC3 — fully queryable read-only: current-state, AS OF, history
// ---------------------------------------------------------------------------

#[test]
fn ac3_current_state_reads_reflect_exclusion() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    // Clean node is untouched; excluded nodes are gone.
    assert!(view.get_node(fx.clean_node).is_ok());
    assert!(view.get_node(fx.all_x_node).is_err());
}

#[test]
fn ac3_as_of_and_history_reads_reflect_exclusion() {
    let fx = seed_fixture();
    let now = time::now();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");

    // AS OF (bi-temporal) read against the view must behave as if X never wrote:
    // the all-X node never existed at any coordinate.
    assert!(
        view.get_node_at_time(fx.all_x_node, now, now).is_err(),
        "AS OF read must not resurrect an entirely-excluded node"
    );

    // History read: the surviving chain node's view history must contain no
    // version attributed to the excluded source X.
    let history = view
        .get_node_history(fx.chain_node)
        .expect("chain node has a view history");
    assert!(
        history
            .versions
            .iter()
            .all(|v| v.provenance.as_ref().and_then(|p| p.source()) != Some("X")),
        "no surviving version may be attributed to the excluded source"
    );
    // The clean node's history is preserved verbatim (same version count as real).
    let real_clean = fx.db.get_node_history(fx.clean_node).expect("real history");
    let view_clean = view.get_node_history(fx.clean_node).expect("view history");
    assert_eq!(view_clean.version_count(), real_clean.version_count());
}

// ---------------------------------------------------------------------------
// AC4 — real DB never mutated across view lifecycle
// ---------------------------------------------------------------------------

#[test]
fn ac4_real_db_structural_digest_unchanged_across_view_lifecycle() {
    let fx = seed_fixture();
    let before = structural_digest(&fx.db, &fx.all_nodes, &fx.all_edges);

    {
        // Create + query + drop a view. In an in-memory engine, WAL byte-identity
        // is approximated by identity of all observable state + full histories.
        let view = fx
            .db
            .counterfactual_replay(
                "lifecycle",
                ExclusionPredicate::source("X"),
                CounterfactualConfig::default(),
            )
            .expect("view materializes");
        let _ = view.get_node(fx.clean_node);
        let _ = view.get_node(fx.orphan_node);
        // `view` dropped here — its shadow storage must be reclaimed and the real
        // DB must be untouched.
    }

    let after = structural_digest(&fx.db, &fx.all_nodes, &fx.all_edges);
    assert_eq!(
        before, after,
        "creating, querying, and dropping a counterfactual view must not mutate the real DB"
    );
}

// ---------------------------------------------------------------------------
// AC5 — divergence report / blast radius
// ---------------------------------------------------------------------------

#[test]
fn ac5_divergence_report_counts_changed_and_removed() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    let report = view.report();

    // X wrote: orphan create, chain middle-update, all_x create + update, x_edge
    // create = 5 excluded writes.
    assert_eq!(report.excluded_writes(), 5, "all X writes excluded");
    // Entities removed entirely: orphan_node (orphaned Y-update), all_x_node
    // (whole chain excluded), and x_edge (create excluded) = exactly 3.
    assert_eq!(
        report.entities_removed(),
        3,
        "orphan_node + all_x_node + x_edge removed"
    );
    for removed in [
        EntityId::Node(fx.orphan_node),
        EntityId::Node(fx.all_x_node),
        EntityId::Edge(fx.x_edge),
    ] {
        assert!(
            report.removed_entities().contains(&removed),
            "{removed:?} must be in removed_entities"
        );
    }
    // Only the chain node's current state changes (secret dropped) but it still
    // exists — exactly one changed entity.
    assert_eq!(report.entities_changed(), 1, "only chain_node changed");
    assert!(
        report
            .changed_entities()
            .contains(&EntityId::Node(fx.chain_node)),
        "the chain node's current state differs and must be reported as changed"
    );
    // Untouched clean node/edge are neither changed nor removed.
    for clean in [EntityId::Node(fx.clean_node), EntityId::Edge(fx.clean_edge)] {
        assert!(!report.changed_entities().contains(&clean));
        assert!(!report.removed_entities().contains(&clean));
    }
}

#[test]
fn ac5_poisoned_feed_blast_radius_is_exactly_contaminated_set() {
    // Success metric: on the 1-of-5 poisoned-feed fixture the divergence report
    // identifies 100% of contaminated current-state facts with zero false
    // negatives, and flags no clean fact.
    let pf = seed_poisoned_feed();
    let view = pf
        .db
        .counterfactual_replay(
            "no-poison",
            ExclusionPredicate::source("poison"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    let report = view.report();

    let affected: std::collections::BTreeSet<EntityId> = report
        .changed_entities()
        .iter()
        .chain(report.removed_entities().iter())
        .copied()
        .collect();

    // Every contaminated fact is recovered (zero false negatives).
    for id in &pf.contaminated {
        assert!(
            affected.contains(&EntityId::Node(*id)),
            "contaminated fact {id} must be in the blast radius"
        );
    }
    // No clean fact is falsely implicated.
    for id in &pf.clean {
        assert!(
            !affected.contains(&EntityId::Node(*id)),
            "clean fact {id} must not appear in the blast radius"
        );
    }
    // The blast radius is exactly the contaminated set.
    assert_eq!(affected.len(), pf.contaminated.len());
}

// ---------------------------------------------------------------------------
// AC6 — determinism
// ---------------------------------------------------------------------------

#[test]
fn ac6_replay_is_deterministic_across_runs() {
    // Determinism property: the same deterministically-built history with the
    // same exclusion predicate yields identical reports and identical query
    // answers. Built by hand (no rng) so the history is fixed across the two
    // independent DBs; version ids differ between DBs, so we compare reported
    // quantities and property answers, never raw version ids.
    let make_view = || {
        let fx = seed_fixture();
        let view = fx
            .db
            .counterfactual_replay(
                "det",
                ExclusionPredicate::source("X"),
                CounterfactualConfig::default(),
            )
            .expect("view materializes");
        // Capture query answers for the surviving nodes (by construction the ids
        // are assigned in the same order across the two identical builds).
        let chain_a = view.get_node(fx.chain_node).ok().and_then(|n| {
            n.get_property("a")
                .and_then(|v| v.as_str())
                .map(String::from)
        });
        let clean_title = view.get_node(fx.clean_node).ok().and_then(|n| {
            n.get_property("title")
                .and_then(|v| v.as_str())
                .map(String::from)
        });
        (view.report().clone(), chain_a, clean_title)
    };

    let (report_a, chain_a1, clean_a1) = make_view();
    let (report_b, chain_a2, clean_a2) = make_view();

    assert!(
        reports_equivalent(&report_a, &report_b),
        "two replays of the same history must produce equivalent divergence reports"
    );
    assert_eq!(chain_a1, chain_a2, "query answers must be deterministic");
    assert_eq!(clean_a1, clean_a2, "query answers must be deterministic");
}

// ---------------------------------------------------------------------------
// AC7 — unattributed writes never excluded, counted as encountered
// ---------------------------------------------------------------------------

#[test]
fn ac7_unattributed_writes_never_excluded_and_counted() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    // The unattributed node must survive verbatim — a source predicate never
    // matches a write with no provenance.
    assert_eq!(
        view.get_node(fx.unattributed_node).ok().and_then(|n| n
            .get_property("title")
            .and_then(|v| v.as_str())
            .map(String::from)),
        Some("anon".to_string()),
        "unattributed writes are never excluded (AC7)"
    );
    assert!(
        view.report().unattributed_writes_encountered() >= 1,
        "the divergence report must count unattributed writes encountered"
    );
    // Sanity: excluding a source that only wrote unattributed data changes nothing.
    let none_view = fx
        .db
        .counterfactual_replay(
            "no-ghost",
            ExclusionPredicate::source("ghost-source-that-never-wrote"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    assert_eq!(none_view.report().excluded_writes(), 0);
}

// ---------------------------------------------------------------------------
// AC8 — guardrails: over-cap error + counterfactual labeling
// ---------------------------------------------------------------------------

#[test]
fn ac8_history_too_large_returns_structured_error() {
    let fx = seed_fixture();
    // A cap of zero versions is exceeded by any recorded history.
    let result = fx.db.counterfactual_replay(
        "capped",
        ExclusionPredicate::source("X"),
        CounterfactualConfig {
            max_replay_versions: 0,
        },
    );
    match result {
        Err(CounterfactualError::HistoryTooLarge { versions, cap }) => {
            assert_eq!(cap, 0, "the error must echo the configured cap");
            assert!(
                versions > 0,
                "the error must report the offending version count"
            );
        }
        Err(other) => panic!("expected HistoryTooLarge, got error {other:?}"),
        Ok(_) => panic!("expected HistoryTooLarge, got a materialized view"),
    }
}

#[test]
fn ac8_view_is_labeled_counterfactual() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "labelled",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    assert!(
        view.is_counterfactual(),
        "every counterfactual view must be labeled counterfactual (AC8)"
    );
    assert_eq!(
        view.handle().name(),
        "labelled",
        "the view name must surface in the handle so callers cannot mistake it for real state"
    );
}

// ---------------------------------------------------------------------------
// No-op — empty / no-match predicate yields a view equal to the real DB
// ---------------------------------------------------------------------------

#[test]
fn noop_no_match_predicate_yields_view_equal_to_real() {
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "noop",
            ExclusionPredicate::source("source-that-matches-nothing"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    let report = view.report();
    assert_eq!(report.excluded_writes(), 0, "no writes excluded");
    assert_eq!(report.entities_changed(), 0, "zero blast radius");
    assert_eq!(report.entities_removed(), 0, "zero blast radius");

    // Every node's FULL current-state property map reads identically to the real
    // DB (not just one key — m6).
    for &id in &fx.all_nodes {
        let real = fx.db.get_node(id).ok().map(|n| n.properties.clone());
        let via_view = view.get_node(id).ok().map(|n| n.properties.clone());
        assert_eq!(
            real, via_view,
            "no-op view must equal the real DB for node {id}"
        );
    }
    // Every edge's current state reads identically too.
    for &id in &fx.all_edges {
        let real = fx.db.get_edge(id).ok().map(|e| e.properties.clone());
        let via_view = view.get_edge(id).ok().map(|e| e.properties.clone());
        assert_eq!(
            real, via_view,
            "no-op view must equal the real DB for edge {id}"
        );
    }
}

// ---------------------------------------------------------------------------
// AC3 — traversal over the view honours exclusion
// ---------------------------------------------------------------------------

#[test]
fn ac3_traversal_over_view_excludes_removed_edges() {
    // clean_node --(x_edge:X)--> orphan_node   (both edge and target excluded)
    // clean_node --(clean_edge:clean-2)--> chain_node  (survives)
    let fx = seed_fixture();
    let view = fx
        .db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");

    let outgoing: Vec<EdgeId> = view
        .get_outgoing_edges(fx.clean_node)
        .iter()
        .map(|e| e.id)
        .collect();
    assert!(
        outgoing.contains(&fx.clean_edge),
        "surviving clean edge must be traversable"
    );
    assert!(
        !outgoing.contains(&fx.x_edge),
        "excluded X edge must be gone from traversal"
    );

    let incoming_chain: Vec<EdgeId> = view
        .get_incoming_edges(fx.chain_node)
        .iter()
        .map(|e| e.id)
        .collect();
    assert!(incoming_chain.contains(&fx.clean_edge));

    // orphan_node was removed entirely; the only edge into it (x_edge) is excluded.
    assert!(
        view.get_incoming_edges(fx.orphan_node).is_empty(),
        "no edges may point at an entirely-removed node in the view"
    );
}

// ---------------------------------------------------------------------------
// m1 — equal-value re-assertion by a surviving source (documented false-negative)
// ---------------------------------------------------------------------------

#[test]
fn equal_value_reassertion_by_surviving_source_is_dropped() {
    // Y create {a:y1}; X update adds {secret:s}; Y update re-asserts {secret:s}
    // (the SAME value X set). Excluding X, Y's re-assertion diffs EMPTY against its
    // real predecessor (X's version, value equal), so `secret` is dropped from the
    // view. This is the safe *no-leak* direction and a documented known limitation
    // (full-state-diff reconstruction cannot recover PATCH intent). Pinned here so
    // the direction can never silently regress into a *leak*.
    let db = AletheiaDB::new().expect("db");
    let id = create_node_by(&db, "Doc", props("a", "y1"), "Y");
    update_node_by(&db, id, props("secret", "s"), "X");
    update_node_by(&db, id, props("secret", "s"), "Y"); // Y re-asserts the same value

    let view = db
        .counterfactual_replay(
            "no-x",
            ExclusionPredicate::source("X"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    let node = view.get_node(id).expect("Y chain survives");
    assert_eq!(node.get_property("a").and_then(|v| v.as_str()), Some("y1"));
    assert!(
        node.get_property("secret").is_none(),
        "shared-value re-assertion is dropped (documented false-negative, never a leak)"
    );
}

// ---------------------------------------------------------------------------
// M1 — current-state reads match the real DB even for future-dated valid times
// ---------------------------------------------------------------------------

#[test]
fn noop_future_valid_time_view_equals_real_current_state() {
    // A no-op (exclude nothing) view must equal the real DB's current state even
    // when #3221 future-dated valid times are in play — the view reads through a
    // shadow CurrentStorage with the same current-state-index semantics, not a
    // bi-temporal AS-OF-now read.
    let db = AletheiaDB::new().expect("db");
    let now = time::now();
    let future = Timestamp::new(now.wallclock() + 5_000_000_000, 0).expect("future ts");

    // (A) Future valid_from create: present in the real current-state index.
    let future_node = db
        .create_node_with_options(
            "Doc",
            props("k", "future"),
            WriteRequestOptions::new()
                .with_provenance(prov("Y"))
                .with_valid_from(future),
        )
        .expect("future create");
    // (B) Future valid_to retraction: absent from the real current-state index.
    let retracted = create_node_by(&db, "Doc", props("k", "present"), "Y");
    db.retract_node(retracted, future).expect("retract");
    // (C) Present-tense control.
    let plain = create_node_by(&db, "Doc", props("k", "plain"), "Y");

    let view = db
        .counterfactual_replay(
            "noop",
            ExclusionPredicate::source("source-that-matches-nothing"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");

    // A no-op view has zero blast radius even with future-dated facts (report is
    // consistent with the read surface).
    assert_eq!(view.report().excluded_writes(), 0);
    assert_eq!(view.report().entities_changed(), 0);
    assert_eq!(view.report().entities_removed(), 0);

    for id in [future_node, retracted, plain] {
        let real = db.get_node(id).ok().map(|n| n.properties.clone());
        let via = view.get_node(id).ok().map(|n| n.properties.clone());
        assert_eq!(
            real, via,
            "no-op view current-state must equal the real DB for node {id}"
        );
    }
    assert!(
        view.get_node(future_node).is_ok(),
        "future valid_from node is present (matches the real current-state index)"
    );
    assert!(
        view.get_node(retracted).is_err(),
        "future valid_to retracted node is absent (matches the real current-state index)"
    );
}
