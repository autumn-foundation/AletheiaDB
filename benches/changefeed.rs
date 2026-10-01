//! Benchmarks for the graph-wide temporal changefeed (Issue #3216).
//!
//! Success metric: a changefeed query over a 24-hour transaction-time window returning
//! <=1,000 changed entities should complete in well under 10 ms (matching the published
//! temporal-reconstruction target).

mod common;

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::PropertyMap;
use aletheiadb::core::changefeed::ChangeFeedQuery;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, Timestamp};
use aletheiadb::core::version::NodeVersion;
use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
use aletheiadb::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;

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

/// Cold-tier `list_changes` pushdown via the `ColdChangeDirectory` (Issue #3677).
///
/// **What this measures — and what it does NOT claim.** The pushdown's headline win is proven
/// *precisely* by the unit-test decode counter, not by this bench: over a 200-version cold history
/// a bounded (`limit = 5`) whole-timeline query decodes **5** cold versions with the directory on
/// versus **200** with it off (see `cold_directory_early_stop_bound` /
/// `cold_directory_version_id_inversion_parity` in `src/db/temporal.rs`). That is the real result:
/// decodes-per-query drop from `O(N_cold)` (full scan of `collect_changes_filtered`) to
/// `O(window)` (directory-seeded point-reads of only the in-window candidates). The
/// `reset_cold_decode_counter`/`cold_decode_count` probes are `pub(crate)`, so a bench (a separate
/// crate) cannot read them; this bench instead demonstrates the **wall-time consequence** of that
/// decode reduction on a large cold history, and separately reports the one-time lazy seed cost.
///
/// Cold history size: **50,000** synthetic cold node anchors, one per version, with transaction
/// times spread `1_000 µs` apart across `[1_000, 50_000_000]`. Chosen large enough that the
/// full-scan "before" path is clearly dominated by cold decode work, yet small enough to seed and
/// bench in reasonable time. All three benches issue the **identical** query — a bounded
/// `limit = 10` page over a narrow *recent* transaction-time window matching ~11 versions (the last
/// 11 anchors) — so the only variable across "before"/"after" is the directory being on vs off.
fn bench_list_changes_cold_pushdown(c: &mut Criterion) {
    /// Number of synthetic cold node versions (see module comment for the size rationale).
    const N_COLD: u64 = 50_000;

    // Build the large cold history exactly once (populated on disk; the tempdir must outlive the
    // benchmarks that read it).
    let dir = tempfile::tempdir().unwrap();
    let cold = Arc::new(
        RedbColdStorage::with_default_config(dir.path().join("cold.redb"))
            .expect("open cold store"),
    );
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let versions: Vec<NodeVersion> = (1..=N_COLD)
        .map(|i| {
            let tx = Timestamp::from((i as i64) * 1_000);
            NodeVersion::new_anchor(
                VersionId::new(i).unwrap(),
                NodeId::new(i).unwrap(),
                BiTemporalInterval::now(tx, tx),
                label,
                PropertyMap::new(),
            )
        })
        .collect();
    cold.store_node_versions_batch(&versions)
        .expect("seed cold");

    // Narrow recent window: half-open [49_989_500, 50_000_001) selects versions with tx-time in
    // {49_990_000, 49_991_000, ..., 50_000_000} — the last 11 anchors. A `limit = 10` page returns
    // 10 with `has_more = true`.
    let tx_from = Timestamp::from(49_989_500);
    let tx_to = Timestamp::from(50_000_001);
    let limit = 10usize;

    // Attach a fresh, unseeded tiered storage (over the shared, already-populated cold store) to a
    // fresh DB, with a caller-chosen directory budget. `max_entries = 0` disables the directory,
    // forcing every query down the full-scan degrade path — the honest v1 "before" baseline.
    let attach = |max_entries: usize| -> AletheiaDB {
        let db = AletheiaDB::new().unwrap();
        let config = TieredStorageConfig {
            cold_change_directory_max_entries: max_entries,
            ..Default::default()
        };
        let tiered = Arc::new(TieredStorage::new(config, cold.clone()));
        db.__test_historical_storage()
            .write()
            .set_tiered_storage(tiered);
        db
    };

    let mut group = c.benchmark_group("list_changes_cold_pushdown");

    // AFTER — directory ON: the pushdown fast path. The lazy seed is a one-time latched cost, so a
    // warm-up query triggers it before the timed loop, leaving only the query-time point-reads of
    // the ~11 window candidates being measured.
    {
        let db = attach(1_000_000);
        let _ = db
            .list_changes(&ChangeFeedQuery::new(tx_from, tx_to, limit))
            .unwrap();
        group.bench_function("directory_on_recent_window_limit10", |b| {
            b.iter(|| {
                let q = ChangeFeedQuery::new(black_box(tx_from), black_box(tx_to), limit);
                let page = db.list_changes(black_box(&q)).unwrap();
                black_box(page.changes.len());
            });
        });
    }

    // BEFORE — directory OFF (`max_entries = 0`): the same bounded window query, but every call
    // full-scans and decodes all 50,000 cold versions via `collect_changes_filtered`.
    {
        let db = attach(0);
        let _ = db
            .list_changes(&ChangeFeedQuery::new(tx_from, tx_to, limit))
            .unwrap();
        group.bench_function("directory_off_recent_window_limit10", |b| {
            b.iter(|| {
                let q = ChangeFeedQuery::new(black_box(tx_from), black_box(tx_to), limit);
                let page = db.list_changes(black_box(&q)).unwrap();
                black_box(page.changes.len());
            });
        });
    }

    // One-time lazy seed cost. The directory is (re)built by streaming the whole cold history on the
    // FIRST changefeed use; this reports that honest `O(N_cold)` rebuild cost the reviewers asked
    // for. Each iteration attaches a fresh (unseeded) tiered storage in the untimed setup, then the
    // timed routine runs the first (seeding) query — so the measurement is "one-time seed over 50k
    // cold versions + one narrow window query".
    group.bench_function("first_query_includes_seed_50k", |b| {
        b.iter_batched(
            || attach(1_000_000),
            |db| {
                let q = ChangeFeedQuery::new(tx_from, tx_to, limit);
                let page = db.list_changes(black_box(&q)).unwrap();
                black_box(page.changes.len());
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_changefeed_24h_window,
        bench_changefeed_paginated,
        bench_list_changes_bounded_vs_unbounded,
        bench_list_changes_cold_pushdown
);
criterion_main!(benches);
