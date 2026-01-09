//! Profiling benchmark for transaction commit bottleneck analysis.
//!
//! This benchmark is designed to run with Tracy profiler via the observability
//! framework to identify bottlenecks in the transaction commit path.
//!
//! ## Usage
//!
//! ```bash
//! # Terminal 1: Start Tracy GUI
//! ./tracy-profiler
//!
//! # Terminal 2: Run benchmark with observability-tracy
//! cargo bench --bench profiling_commit --features observability-tracy -- --profile-time 10
//! ```
//!
//! ## Benchmark Scenarios
//!
//! - **sequential**: Baseline single-threaded performance (no lock contention)
//! - **concurrent**: Exposes lock contention under concurrent writes
//! - **heavy**: Stresses apply_changes with large transactions
//! - **mixed**: Realistic mixed workload simulation
//!
//! With observability-tracy enabled, tracing spans automatically map to Tracy:
//! - `transaction_commit` - Full commit operation
//! - `commit_critical_section` - Timestamp + WAL lock hold time
//! - `apply_changes` - Graph operations (suspected 85-90% of time)
//! - `apply_changes_detailed` - Breakdown of graph operations
//! - `rebuild_adjacency_index` - Adjacency index rebuild overhead

use criterion::{BenchmarkId, Criterion, SamplingMode, black_box, criterion_group, criterion_main};
use gallifreydb::core::PropertyMapBuilder;
use gallifreydb::storage::wal::{DurabilityMode, WalConfig};
use gallifreydb::{GallifreyDB, WriteOps};
use std::sync::Arc;
use std::thread;
use tempfile::TempDir;

#[cfg(feature = "observability-tracy")]
use tracing_subscriber::layer::SubscriberExt;

/// Initialize Tracy subscriber for profiling
#[cfg(feature = "observability-tracy")]
fn init_tracy() {
    use tracing_subscriber::Registry;

    let tracy_layer = tracing_tracy::TracyLayer::default();
    let subscriber = Registry::default().with(tracy_layer);

    // Try to set as global default, ignore error if already set
    let _ = tracing::subscriber::set_global_default(subscriber);
}

#[cfg(not(feature = "observability-tracy"))]
fn init_tracy() {
    // No-op when Tracy not enabled
}

/// Helper to create a database with Synchronous durability for profiling
fn create_db_for_profiling() -> (GallifreyDB, TempDir) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = WalConfig {
        wal_dir: temp_dir.path().to_path_buf(),
        segment_size: 10 * 1024 * 1024,
        segments_to_retain: 3,
        durability_mode: DurabilityMode::Synchronous,
    };
    (GallifreyDB::with_wal_config(config), temp_dir)
}

/// Profile sequential commits (baseline - no lock contention)
fn profile_sequential_commits(c: &mut Criterion) {
    init_tracy();

    let mut group = c.benchmark_group("sequential");
    group.sample_size(50); // Enough samples for Tracy profiling
    group.sampling_mode(SamplingMode::Flat); // All iterations equally weighted

    for ops_per_tx in [1, 10, 50] {
        group.bench_with_input(
            BenchmarkId::new("ops", ops_per_tx),
            &ops_per_tx,
            |b, &ops_per_tx| {
                let (db, _guard) = create_db_for_profiling();
                let mut counter = 0u64;

                b.iter(|| {
                    db.write(|tx| {
                        for i in 0..ops_per_tx {
                            tx.create_node(
                                "TestNode",
                                PropertyMapBuilder::new()
                                    .insert("id", black_box(i) as i64)
                                    .insert("data", format!("value_{}", counter + i))
                                    .build(),
                            )?;
                        }
                        Ok(())
                    })
                    .unwrap();

                    counter += ops_per_tx;
                });
            },
        );
    }

    group.finish();
}

/// Profile concurrent commits (expose lock contention)
fn profile_concurrent_commits(c: &mut Criterion) {
    init_tracy();

    let mut group = c.benchmark_group("concurrent");
    group.sample_size(20); // Fewer samples for heavier concurrent test
    group.sampling_mode(SamplingMode::Flat);

    for thread_count in [2, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::new("threads", thread_count),
            &thread_count,
            |b, &thread_count| {
                let (db, _guard) = create_db_for_profiling();
                let db = Arc::new(db);

                b.iter(|| {
                    let handles: Vec<_> = (0..thread_count)
                        .map(|i| {
                            let db = Arc::clone(&db);
                            thread::spawn(move || {
                                db.write(|tx| {
                                    tx.create_node(
                                        "TestNode",
                                        PropertyMapBuilder::new()
                                            .insert("thread_id", black_box(i) as i64)
                                            .build(),
                                    )
                                })
                                .unwrap();
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Profile heavy transactions (large graph operations to stress apply_changes)
fn profile_heavy_transactions(c: &mut Criterion) {
    init_tracy();

    let mut group = c.benchmark_group("heavy");
    group.sample_size(10); // Even fewer samples for very heavy workload
    group.sampling_mode(SamplingMode::Flat);

    group.bench_function("100_nodes_200_edges", |b| {
        let (db, _guard) = create_db_for_profiling();

        b.iter(|| {
            db.write(|tx| {
                // Create 100 nodes
                let node_ids: Result<Vec<_>, _> = (0..100)
                    .map(|i| {
                        tx.create_node(
                            "HeavyNode",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .insert("data", format!("node_{}", i))
                                .build(),
                        )
                    })
                    .collect();
                let node_ids = node_ids?;

                // Create 200 edges (pseudo-random connections)
                for i in 0..200 {
                    let from = node_ids[i % 100];
                    let to = node_ids[(i * 7) % 100];
                    if from != to {
                        tx.create_edge(
                            from,
                            to,
                            "CONNECTS",
                            PropertyMapBuilder::new()
                                .insert("weight", black_box(i) as i64)
                                .build(),
                        )?;
                    }
                }

                Ok(())
            })
            .unwrap();
        });
    });

    group.finish();
}

/// Profile mixed workload (realistic scenario with multiple threads and mixed operations)
fn profile_mixed_workload(c: &mut Criterion) {
    init_tracy();

    let mut group = c.benchmark_group("mixed");
    group.sample_size(20);
    group.sampling_mode(SamplingMode::Flat);

    group.bench_function("8_threads_mixed_ops", |b| {
        let (db, _guard) = create_db_for_profiling();
        let db = Arc::new(db);

        b.iter(|| {
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let db = Arc::clone(&db);
                    thread::spawn(move || {
                        db.write(|tx| {
                            // Each thread: 5 nodes, 10 edges between them
                            let nodes: Result<Vec<_>, _> = (0..5)
                                .map(|j| {
                                    tx.create_node(
                                        "MixedNode",
                                        PropertyMapBuilder::new()
                                            .insert("thread", black_box(i) as i64)
                                            .insert("seq", black_box(j) as i64)
                                            .build(),
                                    )
                                })
                                .collect();
                            let nodes = nodes?;

                            for j in 0..10 {
                                let from = nodes[j % 5];
                                let to = nodes[(j + 1) % 5];
                                tx.create_edge(
                                    from,
                                    to,
                                    "LINKS",
                                    PropertyMapBuilder::new()
                                        .insert("link_id", black_box(j) as i64)
                                        .build(),
                                )?;
                            }

                            Ok(())
                        })
                        .unwrap();
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    group.finish();
}

criterion_group!(
    profiling_benches,
    profile_sequential_commits,
    profile_concurrent_commits,
    profile_heavy_transactions,
    profile_mixed_workload,
);
criterion_main!(profiling_benches);
