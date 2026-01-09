//! Benchmarks for WAL durability modes
//!
//! Compares performance of:
//! - Synchronous: fsync per commit (baseline)
//! - Async: Background flush thread
//! - GroupCommit: Batched fsync with ACID guarantees
//! - AsyncBatched: Async with intelligent batching (<100µs target)

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::{
    GallifreyDB, WriteOps, WriteOptions, core::PropertyMapBuilder, storage::wal::DurabilityMode,
    storage::wal::WalConfig,
};
use std::sync::Arc;
use std::thread;
use tempfile::TempDir;

/// Helper to create a database with a specific durability mode.
///
/// Returns both the database and the TempDir guard. The guard must be kept
/// alive for the duration of the benchmark to prevent the directory from
/// being cleaned up prematurely. When the guard is dropped, the directory
/// is automatically cleaned up.
fn create_db_with_mode(mode: DurabilityMode) -> (GallifreyDB, TempDir) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = WalConfig {
        wal_dir: temp_dir.path().to_path_buf(),
        segment_size: 10 * 1024 * 1024,
        segments_to_retain: 3,
        durability_mode: mode,
    };
    let db = GallifreyDB::with_wal_config(config);
    (db, temp_dir)
}

/// Benchmark single transaction latency for each mode
///
/// The AsyncBatched benchmarks validate Issue #128's <100µs latency requirement.
/// Expected results: AsyncBatched modes should show 30-80µs average latency.
fn bench_single_transaction_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_transaction_latency");

    // Synchronous (baseline)
    group.bench_function("synchronous", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Synchronous);
        let mut counter = 0u64;

        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Benchmark",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // Async mode
    group.bench_function("async", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        });
        let mut counter = 0u64;

        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Benchmark",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // GroupCommit default (2ms, 200 batch)
    group.bench_function("group_commit_default", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::group_commit_default());
        let mut counter = 0u64;

        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Benchmark",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // GroupCommit fast (1ms, 500 batch) - high throughput
    group.bench_function("group_commit_fast", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::fast());
        let mut counter = 0u64;

        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Benchmark",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // AsyncBatched default (10ms, 100 batch) - low latency target
    group.bench_function("async_batched_default", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::async_batched_default());
        let mut counter = 0u64;

        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Benchmark",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // AsyncBatched aggressive (5ms, 50 batch) - ultra-low latency
    group.bench_function("async_batched_aggressive", |b| {
        let (db, _guard) =
            create_db_with_mode(DurabilityMode::async_batched_validated(5, 50).unwrap());
        let mut counter = 0u64;

        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Benchmark",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    group.finish();
}

/// Benchmark batch throughput for each mode
fn bench_batch_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_throughput");
    group.throughput(Throughput::Elements(1000));

    let modes = vec![
        ("synchronous", DurabilityMode::Synchronous),
        (
            "async",
            DurabilityMode::Async {
                flush_interval_ms: 100,
            },
        ),
        (
            "group_commit",
            DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 100,
            },
        ),
        (
            "async_batched",
            DurabilityMode::AsyncBatched {
                max_delay_ms: 10,
                max_batch_size: 100,
            },
        ),
    ];

    for (name, mode) in modes {
        group.bench_function(name, |b| {
            let (db, _guard) = create_db_with_mode(mode);

            b.iter(|| {
                for i in 0..1000 {
                    db.write(|tx| {
                        tx.create_node(
                            "Benchmark",
                            PropertyMapBuilder::new()
                                .insert("batch_id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark concurrent writes for each mode
fn bench_concurrent_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_writes");
    group.throughput(Throughput::Elements(100));

    let modes = vec![
        ("synchronous", DurabilityMode::Synchronous),
        (
            "async",
            DurabilityMode::Async {
                flush_interval_ms: 100,
            },
        ),
        (
            "group_commit",
            DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 20,
            },
        ),
        (
            "async_batched",
            DurabilityMode::AsyncBatched {
                max_delay_ms: 10,
                max_batch_size: 20,
            },
        ),
    ];

    for (name, mode) in modes {
        group.bench_function(name, |b| {
            let (db, _guard) = create_db_with_mode(mode);
            let db = Arc::new(db);

            b.iter(|| {
                let mut handles = Vec::new();

                // Spawn 10 threads, each doing 10 writes
                for thread_id in 0..10 {
                    let db = Arc::clone(&db);
                    let handle = thread::spawn(move || {
                        for i in 0..10 {
                            db.write(|tx| {
                                tx.create_node(
                                    "Concurrent",
                                    PropertyMapBuilder::new()
                                        .insert("thread", thread_id as i64)
                                        .insert("counter", i as i64)
                                        .build(),
                                )
                            })
                            .unwrap();
                        }
                    });
                    handles.push(handle);
                }

                for handle in handles {
                    handle.join().unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark GroupCommit batch size sensitivity
fn bench_group_commit_batch_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_batch_sizes");
    group.throughput(Throughput::Elements(100));

    for batch_size in &[10, 50, 100, 200, 500] {
        group.bench_function(BenchmarkId::from_parameter(batch_size), |b| {
            let (db, _guard) = create_db_with_mode(DurabilityMode::GroupCommit {
                max_delay_ms: 50,
                max_batch_size: *batch_size,
            });

            b.iter(|| {
                for i in 0..100 {
                    db.write(|tx| {
                        tx.create_node(
                            "BatchTest",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark GroupCommit max_delay sensitivity
fn bench_group_commit_delays(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_delays");
    group.throughput(Throughput::Elements(100));

    for delay_ms in &[1, 5, 10, 20, 50] {
        group.bench_function(BenchmarkId::from_parameter(delay_ms), |b| {
            let (db, _guard) = create_db_with_mode(DurabilityMode::GroupCommit {
                max_delay_ms: *delay_ms,
                max_batch_size: 100,
            });

            b.iter(|| {
                for i in 0..100 {
                    db.write(|tx| {
                        tx.create_node(
                            "DelayTest",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark per-transaction override performance
fn bench_per_transaction_override(c: &mut Criterion) {
    let mut group = c.benchmark_group("per_transaction_override");

    // Async DB with Synchronous override
    group.bench_function("async_db_sync_override", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        });
        let options = WriteOptions {
            durability_mode: Some(DurabilityMode::Synchronous),
        };
        let mut counter = 0u64;

        b.iter(|| {
            db.write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "Override",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // Synchronous DB with Async override
    group.bench_function("sync_db_async_override", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Synchronous);
        let options = WriteOptions {
            durability_mode: Some(DurabilityMode::Async {
                flush_interval_ms: 100,
            }),
        };
        let mut counter = 0u64;

        b.iter(|| {
            db.write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "Override",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // AsyncBatched DB with Synchronous override
    group.bench_function("async_batched_db_sync_override", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::async_batched_default());
        let options = WriteOptions {
            durability_mode: Some(DurabilityMode::Synchronous),
        };
        let mut counter = 0u64;

        b.iter(|| {
            db.write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "Override",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    // Synchronous DB with AsyncBatched override
    group.bench_function("sync_db_async_batched_override", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Synchronous);
        let options = WriteOptions {
            durability_mode: Some(DurabilityMode::async_batched_default()),
        };
        let mut counter = 0u64;

        b.iter(|| {
            db.write_with_options(options.clone(), |tx| {
                tx.create_node(
                    "Override",
                    PropertyMapBuilder::new()
                        .insert("counter", black_box(counter) as i64)
                        .build(),
                )
            })
            .unwrap();
            counter += 1;
        });
    });

    group.finish();
}

/// Benchmark mixed workload (90% Async, 10% Synchronous)
fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_workload");
    group.throughput(Throughput::Elements(100));

    group.bench_function("90_async_10_sync", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        });
        let sync_options = WriteOptions {
            durability_mode: Some(DurabilityMode::Synchronous),
        };

        b.iter(|| {
            for i in 0..100 {
                if i % 10 == 0 {
                    // 10% critical transactions use Synchronous
                    db.write_with_options(sync_options.clone(), |tx| {
                        tx.create_node(
                            "Critical",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                } else {
                    // 90% regular transactions use Async
                    db.write(|tx| {
                        tx.create_node(
                            "Regular",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            }
        });
    });

    group.bench_function("90_async_batched_10_sync", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::async_batched_default());
        let sync_options = WriteOptions {
            durability_mode: Some(DurabilityMode::Synchronous),
        };

        b.iter(|| {
            for i in 0..100 {
                if i % 10 == 0 {
                    // 10% critical transactions use Synchronous
                    db.write_with_options(sync_options.clone(), |tx| {
                        tx.create_node(
                            "Critical",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                } else {
                    // 90% regular transactions use AsyncBatched
                    db.write(|tx| {
                        tx.create_node(
                            "Regular",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            }
        });
    });

    group.bench_function("90_async_batched_10_group_commit", |b| {
        let (db, _guard) = create_db_with_mode(DurabilityMode::async_batched_default());
        let gc_options = WriteOptions {
            durability_mode: Some(DurabilityMode::group_commit_default()),
        };

        b.iter(|| {
            for i in 0..100 {
                if i % 10 == 0 {
                    // 10% critical transactions use GroupCommit (ACID)
                    db.write_with_options(gc_options.clone(), |tx| {
                        tx.create_node(
                            "Critical",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                } else {
                    // 90% regular transactions use AsyncBatched
                    db.write(|tx| {
                        tx.create_node(
                            "Regular",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            }
        });
    });

    group.finish();
}

/// Benchmark AsyncBatched batch size sensitivity
fn bench_async_batched_batch_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_batched_batch_sizes");
    group.throughput(Throughput::Elements(100));

    for batch_size in &[10, 50, 100, 200, 500] {
        group.bench_function(BenchmarkId::from_parameter(batch_size), |b| {
            let (db, _guard) = create_db_with_mode(DurabilityMode::AsyncBatched {
                max_delay_ms: 50,
                max_batch_size: *batch_size,
            });

            b.iter(|| {
                for i in 0..100 {
                    db.write(|tx| {
                        tx.create_node(
                            "BatchTest",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark AsyncBatched max_delay sensitivity
fn bench_async_batched_delays(c: &mut Criterion) {
    let mut group = c.benchmark_group("async_batched_delays");
    group.throughput(Throughput::Elements(100));

    for delay_ms in &[1, 5, 10, 20, 50] {
        group.bench_function(BenchmarkId::from_parameter(delay_ms), |b| {
            let (db, _guard) = create_db_with_mode(DurabilityMode::AsyncBatched {
                max_delay_ms: *delay_ms,
                max_batch_size: 100,
            });

            b.iter(|| {
                for i in 0..100 {
                    db.write(|tx| {
                        tx.create_node(
                            "DelayTest",
                            PropertyMapBuilder::new()
                                .insert("id", black_box(i) as i64)
                                .build(),
                        )
                    })
                    .unwrap();
                }
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_single_transaction_latency,
    bench_batch_throughput,
    bench_concurrent_writes,
    bench_group_commit_batch_sizes,
    bench_group_commit_delays,
    bench_async_batched_batch_sizes,
    bench_async_batched_delays,
    bench_per_transaction_override,
    bench_mixed_workload,
);
criterion_main!(benches);
