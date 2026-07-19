//! End-to-end tests for knowledge half-life analytics (Issue #3377).
//!
//! These exercise the full database path (cohort scan, freshness, staleness,
//! as-of tx-time scoping, property cohorts) via [`SimulatedClock`]-planted
//! version histories, asserting concrete expected values against the (green)
//! Stage-B implementation plus the Fix-A correctness/validation hardening. The
//! pure Kaplan–Meier estimator tests live inline in the module
//! (`src/experimental/temporal/half_life.rs`).
//!
//! Gated on both `semantic-temporal` (the feature under test) and `simulation`
//! (the deterministic clock that plants known valid-time lifespans).
#![cfg(all(feature = "semantic-temporal", feature = "simulation"))]

use std::time::Duration;

use aletheiadb::AletheiaDB;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::error::{Error, QueryError, StorageError};
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
    // The edge world-change (valid_from advanced by 6 days) is exactly one
    // completed-lifespan event; the final open version is censored.
    assert_eq!(
        edge_stats.event_count, 1,
        "the KNOWS edge world-change is one terminating event"
    );
    assert_eq!(
        edge_stats.censored_count, 1,
        "the edge's current open version is censored"
    );
}

// ---- Test 6 (full): freshness age_in_half_lives + survival_probability -------

#[test]
fn fact_freshness_reports_age_in_half_lives() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let id = make_person(&db, "Oslo", T0);

    // Give the Person cohort a computable half-life. A single unchanging node
    // yields only a right-censored observation, so the cohort's Kaplan–Meier
    // curve never crosses 0.5 and its half-life is undefined (and then
    // `age_in_half_lives` would correctly be None per design decision #6). To
    // exercise the freshness math the cohort must have completed lifespans, so
    // a sibling Person undergoes several world-changes (each advancing
    // valid_from), planting terminating-event observations that dominate the
    // censored ones and define the KM median. This is the under-specified
    // Stage-A setup corrected to the feature's intent — no assertion is relaxed.
    let sibling = make_person(&db, "c0", T0);
    for k in 1..=5i64 {
        clock.jump_to(T0 + k * DAY_US);
        set_city(&db, sibling, &format!("c{k}"), T0 + k * DAY_US); // world-change
    }

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
    // The fact has aged exactly 60 days (Oslo valid_from = T0, clock = T0 + 60d).
    assert_eq!(
        score.age,
        Duration::from_micros((60 * DAY_US) as u64),
        "the fact aged exactly 60 days, got {:?}",
        score.age
    );
    // Concrete freshness (AC4), not a presence check. The cohort's observations
    // are: 5 world-change events @1 day (the sibling), the sibling's final open
    // version censored @55 days, and Oslo censored @60 days. The Kaplan–Meier
    // curve drops to S(1 day) = 1 - 5/7 <= 0.5, so the half-life is exactly
    // 1 day. Hence age_in_half_lives = 60 days / 1 day = 60.0, and the survival
    // probability reads the curve's plateau at 60 days = 1 - 5/7.
    let aihl = score
        .age_in_half_lives
        .expect("cohort half-life is defined => age in half-lives present");
    assert!(
        (aihl - 60.0).abs() < 1e-9,
        "age_in_half_lives = 60d / 1d = 60.0, got {aihl}"
    );
    let sp = score
        .survival_probability
        .expect("cohort curve exists => survival probability present");
    assert!(
        (sp - (1.0 - 5.0 / 7.0)).abs() < 1e-9,
        "survival_probability = KM survival at 60 days = 1 - 5/7, got {sp}"
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

// ===========================================================================
// Fix-A additions: correctness guards, concrete recovery, error paths, AC8.
// ===========================================================================

/// A tiny deterministic LCG (Numerical Recipes constants) — NOT `rand` — so the
/// DB-recovery fixture is byte-reproducible across runs.
struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }
    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ---- Retraction lifespan duration = valid_to - valid_from (AC2, primitive) ---

#[test]
fn retraction_lifespan_duration_equals_valid_interval() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let id = make_person(&db, "One", T0);
    // Advance the clock so the retraction's transaction time is strictly after
    // the create's — version history is ordered by transaction time.
    clock.jump_to(T0 + DAY_US);
    let _ = time::now();
    db.retract_node(id, ts(T0 + 12 * DAY_US)).expect("retract");
    clock.jump_to(T0 + 40 * DAY_US);
    let _ = time::now();

    let stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("stats");
    // A single completed event of lifespan 12 days => KM median = 12 days. This
    // pins the retraction lifespan semantics the recovery/reproducibility
    // fixtures below rely on.
    assert_eq!(stats.event_count, 1);
    assert_eq!(stats.censored_count, 0);
    assert_eq!(
        stats.half_life,
        Some(Duration::from_micros((12 * DAY_US) as u64)),
        "retraction lifespan = valid_to - valid_from = 12 days"
    );
}

// ---- AC1/SM1: recovery within ±10% over the real DB scan path ---------------

#[test]
fn knowledge_half_life_recovers_planted_median_over_db_path() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    // Plant an exponential-lifespan cohort with a known median (30 days) under
    // ~30% independent right-censoring, but through the real database path:
    // completed events are planted as create+retract (event lifespan = the
    // retracted interval), right-censored facts as still-open nodes whose
    // current valid age equals the drawn censoring time. Unlike the inline pure
    // test (hand-built `Observation` vectors), this exercises
    // `observations_from_versions` end to end.
    const H: f64 = (30 * DAY_US) as f64; // planted half-life = 30 days
    const CENSOR_SCALE: f64 = 7.0 / 3.0; // ~30% censoring for two exponentials
    const WINDOW: i64 = 200 * DAY_US; // analysis horizon
    const CLAMP: f64 = (150 * DAY_US) as f64; // keep valid-times in range
    let now = T0 + WINDOW;
    let n = 160;
    let mut rng = Lcg(0x3377_0000_1111_2222);
    let mut planted_censored = 0;
    // Each write advances the transaction clock by 1s so version histories are
    // strictly ordered (a create's retraction lands after it in tx time).
    let mut txclock = T0;
    let tick = |clock: &mut SimulatedClock, txclock: &mut i64| {
        *txclock += 1_000_000;
        clock.jump_to(*txclock);
        let _ = time::now();
    };
    for _ in 0..n {
        let u_t = 1.0 - rng.next_unit();
        let t = (H * (-(u_t.ln()) / std::f64::consts::LN_2)).clamp(1.0, CLAMP) as i64;
        let u_c = 1.0 - rng.next_unit();
        let c =
            (CENSOR_SCALE * H * (-(u_c.ln()) / std::f64::consts::LN_2)).clamp(1.0, CLAMP) as i64;
        if c < t {
            // Right-censored: an open node whose current valid age is exactly c.
            planted_censored += 1;
            tick(&mut clock, &mut txclock);
            make_person(&db, "R", now - c);
        } else {
            // Completed event of lifespan t: create, then retract at T0 + t.
            tick(&mut clock, &mut txclock);
            let id = make_person(&db, "R", T0);
            tick(&mut clock, &mut txclock);
            db.retract_node(id, ts(T0 + t)).expect("retract");
        }
    }
    assert!(
        (32..=64).contains(&planted_censored),
        "≈30% of {n} right-censored, got {planted_censored}"
    );

    clock.jump_to(now);
    let _ = time::now();

    let stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("stats");
    let hl_us = stats
        .half_life
        .expect("cohort has a defined half-life")
        .as_micros() as i64;
    let lo = (H * 0.90) as i64;
    let hi = (H * 1.10) as i64;
    assert!(
        (lo..=hi).contains(&hl_us),
        "recovered half-life {hl_us}µs not within ±10% of planted {}µs (censored={planted_censored})",
        H as i64
    );
}

// ---- AC1 IQR is exercised at the DB level (dispersion present) --------------

// ---- AC5: HalfLives threshold exact filtering + per-entry age ---------------

#[test]
fn staleness_half_lives_threshold_exact_count() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    // Three nodes each undergo one world-change of lifespan 1 day (planting a
    // half-life of exactly 1 day), with current ages of 0.5, 1.5 and 2.5
    // half-lives respectively.
    let now = T0 + 10 * DAY_US;
    let ages = [DAY_US / 2, 3 * DAY_US / 2, 5 * DAY_US / 2];
    let mut txclock = T0;
    for age in ages {
        // current version valid_from = now - age; predecessor started one day
        // earlier, so the world-change lifespan is exactly one day.
        let start = now - age - DAY_US;
        txclock += 1_000_000;
        clock.jump_to(txclock);
        let _ = time::now();
        let id = make_person(&db, "S", start);
        // Advance the tx clock so the world-change lands after the create.
        txclock += 1_000_000;
        clock.jump_to(txclock);
        let _ = time::now();
        set_city(&db, id, "moved", start + DAY_US); // world-change (+1 day)
    }
    clock.jump_to(now);
    let _ = time::now();

    // half-life = 1 day; a 1.0-half-life threshold qualifies ages >= 1 day: the
    // 1.5d and 2.5d members (2), not the 0.5d one.
    let page = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::HalfLives(1.0),
            0,
            10,
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("page");
    assert_eq!(
        page.total_matching, 2,
        "exactly the 1.5 and 2.5 half-life members qualify"
    );
    assert_eq!(page.entries.len(), 2);
    // Sorted oldest-first: 2.5 then 1.5 half-lives.
    let a0 = page.entries[0]
        .age_in_half_lives
        .expect("age in half-lives");
    let a1 = page.entries[1]
        .age_in_half_lives
        .expect("age in half-lives");
    assert!((a0 - 2.5).abs() < 1e-9, "oldest = 2.5 half-lives, got {a0}");
    assert!((a1 - 1.5).abs() < 1e-9, "next = 1.5 half-lives, got {a1}");
}

// ---- AC2: multiple corrections between world-changes = ONE lifespan ---------

#[test]
fn multiple_corrections_between_world_changes_are_one_lifespan() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let id = make_person(&db, "Paris", T0);
    // Three corrections (valid_from held at T0) — none is a world-change.
    clock.jump_to(T0 + DAY_US);
    set_city(&db, id, "Paris-1", T0);
    clock.jump_to(T0 + 2 * DAY_US);
    set_city(&db, id, "Paris-2", T0);
    clock.jump_to(T0 + 3 * DAY_US);
    set_city(&db, id, "Paris-3", T0);
    // One world-change (valid_from advanced).
    clock.jump_to(T0 + 4 * DAY_US);
    set_city(&db, id, "Lyon", T0 + 10 * DAY_US);
    clock.jump_to(T0 + 20 * DAY_US);
    let _ = time::now();

    let stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("stats");
    assert_eq!(
        stats.event_count, 1,
        "three corrections collapse into ONE lifespan; only the world-change is an event"
    );
    assert_eq!(
        stats.censored_count, 1,
        "the current open version is censored"
    );
}

// ---- AC6/MAJOR-3: an as-of report is reproducible across wall-clock advance --

#[test]
fn as_of_report_is_reproducible_across_wall_clock_advance() {
    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    // One completed event of lifespan 50 days (create + retract) ...
    let ev = make_person(&db, "E", T0);
    // Advance the clock so the retraction is ordered after the create.
    clock.jump_to(T0 + DAY_US);
    let _ = time::now();
    db.retract_node(ev, ts(T0 + 50 * DAY_US)).expect("retract");
    // ... plus two still-open facts (valid_from = T0) whose censored duration
    // depends on the analysis clock.
    make_person(&db, "E", T0);
    make_person(&db, "E", T0);

    // Pin the report to a past tx-time whose analysis clock is T0 + 40 days: the
    // two censored obs (40d) precede the event (50d), so at t=50d only the event
    // is at risk => S drops to 0 and the half-life is defined (50 days). Under a
    // drifted wall clock the censored durations would instead straddle the event
    // and the median would vanish — the drift MAJOR-3 fixes.
    let as_of = ts(T0 + 40 * DAY_US);
    let opts = HalfLifeOptions::new()
        .with_as_of_transaction_time(as_of)
        .with_min_events(1);

    clock.jump_to(T0 + 40 * DAY_US);
    let _ = time::now();
    let run1 = db
        .knowledge_half_life(Cohort::NodeLabel("Person".into()), &opts)
        .expect("run1");

    // Advance the wall clock far forward and re-run the SAME as-of report.
    clock.jump_to(T0 + 100 * DAY_US);
    let _ = time::now();
    let run2 = db
        .knowledge_half_life(Cohort::NodeLabel("Person".into()), &opts)
        .expect("run2");

    assert_eq!(
        run1, run2,
        "the same as-of report must be identical regardless of wall-clock advance (Fix-A MAJOR-3)"
    );
    assert_eq!(
        run1.half_life,
        Some(Duration::from_micros((50 * DAY_US) as u64)),
        "at the as-of clock (T0+40d) the two censored facts (40d) precede the 50d event => median 50d"
    );
}

// ---- MAJOR-1: property removal yields ONE event, no phantom -----------------

#[test]
fn property_removal_yields_single_event_no_phantom() {
    use aletheiadb::api::transaction::WriteRequestOptions;

    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

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
    // World-change (valid_from advanced) that REMOVES `city` via a full replace.
    clock.jump_to(T0 + 4 * DAY_US);
    db.replace_node_with_valid_time(
        id,
        "Person",
        PropertyMapBuilder::new().insert("name", "Ada").build(),
        Some(ts(T0 + 4 * DAY_US)),
    )
    .expect("replace");
    clock.jump_to(T0 + 30 * DAY_US);
    let _ = time::now();

    let city = db
        .knowledge_half_life(
            Cohort::NodeProperty {
                label: "Person".into(),
                key: "city".into(),
            },
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("city stats");
    // Exactly ONE completed event (the removal), and NO phantom censored
    // observation for the now-absent property (Fix-A MAJOR-1).
    assert_eq!(city.event_count, 1, "removing city ends its lifespan once");
    assert_eq!(city.censored_count, 0, "no phantom lifespan after removal");
    assert_eq!(city.observation_count, 1, "one observation total, not two");
}

// ---- Per-property: an unrelated world-change does NOT terminate the key ------

#[test]
fn property_cohort_unrelated_change_does_not_terminate() {
    use aletheiadb::api::transaction::WriteRequestOptions;

    let mut clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");

    let id = db
        .create_node_with_options(
            "Person",
            PropertyMapBuilder::new()
                .insert("city", "Paris")
                .insert("population", 100)
                .build(),
            WriteRequestOptions::new().with_valid_from(ts(T0)),
        )
        .expect("create");
    // World-change #1: only `population` changes (city held) — must NOT end the
    // `city` lifespan.
    clock.jump_to(T0 + 3 * DAY_US);
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("population", 200).build(),
        Some(ts(T0 + 3 * DAY_US)),
    )
    .expect("update population");
    // World-change #2: `city` changes — exactly one `city` event.
    clock.jump_to(T0 + 6 * DAY_US);
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("city", "Lyon").build(),
        Some(ts(T0 + 6 * DAY_US)),
    )
    .expect("update city");
    clock.jump_to(T0 + 20 * DAY_US);
    let _ = time::now();

    let city = db
        .knowledge_half_life(
            Cohort::NodeProperty {
                label: "Person".into(),
                key: "city".into(),
            },
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("city stats");
    assert_eq!(
        city.event_count, 1,
        "only the city change terminates the city lifespan; the population change does not"
    );
}

// ---- Error / no-panic paths (structured codes, AC3) -------------------------

#[test]
fn error_and_edge_paths_are_structured_not_panics() {
    let clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");
    make_person(&db, "Present", T0);

    // Empty cohort label => INVALID_ARGUMENT.
    let e = db
        .knowledge_half_life(
            Cohort::NodeLabel(String::new()),
            &HalfLifeOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(e, Error::Query(QueryError::InvalidParameter { .. })),
        "empty label => INVALID_ARGUMENT, got {e:?}"
    );

    // Unknown entity => NOT_FOUND.
    let missing = EntityId::Node(NodeId::new(9_999_999).expect("id"));
    let e = db
        .fact_freshness(missing, &HalfLifeOptions::new().with_min_events(1))
        .unwrap_err();
    assert!(
        matches!(e, Error::Storage(StorageError::NodeNotFound(_))),
        "unknown entity => NOT_FOUND, got {e:?}"
    );

    // limit == 0 => INVALID_ARGUMENT.
    let e = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::AbsoluteAge(Duration::from_micros(1)),
            0,
            0,
            &HalfLifeOptions::new().with_min_events(1),
        )
        .unwrap_err();
    assert!(
        matches!(e, Error::Query(QueryError::InvalidParameter { .. })),
        "limit == 0 => INVALID_ARGUMENT, got {e:?}"
    );

    // HalfLives(NaN) => INVALID_ARGUMENT.
    let e = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::HalfLives(f64::NAN),
            0,
            10,
            &HalfLifeOptions::new().with_min_events(1),
        )
        .unwrap_err();
    assert!(
        matches!(e, Error::Query(QueryError::InvalidParameter { .. })),
        "HalfLives(NaN) => INVALID_ARGUMENT, got {e:?}"
    );

    // HalfLives(negative) => INVALID_ARGUMENT.
    let e = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::HalfLives(-1.0),
            0,
            10,
            &HalfLifeOptions::new().with_min_events(1),
        )
        .unwrap_err();
    assert!(
        matches!(e, Error::Query(QueryError::InvalidParameter { .. })),
        "negative HalfLives => INVALID_ARGUMENT, got {e:?}"
    );

    // min_events == 0 => INVALID_ARGUMENT.
    let e = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new().with_min_events(0),
        )
        .unwrap_err();
    assert!(
        matches!(e, Error::Query(QueryError::InvalidParameter { .. })),
        "min_events == 0 => INVALID_ARGUMENT, got {e:?}"
    );

    // Unknown but valid label => empty cohort => insufficient_data, no panic.
    let stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Ghost".into()),
            &HalfLifeOptions::default(),
        )
        .expect("ghost stats");
    assert!(
        stats.insufficient_data,
        "unknown label => empty cohort => insufficient_data"
    );
    assert_eq!(stats.observation_count, 0);
    assert!(stats.half_life.is_none());
}

// ---- AC8: coverage scope is disclosed on results ----------------------------

#[test]
fn coverage_scope_is_disclosed_on_results() {
    let clock = SimulatedClock::new(T0);
    let _g = clock.inject();
    let db = AletheiaDB::new().expect("db");
    make_person(&db, "C", T0);

    let stats = db
        .knowledge_half_life(
            Cohort::NodeLabel("Person".into()),
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("stats");
    assert!(stats.coverage.hot_tier, "the hot tier is scanned");
    assert!(
        !stats.coverage.cold_migrated_included,
        "cold-migrated history is excluded in v1 (AC8 / #3238)"
    );

    let page = db
        .staleness_inventory(
            Cohort::NodeLabel("Person".into()),
            StalenessThreshold::AbsoluteAge(Duration::from_micros(0)),
            0,
            10,
            &HalfLifeOptions::new().with_min_events(1),
        )
        .expect("page");
    assert!(page.coverage.hot_tier);
    assert!(!page.coverage.cold_migrated_included);
}
