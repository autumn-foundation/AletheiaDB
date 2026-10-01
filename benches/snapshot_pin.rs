//! Benchmarks for named snapshots / reproducible reads (Issue #3370).
//!
//! Success metric: **snapshot-name resolution < 100µs p99**. Resolving a name
//! to a pinned coordinate is a single `RwLock` read + clone, so it should be
//! well under budget. We compare:
//!
//! - `resolve`: `db.snapshot(name)` — name -> borrowed handle.
//! - `resolve_and_read`: resolve + one pinned `get_node` at the coordinate.
//! - `raw_as_of_read`: the equivalent `get_node_at_time` with raw timestamps
//!   (the baseline the pin adds naming convenience on top of).
//!
//! These are intentionally cheap to sample so CI/local runs stay fast; the
//! numeric p99 target is validated on the performance rig, not gated here.

use aletheiadb::AletheiaDB;
use aletheiadb::core::PropertyMapBuilder;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_snapshot_pin(c: &mut Criterion) {
    let db = AletheiaDB::new().expect("db init");

    // Seed a small graph so the pinned reads resolve real versions.
    let mut node = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("seed node");
    for i in 0..64 {
        node = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("P{i}"))
                    .build(),
            )
            .expect("seed node");
    }

    let snap = db.create_snapshot("bench", None).expect("snapshot");
    let (vt, tt) = snap.coordinate();

    c.bench_function("snapshot_resolve", |b| {
        b.iter(|| {
            let handle = db.snapshot(black_box("bench")).expect("resolve");
            black_box(handle.coordinate());
        });
    });

    c.bench_function("snapshot_resolve_and_read", |b| {
        b.iter(|| {
            let handle = db.snapshot(black_box("bench")).expect("resolve");
            black_box(handle.get_node(black_box(node)).expect("pinned read"));
        });
    });

    c.bench_function("snapshot_raw_as_of_read", |b| {
        b.iter(|| {
            black_box(
                db.get_node_at_time(black_box(node), black_box(vt), black_box(tt))
                    .expect("as-of read"),
            );
        });
    });

    c.bench_function("snapshot_create", |b| {
        let mut n = 0u64;
        b.iter(|| {
            n += 1;
            black_box(
                db.create_snapshot(format!("bench-create-{n}"), None)
                    .expect("create"),
            );
        });
    });
}

criterion_group!(benches, bench_snapshot_pin);
criterion_main!(benches);
