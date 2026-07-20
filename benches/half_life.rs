//! Knowledge half-life analytics performance bench (Issue #3377, Fix-B).
//!
//! SM3 success metrics (bench-reported, **not** CI-gated):
//!   - cohort `knowledge_half_life` scan: **< 10 s / 1M versions**;
//!   - warm `fact_freshness`: **< 1 ms** on a warm cache.
//!
//! Two measurements:
//!   (a) `knowledge_half_life_cohort` — a **cold** cohort scan + Kaplan–Meier
//!       over a sizeable synthetic cohort. Each iteration uses a distinct
//!       `as_of` transaction-time coordinate so it is a genuine cache **miss**
//!       (a full scan), never a warm hit — the number to extrapolate against the
//!       "< 10 s / 1M versions" budget (this cohort is `ENTITIES * VERSIONS`
//!       versions; scale linearly, keeping the documented O(k²)-in-versions-per-
//!       entity caveat in mind for pathologically deep single entities).
//!   (b) `fact_freshness_warm` — the warm-cache freshness path (no intervening
//!       write, so the cohort stats are served from the [`CohortStatsCache`]),
//!       the O(1)-in-cohort-size latency the "< 1 ms warm" target names.
//!
//! Gated behind `semantic-temporal` (the cohort the feature lives in); run with:
//! `cargo bench --no-default-features --features semantic-temporal --bench half_life`.

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::id::{EntityId, NodeId};
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::experimental::temporal::half_life::{Cohort, HalfLifeOptions};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

const DAY_US: i64 = 86_400 * 1_000_000;
/// Cohort width. Kept modest so the harness builds and runs within the disk /
/// time envelope; scale the reported per-scan time linearly to project the SM3
/// "< 10 s / 1M versions" budget (this cohort holds `ENTITIES * VERSIONS`
/// versions).
const ENTITIES: usize = 1_500;
/// Versions per entity (one initial assertion + `VERSIONS - 1` world-changes,
/// each a completed valid-time lifespan; the final version stays open =
/// censored).
const VERSIONS: usize = 4;

/// Build a `Doc` cohort of `ENTITIES` nodes, each undergoing `VERSIONS - 1`
/// backdated world-changes (advancing `valid_from`) so every member contributes
/// completed-lifespan events plus one right-censored open version. Returns the
/// first node id (an entry point for the freshness bench).
fn build_cohort(db: &AletheiaDB) -> NodeId {
    let base = time::now().wallclock();
    let mut first: Option<NodeId> = None;
    for e in 0..ENTITIES {
        let vf0 = Timestamp::new(base - 100 * DAY_US + e as i64, 0).unwrap();
        let id = db
            .create_node_with_options(
                "Doc",
                PropertyMapBuilder::new().insert("v", 0_i64).build(),
                WriteRequestOptions::new().with_valid_from(vf0),
            )
            .unwrap();
        if first.is_none() {
            first = Some(id);
        }
        for v in 1..VERSIONS {
            // World-change: advance valid_from by v days => a completed lifespan.
            let vf = Timestamp::new(base - 100 * DAY_US + e as i64 + v as i64 * DAY_US, 0).unwrap();
            db.update_node_with_options(
                id,
                PropertyMapBuilder::new().insert("v", v as i64).build(),
                WriteRequestOptions::new().with_valid_from(vf),
            )
            .unwrap();
        }
    }
    first.expect("at least one entity built")
}

fn bench_knowledge_half_life_cohort(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    let _ = build_cohort(&db);
    // A far-future base coordinate that is > every recorded tx-time, so each
    // distinct as_of still sees the whole cohort — but forces a fresh (cold)
    // scan every iteration rather than a warm cache hit.
    let base_future = time::now().wallclock() + 365 * DAY_US;
    let mut i: i64 = 0;

    c.bench_function("knowledge_half_life_cohort_1500x4_cold", |b| {
        b.iter(|| {
            i += 1;
            let opts = HalfLifeOptions::new()
                .with_as_of_transaction_time(Timestamp::new(base_future + i, 0).unwrap())
                .with_min_events(1);
            let stats = db
                .knowledge_half_life(black_box(Cohort::NodeLabel("Doc".into())), &opts)
                .unwrap();
            black_box(stats.observation_count);
        });
    });
}

fn bench_fact_freshness_warm(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    let first = build_cohort(&db);
    let entity = EntityId::Node(first);
    let opts = HalfLifeOptions::new().with_min_events(1);
    // Warm the shared cache once (no further writes => subsequent calls are warm
    // cache hits at the same write generation).
    let _ = db.fact_freshness(entity, &opts).unwrap();

    c.bench_function("fact_freshness_warm", |b| {
        b.iter(|| {
            let score = db.fact_freshness(black_box(entity), &opts).unwrap();
            black_box(score.age);
        });
    });
}

criterion_group!(
    benches,
    bench_knowledge_half_life_cohort,
    bench_fact_freshness_warm
);
criterion_main!(benches);
