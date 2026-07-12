//! Throughput benchmarks for the tamper-evident provenance hash chain
//! (Issue #3351).
//!
//! The chain is an opt-in, async sidecar: on the commit hot path it only
//! *captures* the version refs a transaction produced and enqueues them to a
//! background sealer (the SHA-256 folding happens off the critical path). The
//! acceptance metric (#3383 concern) is that enabling the chain keeps write
//! throughput at **≥ 90% of the chain-disabled baseline** under GroupCommit
//! durability.
//!
//! This bench measures GroupCommit `create_node` throughput with the chain
//! DISABLED (baseline) vs ENABLED, on otherwise-identical durable databases,
//! and prints the enabled/baseline ratio so the comparison is explicit. It is
//! deliberately cheap to sample so CI/local runs stay fast; the ratio is
//! reported, not hard-gated here (headless CI hardware is noisy — gate the
//! metric on the performance rig).
//!
//! Two workloads are covered: the original single-thread **create-only** path
//! (a best case for the sealer — one leaf per transaction, no property
//! reconstruction churn) and, added under Issue #3351 task #7, a
//! **multi-threaded update-heavy** path. Updates supersede prior versions (the
//! seal path reconstructs full property state and the timeline shifts), and
//! concurrent writers contend on the bounded seal channel — so the update
//! variant stresses inline-seal backpressure that the create-only best case
//! never hits.

mod common;

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::config::WalConfigBuilder;
use aletheiadb::provenance_chain::{ChainConfig, ChainFsyncMode};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, NodeId, PropertyMapBuilder};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tempfile::TempDir;

/// Number of `create_node` writes measured per benchmark iteration.
const WRITES_PER_ITER: u64 = 1000;

/// Build a durable, GroupCommit database rooted at `data_dir`, optionally
/// enabling the provenance hash chain. Both variants share identical WAL and
/// persistence settings so the only measured difference is the chain.
fn build_db(data_dir: &std::path::Path, chain_enabled: bool) -> AletheiaDB {
    let mut builder = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::GroupCommit {
                    max_delay_ms: 10,
                    max_batch_size: 200,
                })
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        });

    if chain_enabled {
        builder = builder.chain(ChainConfig {
            enabled: true,
            // Batched flush keeps the sealer off the commit critical path —
            // the durability model this metric targets.
            fsync: ChainFsyncMode::Batched,
            dir: None,
        });
    }

    AletheiaDB::with_unified_config(builder.build()).expect("db init")
}

/// Drive `WRITES_PER_ITER` `create_node` writes against `db`, returning nothing
/// but forcing the work via `black_box`.
fn drive_writes(db: &AletheiaDB, counter: &AtomicU64) {
    for _ in 0..WRITES_PER_ITER {
        let n = counter.fetch_add(1, Ordering::Relaxed);
        let id = db
            .create_node(
                "Bench",
                PropertyMapBuilder::new()
                    .insert("counter", black_box(n) as i64)
                    .build(),
            )
            .expect("create_node");
        black_box(id);
    }
}

/// Measured wall-clock throughput (writes/sec) of `WRITES_PER_ITER` writes,
/// used to compute and print the enabled/baseline ratio outside Criterion's
/// statistical harness.
fn measure_throughput(chain_enabled: bool) -> f64 {
    let dir = TempDir::new().expect("temp dir");
    let db = build_db(dir.path(), chain_enabled);
    let counter = AtomicU64::new(0);
    // Warm up so index/WAL structures are populated before timing.
    drive_writes(&db, &counter);
    let start = Instant::now();
    let iters = 5u64;
    for _ in 0..iters {
        drive_writes(&db, &counter);
    }
    let elapsed = start.elapsed().as_secs_f64();
    (WRITES_PER_ITER * iters) as f64 / elapsed
}

/// Benchmark GroupCommit write throughput: chain disabled vs enabled.
fn bench_chain_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("provenance_chain_write_throughput");
    group.throughput(Throughput::Elements(WRITES_PER_ITER));

    group.bench_function("chain_disabled", |b| {
        let dir = TempDir::new().expect("temp dir");
        let db = build_db(dir.path(), false);
        let counter = AtomicU64::new(0);
        b.iter(|| drive_writes(&db, &counter));
    });

    group.bench_function("chain_enabled", |b| {
        let dir = TempDir::new().expect("temp dir");
        let db = build_db(dir.path(), true);
        let counter = AtomicU64::new(0);
        b.iter(|| drive_writes(&db, &counter));
    });

    group.finish();

    // Report the enabled/baseline ratio explicitly (the AC success metric).
    // This is an out-of-band coarse wall-clock comparison, not a Criterion
    // statistical result; it makes the ≥90% target visible in the bench output
    // without hard-failing on noisy CI hardware.
    let baseline = measure_throughput(false);
    let enabled = measure_throughput(true);
    let ratio = if baseline > 0.0 {
        enabled / baseline
    } else {
        f64::NAN
    };
    println!(
        "\n[provenance-chain] GroupCommit write throughput\n\
         [provenance-chain]   chain DISABLED: {baseline:>10.0} writes/sec (baseline)\n\
         [provenance-chain]   chain ENABLED:  {enabled:>10.0} writes/sec\n\
         [provenance-chain]   ratio enabled/baseline: {ratio:.3} \
         (target >= 0.90 on the perf rig)\n"
    );
}

/// Number of writer threads for the concurrent update variant.
const UPDATE_THREADS: u64 = 4;
/// Updates each thread performs per benchmark iteration.
const UPDATES_PER_THREAD: u64 = 250;

/// Drive `UPDATE_THREADS` threads, each issuing `UPDATES_PER_THREAD`
/// `update_node` writes cycling over the pre-created `nodes`. Updates supersede
/// prior versions, so the seal path reconstructs full property state and the
/// per-entity timeline grows — and concurrent writers contend on the bounded
/// seal channel (inline-seal backpressure) that create-only never exercises.
fn drive_concurrent_updates(db: &Arc<AletheiaDB>, nodes: &Arc<Vec<NodeId>>, counter: &AtomicU64) {
    let mut handles = Vec::with_capacity(UPDATE_THREADS as usize);
    for _ in 0..UPDATE_THREADS {
        let db = Arc::clone(db);
        let nodes = Arc::clone(nodes);
        let base = counter.fetch_add(UPDATES_PER_THREAD, Ordering::Relaxed);
        handles.push(std::thread::spawn(move || {
            for i in 0..UPDATES_PER_THREAD {
                let node = nodes[(base + i) as usize % nodes.len()];
                let n = base + i;
                db.write(|tx| {
                    tx.update_node(
                        node,
                        PropertyMapBuilder::new()
                            .insert("counter", black_box(n) as i64)
                            .build(),
                    )?;
                    Ok::<_, aletheiadb::Error>(())
                })
                .expect("update_node");
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread");
    }
}

/// Benchmark multi-threaded, update-heavy write throughput with the chain
/// enabled (Issue #3351 task #7). This is the worst-case complement to the
/// single-thread create-only best case above.
fn bench_chain_concurrent_updates(c: &mut Criterion) {
    let total = UPDATE_THREADS * UPDATES_PER_THREAD;
    let mut group = c.benchmark_group("provenance_chain_concurrent_updates");
    group.throughput(Throughput::Elements(total));

    group.bench_function("chain_enabled_updates", |b| {
        let dir = TempDir::new().expect("temp dir");
        let db = Arc::new(build_db(dir.path(), true));
        // Pre-create a pool of nodes the update threads cycle over.
        let nodes: Vec<NodeId> = (0..64)
            .map(|i| {
                db.create_node(
                    "Bench",
                    PropertyMapBuilder::new().insert("seed", i as i64).build(),
                )
                .expect("seed node")
            })
            .collect();
        let nodes = Arc::new(nodes);
        let counter = AtomicU64::new(0);
        b.iter(|| drive_concurrent_updates(&db, &nodes, &counter));
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_chain_write_throughput, bench_chain_concurrent_updates,
);
criterion_main!(benches);
