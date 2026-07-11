//! Benchmarks for the queryable bi-temporal extent (Issue #3238).
//!
//! Covers:
//! - `AletheiaDB::temporal_extent()` — must be O(1) regardless of the number
//!   of recorded versions (it reads a write-time maintained aggregate).
//! - `AletheiaDB::temporal_extent_by_label()` — O(hot-tier versions) fold
//!   for the per-label breakdown.
//! - `TemporalIndexes::extent()` — the underlying O(1) aggregate read.

mod common;

use aletheiadb::AletheiaDB;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange, Timestamp};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Populate the database's temporal indexes directly with `count` versions
/// spread across `count / 10` entities (10 versions each), so the aggregate
/// observes a realistic mix of entities and versions.
fn populate_index_versions(db: &AletheiaDB, count: u64) {
    let indexes = db.__test_temporal_indexes();
    let per_entity = 10;
    for i in 0..count {
        let node_id = NodeId::new(i / per_entity + 1).unwrap();
        let version_id = VersionId::new(i + 1).unwrap();
        let start = 1_000_000_000_000_000 + i as i64 * 1_000;
        let interval = BiTemporalInterval::new(
            TimeRange::from(Timestamp::from(start)),
            TimeRange::from(Timestamp::from(start)),
        );
        indexes
            .insert_node_version(node_id, version_id, interval)
            .unwrap();
    }
}

/// Overall extent: O(1) aggregate read, independent of version count.
fn bench_temporal_extent_overall(c: &mut Criterion) {
    let mut group = c.benchmark_group("temporal_extent_overall");

    for count in [100_000u64, 1_000_000] {
        let db = AletheiaDB::new().expect("db init");
        populate_index_versions(&db, count);

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_versions", count)),
            &count,
            |b, _| {
                b.iter(|| black_box(db.temporal_extent().expect("extent")));
            },
        );
    }

    group.finish();
}

/// The underlying index aggregate read (no db wrapper).
fn bench_index_extent_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("temporal_extent_index_read");

    let db = AletheiaDB::new().expect("db init");
    populate_index_versions(&db, 1_000_000);
    let indexes = db.__test_temporal_indexes();

    group.bench_function("1000000_versions", |b| {
        b.iter(|| black_box(indexes.extent()));
    });

    group.finish();
}

/// Per-label breakdown: O(hot-tier versions) fold over the historical store.
fn bench_temporal_extent_by_label(c: &mut Criterion) {
    let mut group = c.benchmark_group("temporal_extent_by_label");
    // The full-store fold is comparatively slow; keep sample counts sane.
    group.sample_size(20);

    for count in [10_000u64, 100_000] {
        let db = AletheiaDB::new().expect("db init");
        let labels = ["Person", "Company", "Event", "Place"];
        for i in 0..count {
            let label = labels[(i % labels.len() as u64) as usize];
            let valid = Timestamp::from(1_000_000_000_000_000 + i as i64 * 1_000);
            db.create_node_with_valid_time(
                label,
                PropertyMapBuilder::new().insert("i", i.to_string()).build(),
                Some(valid),
            )
            .expect("create node");
        }

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_versions", count)),
            &count,
            |b, _| {
                b.iter(|| black_box(db.temporal_extent_by_label().expect("extent")));
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_temporal_extent_overall,
    bench_index_extent_read,
    bench_temporal_extent_by_label
);
criterion_main!(benches);
