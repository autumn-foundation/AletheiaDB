//! End-to-end RED tests for knowledge half-life analytics (Issue #3377, Stage A).
//!
//! These exercise the full database path (cohort scan, freshness, staleness,
//! as-of tx-time scoping, property cohorts) via [`SimulatedClock`]-planted
//! version histories. Every test asserts a real Stage-B expected value and
//! therefore **fails** against the `todo!()` skeleton — that is the point of the
//! red phase. The pure Kaplan–Meier estimator tests live inline in the module
//! (`src/experimental/temporal/half_life.rs`).
//!
//! Gated on both `semantic-temporal` (the feature under test) and `simulation`
//! (the deterministic clock that plants known valid-time lifespans).
#![cfg(all(feature = "semantic-temporal", feature = "simulation"))]

use std::time::Duration;

use aletheiadb::AletheiaDB;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::id::{EntityId, NodeId};
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::experimental::temporal::half_life::{Cohort, HalfLifeOptions, StalenessThreshold};
use aletheiadb::simulation::SimulatedClock;

const T0: i64 = 1_600_000_000_000_000; // a fixed epoch (µs) well within valid range
const DAY_US: i64 = 86_400 * 1_000_000;

fn ts(micros: i64) -> Timestamp {
    Timestamp::new(micros, 0).expect("valid timestamp")
}

fn props(city: &str) -> aletheiadb::core::property::PropertyMap {
    PropertyMapBuilder::new().insert("city", city).build()
}

/// Create a `Person` node with a `city` property valid from `valid_from`.
fn make_person(db: &AletheiaDB, city: &str, valid_from: i64) -> NodeId {
    use aletheiadb::api::transaction::WriteRequestOptions;
    db.create_node_with_options(
        "Person",
        props(city),
        WriteRequestOptions::new().with_valid_from(ts(valid_from)),
    )
    .expect("create person")
}

/// Update a node's `city`, advancing `valid_from` (a world-change) or holding it
/// (a correction), controlling the planted lifespan.
fn set_city(db: &AletheiaDB, id: NodeId, city: &str, valid_from: i64) {
    db.update_node_with_valid_time(id, props(city), Some(ts(valid_from)))
        .expect("update city");
}

// ---- Test 4: Correction does NOT terminate a lifespan; WorldChange does ------

#[test]
fn correction_does_not_count_as_end_of_life() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    // One person: an initial assertion, then a *correction* (same valid_from,
    // fixing the recorded value) — which must NOT create a completed lifespan —
    // then a *world-change* (valid_from advanced by 10 days) which does.
    let id = make_person(&db, "Paris", T0);
    clock.jump_to(T0 + DAY_US);
    set_city(&db, id, "Paris-fixed", T0); // correction: valid_from unchanged
    clock.jump_to(T0 + 2 * DAY_US);
    set_city(&db, id, "Lyon", T0 + 10 * DAY_US); // world-change: valid_from advanced

    let stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::default(),
        )
        .expect("stats");
    // Exactly ONE terminating event (the world-change); the correction is not
    // counted, and the final open version is a censored observation.
    assert_eq!(
        stats.event_count, 1,
        "only the world-change terminates a lifespan; the correction does not"
    );
    assert_eq!(
        stats.censored_count, 1,
        "the current open version is censored"
    );
}

// ---- Test 5: retraction (#3230) is an end-of-life event ----------------------

#[test]
fn retraction_is_end_of_life() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let id = make_person(&db, "Berlin", T0);
    clock.jump_to(T0 + 5 * DAY_US);
    db.retract_node(id, ts(T0 + 5 * DAY_US)).expect("retract");

    let stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::default(),
        )
        .expect("stats");
    assert_eq!(
        stats.event_count, 1,
        "a retraction closes the valid interval => one completed lifespan"
    );
    assert_eq!(
        stats.censored_count, 0,
        "nothing is left open after retraction"
    );
}

// ---- Test 8: as-of tx-time excludes later-recorded revisions (replayable) ----

#[test]
fn as_of_transaction_time_excludes_later_revisions() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let id = make_person(&db, "Rome", T0);
    // A world-change recorded much later in transaction time.
    let cutoff = ts(T0 + 3 * DAY_US);
    clock.jump_to(T0 + 10 * DAY_US);
    set_city(&db, id, "Milan", T0 + 20 * DAY_US);

    // As of a tx-time BEFORE the world-change was recorded: only the initial
    // (still-open) assertion is visible => zero completed events.
    let before = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new()
                .with_as_of_transaction_time(cutoff)
                .with_min_events(1),
        )
        .expect("stats before");
    assert_eq!(
        before.event_count, 0,
        "the later-recorded world-change is invisible as-of the earlier tx-time"
    );

    // As of now: the world-change is visible => one completed event.
    let now = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("stats now");
    assert_eq!(now.event_count, 1, "the world-change is visible as-of now");
}

// ---- Test 9: cohort scan cap => sampled flag ---------------------------------

#[test]
fn scan_cap_sets_sampled_flag() {
    let clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    for i in 0..10 {
        make_person(&db, "City", T0 + i * DAY_US);
    }
    // Cap the scan below the cohort size => sampled must be true.
    let capped = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new()
                .with_max_entities(3)
                .with_min_events(1),
        )
        .expect("capped stats");
    assert!(
        capped.sampled,
        "a cohort larger than the cap must flag sampled"
    );

    // A generous cap sees everything => not sampled.
    let full = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new()
                .with_max_entities(1000)
                .with_min_events(1),
        )
        .expect("full stats");
    assert!(
        !full.sampled,
        "a cap above the cohort size must not flag sampled"
    );
}

// ---- Test 10: property-cohort per-property event classification --------------

#[test]
fn property_cohort_classifies_per_property() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    // A node whose `city` changes once (world-change) but whose `name` never
    // changes. The `city` property cohort sees one event; a hypothetical `name`
    // cohort would see none.
    use aletheiadb::api::transaction::WriteRequestOptions;
    let id = db
        .create_node_with_options(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Ada")
                .insert("city", "Paris")
                .build(),
            WriteRequestOptions::new().with_valid_from(ts(T0)),
        )
        .expect("create");
    clock.jump_to(T0 + 4 * DAY_US);
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new()
            .insert("name", "Ada")
            .insert("city", "Lyon")
            .build(),
        Some(ts(T0 + 4 * DAY_US)),
    )
    .expect("update");

    let city_stats = db
        .knowledge_half_life(
            Cohort::NodeProperty {
                label: "Person".into(),
                key: "city".into(),
            },
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("city stats");
    assert_eq!(city_stats.event_count, 1, "city changed once => one event");

    let name_stats = db
        .knowledge_half_life(
            Cohort::NodeProperty {
                label: "Person".into(),
                key: "name".into(),
            },
            &HalfLifeOptions::default(),
        )
        .expect("name stats");
    assert_eq!(name_stats.event_count, 0, "name never changed => no events");
    assert!(
        name_stats.insufficient_data,
        "no events => insufficient_data"
    );
}

// ---- Test 14: node-label AND edge-type cohorts both work ---------------------

#[test]
fn node_label_and_edge_type_cohorts_both_work() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let a = make_person(&db, "A", T0);
    let b = make_person(&db, "B", T0);
    let e = db
        .create_edge(
            a,
            b,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020).build(),
        )
        .expect("edge");
    clock.jump_to(T0 + 6 * DAY_US);
    db.update_edge_with_valid_time(
        e,
        PropertyMapBuilder::new().insert("since", 2021).build(),
        Some(ts(T0 + 6 * DAY_US)),
    )
    .expect("update edge");

    let node_stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("node stats");
    assert!(
        node_stats.observation_count >= 2,
        "two Person nodes observed"
    );

    let edge_stats = db
        .knowledge_half_life(
            Cohort::EdgeType("KNOWS".into()),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("edge stats");
    assert!(
        edge_stats.observation_count >= 1,
        "the KNOWS edge is observed"
    );
    assert_eq!(edge_stats.cohort, Cohort::EdgeType("KNOWS".into()));
}

// ---- Test 6 (full): freshness age_in_half_lives + survival_probability -------

#[test]
fn fact_freshness_reports_age_in_half_lives() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let id = make_person(&db, "Oslo", T0);
    // Advance the clock 60 days so the fact's current age is 60 days.
    clock.jump_to(T0 + 60 * DAY_US);
    let _ = time::now();

    let score = db
        .fact_freshness(
            EntityId::Node(id),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("freshness");
    assert_eq!(score.entity, EntityId::Node(id));
    // The fact has aged ~60 days.
    assert!(
        score.age >= Duration::from_micros((59 * DAY_US) as u64),
        "the fact aged ~60 days, got {:?}",
        score.age
    );
    // With a computed cohort half-life, both derived fields are present.
    assert!(
        score.age_in_half_lives.is_some(),
        "age in half-lives is computable"
    );
    assert!(
        score.survival_probability.is_some(),
        "survival probability is computable"
    );
}

// ---- Test 7: staleness inventory threshold filter + pagination + sampled -----

#[test]
fn staleness_inventory_filters_paginates_and_flags_sampled() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    // Five people, each older than the last by 10 days: ages span 10..50 days.
    for i in 1..=5 {
        make_person(&db, "P", T0 - i * 10 * DAY_US);
    }
    clock.jump_to(T0 + 1);
    let _ = time::now();

    // Absolute threshold: facts older than 25 days => the 30/40/50-day-old ones (3).
    let page = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::AbsoluteAge(Duration::from_micros((25 * DAY_US) as u64)),
            0,
            2, // page size 2
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("staleness page");
    assert_eq!(page.entries.len(), 2, "page size 2");
    assert_eq!(page.total_matching, 3, "three facts exceed 25 days");
    assert_eq!(
        page.next_offset,
        Some(2),
        "more remain => next_offset advances"
    );
    // Entries sorted by descending age (oldest first).
    assert!(
        page.entries[0].age >= page.entries[1].age,
        "staleness entries are sorted oldest-first"
    );

    // Half-lives threshold also works (routes through the cohort half-life).
    let hl_page = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::HalfLives(1.0),
            0,
            10,
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("half-life staleness page");
    assert!(
        hl_page.total_matching <= 5,
        "half-life threshold yields a subset of the cohort"
    );

    // A cohort scan capped below the cohort size flags sampled.
    let capped = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::AbsoluteAge(Duration::from_micros(1)),
            0,
            10,
            &HalfLifeOptions::new()
                .with_max_entities(2)
                .with_min_events(1),
        )
        .expect("capped staleness page");
    assert!(capped.sampled, "a cap below cohort size flags sampled");
}
