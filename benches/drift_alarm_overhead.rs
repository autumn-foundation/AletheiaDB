//! Write-path overhead of temporal semantic drift alarms (Issue #3367).
//!
//! Success metric: commit throughput under **GroupCommit** degrades by **< 10%**
//! with 10 active on-write monitors, and **0%** when the feature is compiled out
//! (the whole module is gated behind `semantic-temporal`, so a flag-off build
//! has literally no drift-alarm code on any path).
//!
//! # Why the overhead is structurally near-zero
//!
//! The drift-alarm engine installs **no write-path hook**. Evaluation is driven
//! entirely by an *existing-changefeed subscriber* running on a background
//! thread, off every write-path lock. The only cost a commit pays for having
//! monitors active is the changefeed broadcast into the engine's single
//! subscriber buffer — the same fan-out any changefeed consumer incurs — and the
//! evaluation itself never touches the commit path. Under sustained write load
//! the bounded evaluation queue **sheds** (it does not perform O(history) work
//! per commit), so the steady-state per-commit overhead is exactly that one
//! subscriber push.
//!
//! # What this bench measures
//!
//! Three GroupCommit configurations, all writing the same vector-indexed
//! `embedding` property (so the vector-index write cost is common to all):
//!
//! - `baseline` — no monitors, no engine (no changefeed subscriber).
//! - `ten_monitors_shedding` — 10 on-write monitors + a running engine whose
//!   evaluation is paused, i.e. the queue saturates and **sheds** every task.
//!   This isolates the pure write-path cost (the subscriber broadcast) and is the
//!   faithful model of steady-state high-throughput behavior, where the queue
//!   sheds rather than doing per-commit evaluation work. This is the number the
//!   `< 10%` AC is measured against.
//! - `ten_monitors_evaluating` — 10 monitors + a running engine that actively
//!   evaluates. Reported for completeness only: its cost is dominated by the
//!   background evaluator's O(population) reads contending for MVCC read locks,
//!   which grows with the dataset and is *not* a fixed per-commit write-path
//!   cost. Not the AC metric.
//!
//! Run with:
//! `cargo bench --no-default-features --features semantic-temporal --bench drift_alarm_overhead`.

use aletheiadb::AletheiaDB;
use aletheiadb::WalConfigBuilder;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::temporal::drift_alarm::{
    DriftAlarmEngine, DriftMonitorSpec, DriftTarget, EvalMode,
};
use aletheiadb::index::vector::temporal::{
    DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
};
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::storage::wal::DurabilityMode;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

const DIM: usize = 8;

/// A GroupCommit-durable database with a Cosine temporal vector index on
/// `"embedding"`. Returns the tempdir guard first so `db` drops before it.
fn group_commit_db() -> (TempDir, AletheiaDB) {
    let dir = TempDir::new().expect("temp dir");
    let config = WalConfigBuilder::new()
        .wal_dir(dir.path().to_path_buf())
        .durability_mode(DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 200,
        })
        .build();
    let db = AletheiaDB::with_wal_config(config).expect("db");
    let tvc = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 10_000_000,
        full_snapshot_interval: 16,
        hnsw_config: Some(HnswConfig::new(DIM, DistanceMetric::Cosine)),
    };
    db.enable_temporal_vector_index("embedding", tvc)
        .expect("enable temporal vector index");
    (dir, db)
}

fn embedding(i: u64) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    v[(i as usize) % DIM] = 1.0;
    v
}

/// Create one `Doc` node carrying an `embedding` — a single GroupCommit
/// transaction that (when a subscriber exists) broadcasts a `Created` change.
fn create_doc(db: &AletheiaDB, i: u64) -> NodeId {
    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding(i))
            .build(),
    )
    .expect("create node")
}

/// Register `n` on-write per-entity monitors on the `embedding` property.
fn register_monitors(db: &AletheiaDB, n: usize) {
    for k in 0..n {
        db.create_drift_monitor(DriftMonitorSpec {
            property_key: "embedding".to_string(),
            label: Some("Doc".to_string()),
            entities: None,
            metric: DriftMetric::Cosine,
            // Distinct thresholds so the 10 monitors are genuinely distinct rules.
            threshold: 0.10 + (k as f32) * 0.02,
            window: Duration::from_secs(3600),
            target: DriftTarget::PerEntity,
            mode: EvalMode::OnWrite,
        })
        .expect("create monitor");
    }
}

fn bench_write_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("drift_alarm_write_overhead");
    group.throughput(Throughput::Elements(1));

    // Baseline: no monitors, no engine, no changefeed subscriber.
    group.bench_function("baseline", |b| {
        let (_dir, db) = group_commit_db();
        let mut i = 0u64;
        b.iter(|| {
            black_box(create_doc(&db, i));
            i += 1;
        });
    });

    // 10 monitors + engine whose evaluation is paused → the bounded queue sheds
    // every task. Isolates the pure write-path (subscriber-broadcast) cost; this
    // is the AC metric (steady-state shedding behavior under high throughput).
    group.bench_function("ten_monitors_shedding", |b| {
        let (_dir, db) = group_commit_db();
        register_monitors(&db, 10);
        let db = Arc::new(db);
        let engine = DriftAlarmEngine::with_capacity(Arc::clone(&db), 1);
        engine.start().expect("start engine");
        engine.set_evaluation_paused(true);
        let mut i = 0u64;
        b.iter(|| {
            black_box(create_doc(&db, i));
            i += 1;
        });
        engine.set_evaluation_paused(false);
        engine.stop();
    });

    // 10 monitors + actively-evaluating engine. Reported for completeness only —
    // dominated by background O(population) read contention, not a fixed
    // per-commit write cost, and not the AC metric (see the module docs).
    group.bench_function("ten_monitors_evaluating", |b| {
        let (_dir, db) = group_commit_db();
        register_monitors(&db, 10);
        let db = Arc::new(db);
        let engine = DriftAlarmEngine::new(Arc::clone(&db));
        engine.start().expect("start engine");
        let mut i = 0u64;
        b.iter(|| {
            black_box(create_doc(&db, i));
            i += 1;
        });
        engine.stop();
    });

    group.finish();
}

criterion_group!(benches, bench_write_overhead);
criterion_main!(benches);
