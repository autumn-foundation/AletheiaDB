//! Benchmarks for temporal index internals and concurrent access
//!
//! Covers:
//! - Temporal B-Tree operations
//! - Time-point and time-range queries
//! - Concurrent read/write patterns
//! - Cache effectiveness

mod common;

use aletheiadb::AletheiaDB;
use aletheiadb::Error;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};
use aletheiadb::index::temporal::TemporalIndexes;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

fn bench_valid_at_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("valid_at_query");

    // Test with 100, 1K, 10K versions per entity
    for version_count in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_versions", version_count)),
            &version_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        // Setup: Create fresh index (isolated from measurement)
                        let indexes = TemporalIndexes::new();
                        let node_id = NodeId::new(1).unwrap();

                        // Insert sequential versions
                        // Each version is valid for 1000 ticks
                        for i in 0..count {
                            let start = i * 1000;
                            let end = (i + 1) * 1000;
                            let v_id = VersionId::new(i as u64).unwrap();

                            indexes
                                .insert_node_version(
                                    node_id,
                                    v_id,
                                    BiTemporalInterval::new(
                                        TimeRange::new(start.into(), end.into()).unwrap(),
                                        TimeRange::from(0.into()), // Tx time is irrelevant for this test
                                    ),
                                )
                                .unwrap();
                        }

                        // Query time in the middle of the history
                        let query_time = (count / 2 * 1000) + 500; // Middle of a version
                        (indexes, node_id, query_time)
                    },
                    |(indexes, node_id, time)| {
                        // Benchmark: Query operation only
                        // New efficient query: "valid at time T"
                        // Since the index now supports overlaps, we can just query the point interval.
                        let range = TimeRange::new(time.into(), (time + 1).into()).unwrap();
                        black_box(indexes.find_node_versions_in_valid_time_range(node_id, range))
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_insert_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("temporal_insert");

    // Benchmark 1: Append-only inserts (common case - chronological)
    for version_count in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::new("append_only", version_count),
            &version_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        // Setup: Create fresh indexes
                        (TemporalIndexes::new(), NodeId::new(1).unwrap())
                    },
                    |(indexes, node_id)| {
                        // Benchmark: Insert chronologically
                        for i in 0..count {
                            let start = i * 1000;
                            let end = (i + 1) * 1000;
                            let v_id = VersionId::new(i as u64).unwrap();
                            indexes
                                .insert_node_version(
                                    node_id,
                                    v_id,
                                    BiTemporalInterval::new(
                                        TimeRange::new(start.into(), end.into()).unwrap(),
                                        TimeRange::from(0.into()),
                                    ),
                                )
                                .unwrap();
                        }
                        black_box(indexes)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    // Benchmark 2: Retroactive inserts (worst case - random order)
    for version_count in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::new("retroactive", version_count),
            &version_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        // Setup: Create fresh indexes and random order
                        use rand::seq::SliceRandom;
                        let mut rng = rand::thread_rng();
                        let mut order: Vec<i64> = (0..count).collect();
                        order.shuffle(&mut rng);
                        (TemporalIndexes::new(), NodeId::new(1).unwrap(), order)
                    },
                    |(indexes, node_id, order)| {
                        // Benchmark: Insert in random order (retroactive)
                        for &i in &order {
                            let start = i * 1000;
                            let end = (i + 1) * 1000;
                            let v_id = VersionId::new(i as u64).unwrap();
                            indexes
                                .insert_node_version(
                                    node_id,
                                    v_id,
                                    BiTemporalInterval::new(
                                        TimeRange::new(start.into(), end.into()).unwrap(),
                                        TimeRange::from(0.into()),
                                    ),
                                )
                                .unwrap();
                        }
                        black_box(indexes)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_concurrent_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_writes");

    // Benchmark concurrent writes to different entities (optimal case)
    for num_threads in [2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("different_entities", num_threads),
            &num_threads,
            |b, &threads| {
                b.iter_batched(
                    || Arc::new(TemporalIndexes::new()),
                    |indexes| {
                        let handles: Vec<_> = (0..threads)
                            .map(|thread_id| {
                                let idx = Arc::clone(&indexes);
                                thread::spawn(move || {
                                    let node_id = NodeId::new(thread_id + 1).unwrap();
                                    for v in 0..100 {
                                        let version_id =
                                            VersionId::new(thread_id * 100 + v).unwrap();
                                        idx.insert_node_version(
                                            node_id,
                                            version_id,
                                            BiTemporalInterval::new(
                                                TimeRange::new(
                                                    ((v * 1000) as i64).into(),
                                                    (((v + 1) * 1000) as i64).into(),
                                                )
                                                .unwrap(),
                                                TimeRange::from(0.into()),
                                            ),
                                        )
                                        .unwrap();
                                    }
                                })
                            })
                            .collect();

                        for h in handles {
                            h.join().unwrap();
                        }
                        black_box(indexes)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    // Benchmark concurrent writes to the same entity (contention case)
    for num_threads in [2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("same_entity_contention", num_threads),
            &num_threads,
            |b, &threads| {
                b.iter_batched(
                    || Arc::new(TemporalIndexes::new()),
                    |indexes| {
                        let node_id = NodeId::new(1).unwrap(); // Same entity
                        let handles: Vec<_> = (0..threads)
                            .map(|thread_id| {
                                let idx = Arc::clone(&indexes);
                                thread::spawn(move || {
                                    for v in 0..100 {
                                        let version_id =
                                            VersionId::new(thread_id * 100 + v).unwrap();
                                        idx.insert_node_version(
                                            node_id,
                                            version_id,
                                            BiTemporalInterval::new(
                                                TimeRange::new(
                                                    (((thread_id * 100 + v) * 1000) as i64).into(),
                                                    ((((thread_id * 100 + v) + 1) * 1000) as i64)
                                                        .into(),
                                                )
                                                .unwrap(),
                                                TimeRange::from(0.into()),
                                            ),
                                        )
                                        .unwrap();
                                    }
                                })
                            })
                            .collect();

                        for h in handles {
                            h.join().unwrap();
                        }
                        black_box(indexes)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_read_latency_under_write_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_under_write_contention");

    // Test read performance while writes are happening to the same entity
    for num_writers in [0, 2, 4] {
        group.bench_with_input(
            BenchmarkId::new("concurrent_writers", num_writers),
            &num_writers,
            |b, &writers| {
                // Setup: Pre-populate with 10K versions
                let indexes = Arc::new(TemporalIndexes::new());
                let node_id = NodeId::new(1).unwrap();
                for i in 0..10_000 {
                    let version_id = VersionId::new(i).unwrap();
                    indexes
                        .insert_node_version(
                            node_id,
                            version_id,
                            BiTemporalInterval::new(
                                TimeRange::new(
                                    ((i * 1000) as i64).into(),
                                    (((i + 1) * 1000) as i64).into(),
                                )
                                .unwrap(),
                                TimeRange::from(0.into()),
                            ),
                        )
                        .unwrap();
                }

                // Query range in the middle
                let query_range = TimeRange::new(5_000_000.into(), 5_001_000.into()).unwrap();

                b.iter_batched(
                    || {
                        // Spawn background writers
                        let writer_handles: Vec<_> = (0..writers)
                            .map(|thread_id| {
                                let idx = Arc::clone(&indexes);
                                thread::spawn(move || {
                                    // Each writer inserts 100 versions retroactively
                                    for v in 0..100 {
                                        let version_id =
                                            VersionId::new(10_000 + thread_id * 100 + v).unwrap();
                                        idx.insert_node_version(
                                            node_id,
                                            version_id,
                                            BiTemporalInterval::new(
                                                TimeRange::new(
                                                    (((thread_id * 100 + v) * 1000) as i64).into(),
                                                    ((((thread_id * 100 + v) + 1) * 1000) as i64)
                                                        .into(),
                                                )
                                                .unwrap(),
                                                TimeRange::from(0.into()),
                                            ),
                                        )
                                        .unwrap();
                                    }
                                })
                            })
                            .collect();
                        (Arc::clone(&indexes), writer_handles)
                    },
                    |(idx, handles)| {
                        // Benchmark: Read query while writers are active
                        let results =
                            idx.find_node_versions_in_valid_time_range(node_id, query_range);

                        // Wait for writers to finish
                        for h in handles {
                            h.join().unwrap();
                        }

                        black_box(results)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_concurrent_read_same_entity(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_reads_same_entity");

    // This benchmark measures the scenario where RwLock would provide benefit:
    // Multiple threads reading the same entity's timeline concurrently.
    // With current DashMap implementation, readers contend on DashMap's internal shard lock.
    // With RwLock pattern (DashMap<EntityId, Arc<RwLock<EntityTimelines>>>),
    // multiple readers could access the timeline concurrently.

    // Setup: Pre-populate with varying history sizes
    for version_count in [100, 1000, 10000] {
        let indexes = Arc::new(TemporalIndexes::new());
        let node_id = NodeId::new(1).unwrap();

        // Insert versions
        for i in 0..version_count {
            let version_id = VersionId::new(i).unwrap();
            indexes
                .insert_node_version(
                    node_id,
                    version_id,
                    BiTemporalInterval::new(
                        TimeRange::new(
                            ((i * 1000) as i64).into(),
                            (((i + 1) * 1000) as i64).into(),
                        )
                        .unwrap(),
                        TimeRange::from(0.into()),
                    ),
                )
                .unwrap();
        }

        // Benchmark concurrent reads with varying thread counts
        for num_readers in [2, 4, 8, 16] {
            group.bench_with_input(
                BenchmarkId::new(
                    format!("{}_versions", version_count),
                    format!("{}_readers", num_readers),
                ),
                &(num_readers, Arc::clone(&indexes)),
                |b, (readers, idx)| {
                    b.iter(|| {
                        // Spawn multiple reader threads querying the same entity
                        let handles: Vec<_> = (0..*readers)
                            .map(|reader_id| {
                                let idx_clone = Arc::clone(idx);
                                thread::spawn(move || {
                                    // Each reader performs 50 queries
                                    let mut results = Vec::new();
                                    for q in 0..50 {
                                        // Query different time points across the history
                                        let time = ((reader_id * 50 + q) * 1000) as i64;
                                        let range =
                                            TimeRange::new(time.into(), (time + 1).into()).unwrap();
                                        let r = idx_clone
                                            .find_node_versions_in_valid_time_range(node_id, range);
                                        results.push(r);
                                    }
                                    results
                                })
                            })
                            .collect();

                        // Wait for all readers
                        let all_results: Vec<_> =
                            handles.into_iter().map(|h| h.join().unwrap()).collect();

                        black_box(all_results)
                    })
                },
            );
        }
    }

    group.finish();
}

fn bench_mixed_read_write_same_entity(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_read_write_same_entity");

    // This benchmark simulates LLM reasoning queries:
    // - Many concurrent readers (LLM queries)
    // - Occasional writers (new knowledge updates)
    // This is where RwLock would shine: readers don't block each other.

    for num_readers in [4, 8] {
        for num_writers in [0, 1, 2] {
            let indexes = Arc::new(TemporalIndexes::new());
            let node_id = NodeId::new(1).unwrap();

            // Pre-populate with 1000 versions
            for i in 0..1000 {
                let version_id = VersionId::new(i).unwrap();
                indexes
                    .insert_node_version(
                        node_id,
                        version_id,
                        BiTemporalInterval::new(
                            TimeRange::new(
                                ((i * 1000) as i64).into(),
                                (((i + 1) * 1000) as i64).into(),
                            )
                            .unwrap(),
                            TimeRange::from(0.into()),
                        ),
                    )
                    .unwrap();
            }

            group.bench_with_input(
                BenchmarkId::new(
                    format!("{}_readers", num_readers),
                    format!("{}_writers", num_writers),
                ),
                &(num_readers, num_writers, Arc::clone(&indexes)),
                |b, (readers, writers, idx)| {
                    b.iter(|| {
                        let mut reader_handles = Vec::new();
                        let mut writer_handles = Vec::new();

                        // Spawn reader threads
                        for reader_id in 0..*readers {
                            let idx_clone = Arc::clone(idx);
                            reader_handles.push(thread::spawn(move || {
                                let mut results = Vec::new();
                                for q in 0..25 {
                                    let time = ((reader_id * 25 + q) * 1000) as i64;
                                    let range =
                                        TimeRange::new(time.into(), (time + 1).into()).unwrap();
                                    let r = idx_clone
                                        .find_node_versions_in_valid_time_range(node_id, range);
                                    results.push(r);
                                }
                                results
                            }));
                        }

                        // Spawn writer threads
                        for writer_id in 0..*writers {
                            let idx_clone = Arc::clone(idx);
                            writer_handles.push(thread::spawn(move || {
                                for w in 0..10 {
                                    let version_id =
                                        VersionId::new(1000 + writer_id * 10 + w).unwrap();
                                    idx_clone
                                        .insert_node_version(
                                            node_id,
                                            version_id,
                                            BiTemporalInterval::new(
                                                TimeRange::new(
                                                    (((1000 + writer_id * 10 + w) * 1000) as i64)
                                                        .into(),
                                                    (((1001 + writer_id * 10 + w) * 1000) as i64)
                                                        .into(),
                                                )
                                                .unwrap(),
                                                TimeRange::from(0.into()),
                                            ),
                                        )
                                        .unwrap();
                                }
                            }));
                        }

                        // Wait for all threads
                        for h in reader_handles {
                            h.join().unwrap();
                        }
                        for h in writer_handles {
                            h.join().unwrap();
                        }

                        black_box(())
                    })
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Concurrent Time-Travel Reads Benchmarks
// ============================================================================

/// Benchmark concurrent time-travel reads with varying concurrency levels.
fn bench_concurrent_time_travel_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_time_travel_reads");

    // Setup: Create database ONCE.
    // We wrap it in Arc here so we can clone the reference into threads cheaply.
    let db = Arc::new(setup_database_with_versions(100)); // 100 nodes is enough for this

    for num_threads in [1, 2, 4, 8, 10] {
        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                // OPTIMIZATION 1: Create the thread pool OUTSIDE the measurement loop.
                // We only want to measure the query time, not OS thread spawning time.
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(num_threads)
                    .build()
                    .unwrap();

                b.iter(|| {
                    pool.install(|| {
                        rayon::scope(|s| {
                            for _ in 0..num_threads {
                                let db_clone = Arc::clone(&db);
                                s.spawn(move |_| {
                                    // Each thread performs 25 queries (total work per iter scales with threads)
                                    for i in 0..25 {
                                        let node_id =
                                            aletheiadb::core::id::NodeId::new((i % 100) as u64 + 1)
                                                .unwrap();
                                        // Pick a valid timestamp
                                        let timestamp = 1000 + (i as i64 * 100);

                                        let result = db_clone.get_node_at_time(
                                            node_id,
                                            timestamp.into(),
                                            timestamp.into(),
                                        );

                                        let _ = black_box(result);
                                    }
                                });
                            }
                        });
                    });
                });
            },
        );
    }

    group.finish();
}

/// Benchmark cache hit rate by reading the same version repeatedly.
fn bench_cache_hit_rate(c: &mut Criterion) {
    // Setup ONCE
    let db = setup_database_with_versions(10);
    let node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
    let timestamp = 1000;

    // Warm up the cache manually (optional, but ensures we measure hits)
    let _ = db.get_node_at_time(node_id, timestamp.into(), timestamp.into());

    c.bench_function("time_travel_cache_hit", |b| {
        b.iter(|| {
            // Hot path: This should be ~50ns
            let result = db.get_node_at_time(
                black_box(node_id),
                black_box(timestamp.into()),
                black_box(timestamp.into()),
            );
            black_box(result)
        })
    });
}

/// Benchmark cache miss (first-time reconstruction).
fn bench_cache_miss(c: &mut Criterion) {
    // OPTIMIZATION 2: The "Infinite Corridor" Strategy
    // Instead of rebuilding the DB 100 times, build ONE DB with 10,000 nodes.
    // In the loop, read Node 1, then Node 2, then Node 3...
    // Since we never repeat a node, every read is a cache miss.
    let node_count = 10_000;
    let db = setup_database_with_versions(node_count);

    // Atomic counter to pick a unique node each iteration
    let counter = AtomicU64::new(1);

    c.bench_function("time_travel_cache_miss", |b| {
        b.iter(|| {
            // Get unique ID (mod count to be safe, but ideally we don't wrap)
            let i = counter.fetch_add(1, Ordering::Relaxed);
            let id_to_read = (i % node_count as u64) + 1;

            let node_id = aletheiadb::core::id::NodeId::new(id_to_read).unwrap();
            let timestamp = 1000; // First version

            let result = db.get_node_at_time(
                black_box(node_id),
                black_box(timestamp.into()),
                black_box(timestamp.into()),
            );
            black_box(result)
        })
    });
}

/// Setup helper: Create a database with `count` versioned nodes.
fn setup_database_with_versions(count: usize) -> AletheiaDB {
    let db = AletheiaDB::new().unwrap();

    // Batch writes if possible, otherwise individual
    for i in 0..count {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i).as_str())
            .insert("value", i as i64)
            .build();

        let node_id = db.create_node("TestNode", props).unwrap();

        // Create versions
        for version in 1..3 {
            // Reduced versions to speed up setup, still sufficient for testing
            let updated_props = PropertyMapBuilder::new()
                .insert("name", format!("Node_{}", i).as_str())
                .insert("value", (i * 10 + version) as i64)
                .insert("version", version as i64)
                .build();

            db.write(|tx| {
                tx.update_node(node_id, updated_props.clone())?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
    }

    db
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_valid_at_query,
    bench_insert_performance,
    bench_concurrent_write_throughput,
    bench_read_latency_under_write_contention,
    bench_concurrent_read_same_entity,
    bench_mixed_read_write_same_entity,
    bench_concurrent_time_travel_reads,
    bench_cache_hit_rate,
    bench_cache_miss
);
criterion_main!(benches);
