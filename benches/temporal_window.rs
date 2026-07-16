//! Benchmark for AQL temporal aggregation windows (Issue #3363).
//!
//! Success metric: a 12-window (monthly, 1 year) aggregation over an entity
//! with <=1,000 historical versions completes in **< 50ms** end-to-end. This
//! bench builds a single Product with ~1,000 monotonic valid-time versions and
//! times a 12-window monthly `AVG`/`MIN`/`MAX`/`CHANGES` query end-to-end
//! through `execute_aql`. The numeric target is validated on the performance
//! rig, not gated here; this keeps a cheap, representative sample.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::temporal::Timestamp;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Microseconds since epoch for 2023-01-01T00:00:00Z (a fixed, past anchor).
const YEAR_START_MICROS: i64 = 1_672_531_200_000_000;
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Build a Product whose price is updated `versions` times at successive
/// valid-time instants, roughly one every few hours across ~2023.
fn build_versioned_product(versions: usize) -> AletheiaDB {
    let db = AletheiaDB::new().expect("db init");
    let id = db
        .create_node_with_options(
            "Product",
            PropertyMapBuilder::new().insert("price", 100i64).build(),
            WriteRequestOptions::new().with_valid_from(Timestamp::from(YEAR_START_MICROS)),
        )
        .expect("create");
    // Spread `versions` updates over ~360 days so all land inside the query range.
    let step = (360 * MICROS_PER_DAY) / versions.max(1) as i64;
    for i in 1..versions {
        let vf = YEAR_START_MICROS + step * i as i64;
        db.update_node_with_options(
            id,
            PropertyMapBuilder::new()
                .insert("price", 100i64 + i as i64)
                .build(),
            WriteRequestOptions::new().with_valid_from(Timestamp::from(vf)),
        )
        .expect("update");
    }
    db
}

fn bench_monthly_window_1000_versions(c: &mut Criterion) {
    let db = build_versioned_product(1_000);
    let query = "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2023-01-01T00:00:00Z' TO '2024-01-01T00:00:00Z' \
         RETURN AVG(p.price) AS avg_price, MIN(p.price) AS lo, MAX(p.price) AS hi, \
                CHANGES(p.price) AS changes";

    c.bench_function("temporal_window/monthly_1y_1000_versions", |b| {
        b.iter(|| {
            let results = db
                .execute_aql(black_box(query))
                .expect("query")
                .collect_all()
                .expect("collect");
            black_box(results.len())
        });
    });
}

criterion_group!(benches, bench_monthly_window_1000_versions);
criterion_main!(benches);
