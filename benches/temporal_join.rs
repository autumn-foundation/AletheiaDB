//! Benchmark for AQL temporal joins (Issue #3379).
//!
//! Success metric: an event-aligned join over a range containing ~100 alignment
//! instants across 2 entities completes in **< 50ms** end-to-end (each
//! alignment point is bounded by the < 10ms temporal-reconstruction target; the
//! engine amortizes history reconstruction across instants, beating 100 × naive
//! `AS OF` round-trips). This bench builds a driver (Order) with ~100
//! valid-time versions joined via an edge to a Customer with its own tier
//! history, and times the event-aligned join end-to-end through `execute_aql`.
//! The numeric target is validated on the performance rig, not gated here.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::temporal::Timestamp;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Microseconds since epoch for 2023-01-01T00:00:00Z (a fixed, past anchor).
const YEAR_START_MICROS: i64 = 1_672_531_200_000_000;
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Build a driver Order with `events` valid-time versions, joined to a Customer
/// (with a handful of tier changes) via a PLACED_BY edge valid all year.
fn build_scenario(events: usize) -> AletheiaDB {
    let db = AletheiaDB::new().expect("db init");

    // Customer with 12 monthly tier changes.
    let customer = db
        .create_node_with_options(
            "Customer",
            PropertyMapBuilder::new().insert("tier", 1i64).build(),
            WriteRequestOptions::new().with_valid_from(Timestamp::from(YEAR_START_MICROS)),
        )
        .expect("create customer");
    for m in 1..12 {
        let vf = YEAR_START_MICROS + 30 * MICROS_PER_DAY * m as i64;
        db.update_node_with_options(
            customer,
            PropertyMapBuilder::new()
                .insert("tier", 1i64 + m as i64)
                .build(),
            WriteRequestOptions::new().with_valid_from(Timestamp::from(vf)),
        )
        .expect("update tier");
    }

    // Driver Order with `events` valid-time versions spread across ~360 days.
    let order = db
        .create_node_with_options(
            "Purchase",
            PropertyMapBuilder::new().insert("total", 10i64).build(),
            WriteRequestOptions::new().with_valid_from(Timestamp::from(YEAR_START_MICROS)),
        )
        .expect("create order");
    let step = (360 * MICROS_PER_DAY) / events.max(1) as i64;
    for i in 1..events {
        let vf = YEAR_START_MICROS + step * i as i64;
        db.update_node_with_options(
            order,
            PropertyMapBuilder::new()
                .insert("total", 10i64 + i as i64)
                .build(),
            WriteRequestOptions::new().with_valid_from(Timestamp::from(vf)),
        )
        .expect("update order");
    }

    db.create_edge_with_options(
        order,
        customer,
        "PLACED_BY",
        PropertyMapBuilder::new().build(),
        WriteRequestOptions::new().with_valid_from(Timestamp::from(YEAR_START_MICROS)),
    )
    .expect("create edge");

    db
}

fn bench_event_join_100_instants(c: &mut Criterion) {
    let db = build_scenario(100);
    let query = "MATCH (o:Purchase)-[:PLACED_BY]->(c:Customer) \
         ALIGN EVENTS DRIVER o \
         OVER VALID_TIME FROM '2023-01-01T00:00:00Z' TO '2024-01-01T00:00:00Z' \
         RETURN o.total, c.tier AS tier";

    c.bench_function("temporal_join/event_align_100_instants_2_entities", |b| {
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

criterion_group!(benches, bench_event_join_100_instants);
criterion_main!(benches);
