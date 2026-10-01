//! Cost of append-only valid-time supersession on update (ADR-0061).
//!
//! An ordinary update now stores the new version plus a structural
//! carry-forward of the superseded valid-time prefix. These benches pin:
//!
//! - `update/plain`: one update of an entity with a short history (the hot
//!   write path: plan O(1), carry-forward append, new version append);
//! - `update/backfill`: a backfill between two existing versions (carry-forward,
//!   bounded new version, head re-assertion);
//! - `as_of/between_versions`: an as-of valid-time read landing on a
//!   carry-forward slice (previously `NodeNotFound`).

use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::{AletheiaDB, PropertyMap, PropertyMapBuilder};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

const HOUR: i64 = 3_600_000_000;

fn ago(now: i64, hours: i64) -> Timestamp {
    HybridTimestamp::new(now - hours * HOUR, 0).unwrap()
}

fn props(i: i64) -> PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("rev", i)
        .build()
}

/// A node created 10h ago and updated `updates` times at 1h steps.
fn node_with_history(db: &AletheiaDB, now: i64, updates: i64) -> NodeId {
    let id = db
        .create_node_with_valid_time("Person", props(0), Some(ago(now, 10)))
        .unwrap();
    for i in 1..=updates {
        db.update_node_with_valid_time(id, props(i), Some(ago(now, 10 - i)))
            .unwrap();
    }
    id
}

fn bench_updates(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let mut group = c.benchmark_group("update");

    group.bench_function("plain", |b| {
        b.iter_batched(
            || node_with_history(&db, now, 2),
            |id| {
                db.update_node_with_valid_time(id, props(99), None).unwrap();
                black_box(id)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("backfill", |b| {
        b.iter_batched(
            || node_with_history(&db, now, 4),
            |id| {
                // Lands between the versions at 8h and 7h ago.
                let t = HybridTimestamp::new(now - 7 * HOUR - HOUR / 2, 0).unwrap();
                db.update_node_with_valid_time(id, props(99), Some(t))
                    .unwrap();
                black_box(id)
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_as_of(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let id = node_with_history(&db, now, 8);
    let probe = HybridTimestamp::new(now - 5 * HOUR - HOUR / 2, 0).unwrap();

    let mut group = c.benchmark_group("as_of");
    group.bench_function("between_versions", |b| {
        b.iter(|| black_box(db.get_node_at_valid_time(id, probe).is_ok()));
    });
    group.finish();
}

criterion_group!(benches, bench_updates, bench_as_of);
criterion_main!(benches);
