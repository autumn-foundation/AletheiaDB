//! Benchmarks for WAL durability modes and low-level WAL operations
//!
//! Compares performance of:
//! - Synchronous: fsync per commit (baseline)
//! - Async: Background flush thread
//! - GroupCommit: Batched fsync with ACID guarantees
//! - AsyncBatched: Async with intelligent batching (<100µs target)
//!
//! Also includes low-level WAL benchmarks:
//! - WAL append operations (different operation types)
//! - WAL throughput (batch operations)
//! - Sync vs async WAL modes

mod common;

use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
use aletheiadb::{
    AletheiaDB, WalConfigBuilder, WriteOps, WriteOptions,
    core::{PropertyMapBuilder, interning::GLOBAL_INTERNER, temporal::time},
    storage::wal::{
        DurabilityMode, WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
    },
};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Helper to create a database with a specific durability mode.
///
/// Returns both the TempDir guard and the database.
///
/// The tuple order is intentional: benchmark call sites bind as
/// `let (_guard, db) = ...`, so `db` drops before `_guard`. This keeps the
/// WAL directory alive until after the WAL background flush thread has shut
/// down.
fn create_db_with_mode(mode: DurabilityMode) -> (TempDir, AletheiaDB) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = WalConfigBuilder::new()
        .wal_dir(temp_dir.path().to_path_buf())
        .segment_size(10 * 1024 * 1024)
        .unwrap()
        .segments_to_retain(3)
        .unwrap()
        .durability_mode(mode)
        .build();
    let db = AletheiaDB::with_wal_config(config).unwrap();
    (temp_dir, db)
}

/// Benchmark single transaction latency for each mode
///
/// The AsyncBatched benchmarks validate Issue #128's <100µs latency requirement.
/// Expected results: AsyncBatched modes should show 30-80µs average latency.
fn bench_single_transaction_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_transaction_latency");

    // Synchronous (baseline)
    group.bench_function("synchronous", |b| {
        let (_guard, db) = create_db_with_mode(DurabilityMode::Synchronous);
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::Async {
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::group_commit_default());
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::fast());
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::async_batched_default());
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
        let (_guard, db) =
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
            let (_guard, db) = create_db_with_mode(mode);

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
            let (_guard, db) = create_db_with_mode(mode);
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
            let (_guard, db) = create_db_with_mode(DurabilityMode::GroupCommit {
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
            let (_guard, db) = create_db_with_mode(DurabilityMode::GroupCommit {
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::Async {
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::Synchronous);
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::async_batched_default());
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::Synchronous);
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::Async {
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::async_batched_default());
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
        let (_guard, db) = create_db_with_mode(DurabilityMode::async_batched_default());
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
            let (_guard, db) = create_db_with_mode(DurabilityMode::AsyncBatched {
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
            let (_guard, db) = create_db_with_mode(DurabilityMode::AsyncBatched {
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

/// Benchmark the group-commit coordinator's state-mutex acquisition (Issue #3798).
///
/// `register_transaction` is the commit path's single acquisition of the
/// coordinator's state mutex, so it is the measurement that guards the
/// hardened `lock_state` chokepoint against regression: the uncontended case
/// must stay a plain `try_lock` (no clock reads, no counter traffic), and the
/// contended case must not lose throughput to the deadlock instrumentation.
///
/// Sample sizes are deliberately small — this is a regression tripwire run as
/// part of the normal suite, not a tuning study.
fn bench_group_commit_register_transaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_lock_acquisition");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));

    // Uncontended: one thread, no other acquirer. This is the fast path that
    // every commit pays for, so it is the number that must not move.
    group.bench_function("register_transaction_uncontended", |b| {
        let coordinator = GroupCommitCoordinator::with_defaults();
        b.iter(|| {
            black_box(coordinator.register_transaction().unwrap());
        });
    });

    // Contended: 8 threads hammering the same mutex. Thread spawn and the
    // rendezvous are outside the timed region; only the loops are measured.
    const CONTENDED_THREADS: usize = 8;
    group.bench_function(
        BenchmarkId::new("register_transaction_contended", CONTENDED_THREADS),
        |b| {
            b.iter_custom(|iters| {
                let coordinator = Arc::new(GroupCommitCoordinator::with_defaults());
                // Rounded up so every thread runs the same count; at the
                // iteration counts Criterion picks, the overshoot versus
                // `iters` is negligible.
                let per_thread = iters.div_ceil(CONTENDED_THREADS as u64).max(1);
                let barrier = Arc::new(Barrier::new(CONTENDED_THREADS + 1));

                let mut handles = Vec::with_capacity(CONTENDED_THREADS);
                for _ in 0..CONTENDED_THREADS {
                    let coordinator = Arc::clone(&coordinator);
                    let barrier = Arc::clone(&barrier);
                    handles.push(thread::spawn(move || {
                        barrier.wait();
                        for _ in 0..per_thread {
                            black_box(coordinator.register_transaction().unwrap());
                        }
                    }));
                }

                barrier.wait();
                let start = Instant::now();
                for handle in handles {
                    handle.join().unwrap();
                }
                start.elapsed()
            });
        },
    );

    group.finish();
}

/// Benchmark how fast `finish_flush` releases a full batch of waiters (#3798).
///
/// This is the wakeup path #3798 changed: `finish_flush` now drops the state
/// guard *before* `notify_all`, so a woken waiter does not immediately re-block
/// on the notifier's own mutex. Measuring "notify → all 200 waiters have
/// observed the flush" is what makes that thundering herd visible, and keeps it
/// from creeping back.
///
/// The timed region ends when the last waiter reports through an mpsc channel —
/// deliberately **not** when its thread joins. Joining would fold 200 thread
/// teardowns (a scheduler cost that has nothing to do with the wakeup path)
/// into the measurement, so the joins run outside the clock.
fn bench_group_commit_finish_flush_wakeup(c: &mut Criterion) {
    /// Enough waiters to make the herd measurable, small enough that spawning
    /// them per iteration stays cheap.
    const WAITERS: usize = 200;

    let mut group = c.benchmark_group("group_commit_lock_acquisition");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Elements(WAITERS as u64));

    group.bench_function("finish_flush_wakeup_200_waiters", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Default config: the wait_for_flush deadlock timeout is >= 10s,
                // so no waiter can time out while the batch is being set up.
                let coordinator = Arc::new(GroupCommitCoordinator::with_defaults());
                for _ in 0..WAITERS {
                    coordinator.register_transaction().unwrap();
                }
                let epoch = coordinator.current_epoch().unwrap();

                let ready = Arc::new(Barrier::new(WAITERS + 1));
                // Each waiter reports the instant `wait_for_flush` returns, so
                // the timed region can close on "all waiters observed the
                // flush" without waiting for their threads to be torn down.
                let (done_tx, done_rx) = mpsc::channel();
                let mut handles = Vec::with_capacity(WAITERS);
                for _ in 0..WAITERS {
                    let coordinator = Arc::clone(&coordinator);
                    let ready = Arc::clone(&ready);
                    let done_tx = done_tx.clone();
                    handles.push(thread::spawn(move || {
                        ready.wait();
                        let outcome = coordinator.wait_for_flush(epoch);
                        // The receiver outlives every send, so this cannot fail.
                        let _ = done_tx.send(outcome);
                    }));
                }
                // Drop the template sender: only the per-thread clones remain,
                // so a lost waiter surfaces as a disconnect rather than a hang.
                drop(done_tx);

                // Setup, outside the timed region: let the waiters reach the
                // condvar. A straggler that arrives late simply observes the
                // epoch as already flushed and returns immediately.
                ready.wait();
                thread::sleep(Duration::from_millis(20));

                let start = Instant::now();
                let flushing = coordinator.start_flush().unwrap();
                coordinator.finish_flush(flushing, Ok(())).unwrap();
                for _ in 0..WAITERS {
                    done_rx.recv().unwrap().unwrap();
                }
                total += start.elapsed();

                // Teardown, outside the timed region.
                for handle in handles {
                    handle.join().unwrap();
                }
            }
            total
        });
    });

    group.finish();
}

/// Benchmark low-level WAL append performance for different operation types.
fn bench_wal_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_append");

    // Benchmark appending different operation types
    for op_type in &["create_node", "create_edge", "update_node"] {
        group.bench_function(BenchmarkId::from_parameter(op_type), |b| {
            let temp_dir = TempDir::new().unwrap();
            let config = ConcurrentWalSystemConfig::new(temp_dir.path().to_path_buf());
            let wal = ConcurrentWalSystem::new(config).unwrap();

            b.iter(|| {
                let operation = match *op_type {
                    "create_node" => WalOperation::CreateNode {
                        node_id: black_box(aletheiadb::core::id::NodeId::new(1).unwrap()),
                        label: GLOBAL_INTERNER.intern("Person").unwrap(),
                        properties: PropertyMapBuilder::new().build(),
                        valid_from: time::now(),
                        provenance: None,
                    },
                    "create_edge" => WalOperation::CreateEdge {
                        edge_id: black_box(aletheiadb::core::id::EdgeId::new(1).unwrap()),
                        source: aletheiadb::core::id::NodeId::new(1).unwrap(),
                        target: aletheiadb::core::id::NodeId::new(2).unwrap(),
                        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
                        properties: PropertyMapBuilder::new().build(),
                        valid_from: time::now(),
                        provenance: None,
                    },
                    _ => WalOperation::UpdateNode {
                        node_id: aletheiadb::core::id::NodeId::new(1).unwrap(),
                        version_id: aletheiadb::core::id::VersionId::new(2).unwrap(),
                        label: GLOBAL_INTERNER.intern("Person").unwrap(),
                        properties: PropertyMapBuilder::new().build(),
                        valid_from: time::now(),
                        provenance: None,
                    },
                };
                // Guard against sync-mode append_async footgun: in synchronous mode
                // there is no background flusher, so repeated append_async can block
                // on ring-buffer backpressure once full.
                if matches!(wal.durability_mode(), DurabilityMode::Synchronous) {
                    wal.append(operation).unwrap();
                } else {
                    wal.append_async(operation).unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark WAL throughput with batched operations.
/// Fixed: Now uses iter_batched to separate setup from measurement.
fn bench_wal_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_throughput");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("batch_1000_operations", |b| {
        b.iter_batched(
            || {
                // SETUP (not measured)
                let temp_dir = TempDir::new().unwrap();
                let config = ConcurrentWalSystemConfig::new(temp_dir.path().to_path_buf());
                let wal = ConcurrentWalSystem::new(config).unwrap();
                (temp_dir, wal) // Keep temp_dir alive
            },
            |(temp_dir, wal): (TempDir, ConcurrentWalSystem)| {
                // MEASUREMENT (only this is timed)
                let start_lsn = wal.current_lsn();
                for i in 0..1000 {
                    let operation = WalOperation::CreateNode {
                        node_id: aletheiadb::core::id::NodeId::new(i).unwrap(),
                        label: GLOBAL_INTERNER.intern("Person").unwrap(),
                        properties: PropertyMapBuilder::new().build(),
                        valid_from: time::now(),
                        provenance: None,
                    };
                    black_box(wal.append_async(operation).unwrap());
                }
                // Verify we actually appended 1000 operations
                let end_lsn = wal.current_lsn();
                assert_eq!(
                    end_lsn.0 - start_lsn.0,
                    1000,
                    "Expected 1000 operations appended, got {}",
                    end_lsn.0 - start_lsn.0
                );
                drop(temp_dir); // Ensure cleanup
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark WAL with different sync modes (sync vs async).
/// Fixed: Now actually uses different durability modes.
fn bench_wal_with_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_sync_modes");

    for (name, mode) in &[
        (
            "sync_off",
            DurabilityMode::Async {
                flush_interval_ms: 100,
            },
        ),
        ("sync_on", DurabilityMode::Synchronous),
    ] {
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            let temp_dir = TempDir::new().unwrap();
            let config = ConcurrentWalSystemConfig::new(temp_dir.path().to_path_buf())
                .with_durability_mode(*mode);
            let wal = ConcurrentWalSystem::new(config).unwrap();

            b.iter(|| {
                let operation = WalOperation::CreateNode {
                    node_id: aletheiadb::core::id::NodeId::new(1).unwrap(),
                    label: GLOBAL_INTERNER.intern("Person").unwrap(),
                    properties: PropertyMapBuilder::new().build(),
                    valid_from: time::now(),
                    provenance: None,
                };
                wal.append_async(operation).unwrap();
                wal.commit().unwrap(); // ✅ Commit with configured durability mode
            });
        });
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_single_transaction_latency,
    bench_batch_throughput,
    bench_concurrent_writes,
    bench_group_commit_batch_sizes,
    bench_group_commit_delays,
    bench_async_batched_batch_sizes,
    bench_async_batched_delays,
    bench_per_transaction_override,
    bench_mixed_workload,
    bench_group_commit_register_transaction,
    bench_group_commit_finish_flush_wakeup,
    bench_wal_append,
    bench_wal_throughput,
    bench_wal_with_sync,
);
criterion_main!(benches);
