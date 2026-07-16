//! Benchmarks for the push changefeed subscription broadcast (Issue #3375).
//!
//! Two success metrics from the acceptance criteria:
//!
//! 1. **Idle-subscriber overhead**: commit throughput with 100 idle subscribers attached
//!    must be within ~5% of the zero-subscriber baseline (the broadcast must not
//!    meaningfully tax the write path).
//! 2. **Emit cost per commit**: the per-commit broadcast (record build + fan-out) is
//!    small and bounded.
//!
//! Compare the `commit/no_subscribers` and `commit/100_idle_subscribers` lines: their ratio
//! is the throughput cost of the feature under a heavy idle fan-out.

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::changefeed_subscription::ChangefeedConfig;
use aletheiadb::{AletheiaDB, ChangeFilter, Error, PropertyMap, PropertyMapBuilder, Subscription};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn props(i: usize) -> PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", format!("Node_{i}").as_str())
        .insert("value", i as i64)
        .build()
}

fn commit_one(db: &AletheiaDB, i: usize) {
    db.write(|tx| {
        tx.create_node("Person", props(i))?;
        Ok::<_, Error>(())
    })
    .unwrap();
}

fn bench_commit_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("changefeed_subscribe");

    // Baseline: no subscribers attached (broadcast fast-path is a single atomic load).
    group.bench_function("commit/no_subscribers", |b| {
        let db = AletheiaDB::new().unwrap();
        let mut i = 0usize;
        b.iter(|| {
            commit_one(&db, black_box(i));
            i += 1;
        });
    });

    // 100 idle subscribers (match-all, never drained). Buffers saturate then go Lagged, so
    // emit stays cheap; this measures the steady-state fan-out overhead on the writer.
    group.bench_function("commit/100_idle_subscribers", |b| {
        let db = AletheiaDB::new().unwrap();
        db.set_changefeed_config(ChangefeedConfig {
            max_subscriptions: 256,
            buffer_capacity: 1024,
        });
        let _subs: Vec<Subscription> = (0..100)
            .map(|_| db.subscribe_changes(ChangeFilter::all()).unwrap())
            .collect();
        let mut i = 0usize;
        b.iter(|| {
            commit_one(&db, black_box(i));
            i += 1;
        });
    });

    // 100 idle subscribers that keep draining, so their buffers never saturate (worst case
    // for per-commit fan-out cost: every commit clones + pushes to all 100 live buffers).
    group.bench_function("commit/100_draining_subscribers", |b| {
        let db = AletheiaDB::new().unwrap();
        db.set_changefeed_config(ChangefeedConfig {
            max_subscriptions: 256,
            buffer_capacity: 1024,
        });
        let subs: Vec<Subscription> = (0..100)
            .map(|_| db.subscribe_changes(ChangeFilter::all()).unwrap())
            .collect();
        let mut i = 0usize;
        b.iter(|| {
            commit_one(&db, black_box(i));
            i += 1;
            if i.is_multiple_of(500) {
                for s in &subs {
                    black_box(s.poll());
                }
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_commit_throughput);
criterion_main!(benches);
