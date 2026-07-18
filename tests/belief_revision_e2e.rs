#![cfg(feature = "semantic-temporal")]
//! End-to-end integration test for the belief-revision audit (Issue #3362,
//! review finding MED-2).
//!
//! The co-located `#[cfg(test)]` `fixture_20()` builds `VersionInfo`s by hand,
//! which proves the *classifier* but not that the five interval geometries are
//! actually **reachable through the real DB write path**. This test drives real
//! `create`/`update`/`retract` writes (with explicit `valid_time`s and
//! provenance sources) to produce a single entity whose version history hits all
//! five classes, then classifies it via `db.belief_revisions` and asserts each
//! revision's class — closing that synthetic-fixture gap.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::id::EntityId;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::experimental::temporal::belief_revision::{RevisionClass, RevisionOptions};

/// A minimal source-only provenance bundle.
fn prov(source: &str) -> Provenance {
    Provenance::builder()
        .source(source)
        .build()
        .expect("valid provenance")
}

#[test]
fn end_to_end_all_five_classes_through_real_writes() {
    let db = AletheiaDB::new().expect("db");

    // Anchor real-world valid times to a single wallclock base so the
    // correction (equal valid_from) vs world_change (advanced valid_from)
    // geometry is exact and deterministic.
    let base = time::now().wallclock();
    let vt_early = Timestamp::new(base - 10_000_000, 0).expect("early ts");
    let vt_late = Timestamp::new(base - 1_000_000, 0).expect("late ts");

    // v1: InitialAssertion — the first version.
    let id = db
        .create_node_with_options(
            "Person",
            PropertyMapBuilder::new().insert("city", "Paris").build(),
            WriteRequestOptions::new()
                .with_valid_from(vt_early)
                .with_provenance(prov("src-a")),
        )
        .expect("create (initial assertion)");

    // v2: Correction — value changed, SAME valid_from (equal => correction,
    // a later transaction-time rewrite of an already-recorded valid period).
    db.update_node_with_options(
        id,
        PropertyMapBuilder::new().insert("city", "Lyon").build(),
        WriteRequestOptions::new()
            .with_valid_from(vt_early)
            .with_provenance(prov("src-a")),
    )
    .expect("update (correction)");

    // v3: WorldChange — value changed AND valid_from advanced beyond all prior.
    db.update_node_with_options(
        id,
        PropertyMapBuilder::new().insert("city", "Nice").build(),
        WriteRequestOptions::new()
            .with_valid_from(vt_late)
            .with_provenance(prov("src-a")),
    )
    .expect("update (world change)");

    // v4: Reaffirmation — the SAME value ("Nice") re-asserted by a DIFFERENT
    // source (AC2's "different source" condition; MED-1). PATCH re-write of the
    // same value produces no value delta, so this is a reaffirmation rather than
    // a correction/world_change.
    db.update_node_with_options(
        id,
        PropertyMapBuilder::new().insert("city", "Nice").build(),
        WriteRequestOptions::new()
            .with_valid_from(vt_late)
            .with_provenance(prov("src-b")),
    )
    .expect("update (reaffirmation)");

    // v5: Retraction — close the valid-time interval (#3230). The node has no
    // edges, so no detach is required.
    db.retract_node(id, time::now()).expect("retract");

    let log = db
        .belief_revisions(EntityId::Node(id), &RevisionOptions::default())
        .expect("belief_revisions audit");

    let classes: Vec<RevisionClass> = log.revisions().iter().map(|r| r.class).collect();
    assert_eq!(
        classes,
        vec![
            RevisionClass::InitialAssertion,
            RevisionClass::Correction,
            RevisionClass::WorldChange,
            RevisionClass::Reaffirmation,
            RevisionClass::Retraction,
        ],
        "all five belief-revision classes must be reachable end-to-end through \
         real writes; got {classes:?}"
    );

    // The reaffirmation revision must surface the different source that
    // re-asserted the value (AC2 "who says so?").
    let reaff = &log.revisions()[3];
    assert_eq!(reaff.class, RevisionClass::Reaffirmation);
    assert_eq!(
        reaff.provenance.as_ref().and_then(Provenance::source),
        Some("src-b"),
        "reaffirmation surfaces the re-asserting source"
    );

    // Sanity: all five distinct categories present through the real path.
    use std::collections::HashSet;
    let seen: HashSet<_> = classes.iter().copied().collect();
    assert_eq!(seen.len(), 5, "all five categories exercised end-to-end");
}
