//! End-to-end tests for contradiction genealogy (Issue #3352).
//!
//! Exercises the public `AletheiaDB::contradiction_genealogy` /
//! `find_contradictions` API through the real write path, gated behind the
//! `semantic-temporal` experimental cohort.
#![cfg(feature = "semantic-temporal")]

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::id::EntityId;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::experimental::temporal::contradiction_genealogy::{
    ContradictionScope, ContradictionTarget, DivergenceKind, GenealogyOptions,
};

fn backdated(secs: i64) -> Timestamp {
    let base = time::now().wallclock();
    Timestamp::new(base - secs * 1_000_000, 0).unwrap()
}

#[test]
fn e2e_backdated_correction_is_retroactive() {
    let db = AletheiaDB::new().unwrap();
    let vt_recent = backdated(500);
    let vt_old = backdated(1000);
    // Create asserting a recent valid_from, then backdate a different value to
    // an earlier valid_from that overlaps => retroactive correction.
    let id = db
        .create_node_with_options(
            "Company",
            PropertyMapBuilder::new().insert("ceo", "Alice").build(),
            WriteRequestOptions::new().with_valid_from(vt_recent),
        )
        .unwrap();
    db.update_node_with_options(
        id,
        PropertyMapBuilder::new().insert("ceo", "Bob").build(),
        WriteRequestOptions::new().with_valid_from(vt_old),
    )
    .unwrap();

    let g = db
        .contradiction_genealogy(
            ContradictionTarget::EntityProperty {
                entity: EntityId::Node(id),
                property: "ceo".to_string(),
            },
            &GenealogyOptions::default(),
        )
        .unwrap();
    assert!(g.has_contradiction());
    assert_eq!(g.pairs[0].kind, DivergenceKind::RetroactiveCorrection);
    assert!(g.divergence_point.is_some());
    assert!(g.narrative.contains("ceo"));
    assert!(!g.sources.is_empty());
}

#[test]
fn e2e_find_contradictions_scans_label() {
    let db = AletheiaDB::new().unwrap();
    let vt = backdated(1000);
    for i in 0..4 {
        let id = db
            .create_node_with_options(
                "Org",
                PropertyMapBuilder::new()
                    .insert("head", format!("A{i}"))
                    .build(),
                WriteRequestOptions::new().with_valid_from(vt),
            )
            .unwrap();
        if i % 2 == 0 {
            db.update_node_with_options(
                id,
                PropertyMapBuilder::new()
                    .insert("head", format!("B{i}"))
                    .build(),
                WriteRequestOptions::new().with_valid_from(vt),
            )
            .unwrap();
        }
    }
    let scan = db
        .find_contradictions(&ContradictionScope::new().with_label("Org"))
        .unwrap();
    assert_eq!(scan.contradictions.len(), 2, "i=0 and i=2 seeded");
    for c in &scan.contradictions {
        assert_eq!(c.property, "head");
    }
}

#[test]
fn e2e_clean_succession_is_not_a_contradiction() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Company",
            PropertyMapBuilder::new().insert("ceo", "Solo").build(),
        )
        .unwrap();
    let g = db
        .contradiction_genealogy(
            ContradictionTarget::EntityProperty {
                entity: EntityId::Node(id),
                property: "ceo".to_string(),
            },
            &GenealogyOptions::default(),
        )
        .unwrap();
    assert!(!g.has_contradiction());
    assert!(!g.narrative.is_empty());
}
