//! Fast-path benchmark for the public Rust query-builder resource limits
//! (Issue #3368, AC6 — "fast queries are not taxed").
//!
//! This benchmark is the proof artifact that installing the engine-lane
//! resource guard (`EngineQueryLimitsConfig::default()`, the config every
//! `AletheiaDB::new()`/`open()` database runs under) does not meaningfully
//! tax an ordinary, well-under-limit query. It is deliberately structured as
//! head-to-head comparisons on the *identical* query, run once against a
//! database with the guard installed (`default()`, generous 30s / 1M-row
//! ceilings — nothing in these benchmarks gets anywhere close to either) and
//! once against a database with the guard compiled out entirely
//! (`disabled()`, per [`QueryResourceLimits::is_unlimited`] the executor
//! never constructs a `ResourceGuardIterator` at all). The
//! `guard_enabled_default` vs `guard_disabled` delta on `single_hop` is the
//! AC6 evidence: it isolates the per-call cost of checking a wall-clock
//! deadline once and accumulating a row/byte counter, with no other variable
//! changed. `scan/*` repeats the comparison over a traversal that pulls many
//! rows, to also show the *amortized per-row* cost of the guard's
//! per-pull `Instant::now()` + counter-increment path stays below the noise
//! floor across a longer drain.
//!
//! Run: `cargo bench --bench query_resource_limits`
//! Quick smoke: `cargo bench --bench query_resource_limits -- \
//!   --warm-up-time 1 --measurement-time 2 --sample-size 10`

use std::sync::Arc;

use aletheiadb::query::limits::EngineQueryLimitsConfig;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, NodeId, PropertyMapBuilder};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

mod common;

/// Build a database under the given engine query-limits config, seeded with a
/// star graph: one hub with `fan_out` outgoing `KNOWS` edges, reused across
/// benchmark iterations (seeding happens once, outside the timed closure).
fn seeded_db(limits: EngineQueryLimitsConfig, fan_out: usize) -> (Arc<AletheiaDB>, NodeId) {
    let config = AletheiaDBConfig::builder().query_limits(limits).build();
    let db = Arc::new(AletheiaDB::with_unified_config(config).expect("create db"));
    let hub = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Hub").build(),
        )
        .expect("create hub");
    for i in 0..fan_out {
        let leaf = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("Leaf{i}"))
                    .build(),
            )
            .expect("create leaf");
        db.create_edge(hub, leaf, "KNOWS", PropertyMapBuilder::new().build())
            .expect("create edge");
    }
    (db, hub)
}

/// `single_hop/guard_disabled` vs `single_hop/guard_enabled_default`: one
/// 1-hop traversal off a hub with a modest fan-out, comparing a
/// guard-installed database to a guard-free one on the identical query.
fn bench_single_hop(c: &mut Criterion) {
    let (disabled_db, disabled_hub) = seeded_db(EngineQueryLimitsConfig::disabled(), 20);
    let (enabled_db, enabled_hub) = seeded_db(EngineQueryLimitsConfig::default(), 20);

    let mut group = c.benchmark_group("query_resource_limits/single_hop");
    group.bench_function("guard_disabled", |b| {
        b.iter(|| {
            let rows = disabled_db
                .query()
                .start(black_box(disabled_hub))
                .traverse("KNOWS")
                .execute(&disabled_db)
                .expect("execute")
                .collect_all()
                .expect("collect");
            black_box(rows)
        });
    });
    group.bench_function("guard_enabled_default", |b| {
        b.iter(|| {
            let rows = enabled_db
                .query()
                .start(black_box(enabled_hub))
                .traverse("KNOWS")
                .execute(&enabled_db)
                .expect("execute")
                .collect_all()
                .expect("collect");
            black_box(rows)
        });
    });
    group.finish();
}

/// `scan/guard_disabled` vs `scan/guard_enabled_default`: a wider traversal
/// (larger fan-out) so many rows are pulled through the guard per query,
/// exercising the amortized per-row overhead of the guard's per-pull check.
fn bench_scan(c: &mut Criterion) {
    let (disabled_db, disabled_hub) = seeded_db(EngineQueryLimitsConfig::disabled(), 2_000);
    let (enabled_db, enabled_hub) = seeded_db(EngineQueryLimitsConfig::default(), 2_000);

    let mut group = c.benchmark_group("query_resource_limits/scan");
    group.sample_size(20);
    group.bench_function("guard_disabled", |b| {
        b.iter(|| {
            let rows = disabled_db
                .query()
                .start(black_box(disabled_hub))
                .traverse("KNOWS")
                .execute(&disabled_db)
                .expect("execute")
                .collect_all()
                .expect("collect");
            black_box(rows)
        });
    });
    group.bench_function("guard_enabled_default", |b| {
        b.iter(|| {
            let rows = enabled_db
                .query()
                .start(black_box(enabled_hub))
                .traverse("KNOWS")
                .execute(&enabled_db)
                .expect("execute")
                .collect_all()
                .expect("collect");
            black_box(rows)
        });
    });
    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_single_hop, bench_scan
);
criterion_main!(benches);
