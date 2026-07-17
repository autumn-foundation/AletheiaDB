//! Benchmarks for the graph-wide temporal changefeed (Issue #3216).
//!
//! Success metric: a changefeed query over a 24-hour transaction-time window returning
//! <=1,000 changed entities should complete in well under 10 ms (matching the published
//! temporal-reconstruction target).

mod common;

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::changefeed::ChangeFeedQuery;
use aletheiadb::core::temporal::{TIMESTAMP_MAX, Timestamp};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Populate a database with `count` nodes and a fraction of updates/deletes so the feed
/// contains a representative mix of created/modified/deleted rows.
fn setup_db(count: usize) -> AletheiaDB {
    let db = AletheiaDB::new().unwrap();
    for i in 0..count {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{i}").as_str())
            .insert("value", i as i64)
            .build();
        let node_id = db.create_node("Person", props).unwrap();

        // Every 5th node gets an update (a "modified" row).
        if i % 5 == 0 {
            let updated = PropertyMapBuilder::new()
                .insert("name", format!("Node_{i}").as_str())
                .insert("value", (i as i64) + 1)
                .build();
            db.write(|tx| {
                tx.update_node(node_id, updated.clone())?;
                Ok::<_, aletheiadb::Error>(())
            })
            .unwrap();
        }
    }
    db
}

fn bench_changefeed_24h_window(c: &mut Criterion) {
    let mut group = c.benchmark_group("changefeed_24h_window");

    for count in [100usize, 500, 1000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{count}_entities")),
            &count,
            |b, &count| {
                let db = setup_db(count);
                // A 24h window starting before all writes covers every committed version.
                let tx_from = Timestamp::from(0);
                let tx_to = TIMESTAMP_MAX;

                b.iter(|| {
                    let query = ChangeFeedQuery::new(
                        black_box(tx_from),
                        black_box(tx_to),
                        1000, // bounded page
                    );
                    let page = db.list_changes(black_box(&query)).unwrap();
                    black_box(page.changes.len());
                });
            },
        );
    }

    group.finish();
}

fn bench_changefeed_paginated(c: &mut Criterion) {
    let mut group = c.benchmark_group("changefeed_paginated");

    let db = setup_db(1000);
    let tx_from = Timestamp::from(0);
    let tx_to = TIMESTAMP_MAX;

    group.bench_function("page_100_of_1000", |b| {
        b.iter(|| {
            let mut query = ChangeFeedQuery::new(tx_from, tx_to, 100);
            query.cursor = None;
            let page = db.list_changes(black_box(&query)).unwrap();
            black_box(page.next_cursor.is_some());
        });
    });

    group.finish();
}

/// Bounded vs. unbounded `list_changes` over a large hot history (Issue #3216, PR 2).
///
/// **Honest framing.** The filter+limit pushdown's win is a **working-set / candidate-count**
/// reduction: a bounded page holds only `O(limit)` `RawChange`s in memory instead of `O(matches)`.
/// It is **not** a latency win — candidate *enumeration* is still an `O(N)` walk of the hot version
/// maps in v1 (a `(commit_ts, kind, id)` directory would be needed for a sub-linear scan), so
/// wall-clock time is expected to stay roughly flat between the bounded and unbounded cases. This
/// bench exists to track that the bounded page does not regress and to make the memory-bounded and
/// unbounded-fast-path routes measurable. It deliberately does **not** claim cold-scan
/// elimination: cold I/O remains `O(N)` (this bench is hot-only).
fn bench_list_changes_bounded_vs_unbounded(c: &mut Criterion) {
    let mut group = c.benchmark_group("list_changes_bounded_vs_unbounded");

    // ~24k committed versions (20k creates + every-5th update) in the hot tier.
    let db = setup_db(20_000);
    let tx_from = Timestamp::from(0);
    let tx_to = TIMESTAMP_MAX;

    // Bounded: a small page over the whole history — retains only `limit + 1` candidates via the
    // per-tier max-heap.
    group.bench_function("bounded_page_10_over_20k", |b| {
        b.iter(|| {
            let query = ChangeFeedQuery::new(black_box(tx_from), black_box(tx_to), 10);
            let page = db.list_changes(black_box(&query)).unwrap();
            black_box(page.changes.len());
        });
    });

    // Unbounded: drain everything — exercises the plain-`Vec` fast path (no heap maintenance) that
    // the `usize::MAX` bound selects.
    group.bench_function("unbounded_drain_20k", |b| {
        b.iter(|| {
            let query = ChangeFeedQuery::new(black_box(tx_from), black_box(tx_to), usize::MAX);
            let page = db.list_changes(black_box(&query)).unwrap();
            black_box(page.changes.len());
        });
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_changefeed_24h_window,
        bench_changefeed_paginated,
        bench_list_changes_bounded_vs_unbounded
);
criterion_main!(benches);
