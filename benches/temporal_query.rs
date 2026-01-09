use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::GallifreyDB;
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::{BiTemporalInterval, TimeRange};
use gallifreydb::index::temporal::TemporalIndexes;
use std::sync::Arc;
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
                                        TimeRange::new(start, end),
                                        TimeRange::from(0), // Tx time is irrelevant for this test
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
                        let range = TimeRange::new(time, time + 1);
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
                                        TimeRange::new(start, end),
                                        TimeRange::from(0),
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
                                        TimeRange::new(start, end),
                                        TimeRange::from(0),
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
                                                    (v * 1000) as i64,
                                                    ((v + 1) * 1000) as i64,
                                                ),
                                                TimeRange::from(0),
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
                                                    ((thread_id * 100 + v) * 1000) as i64,
                                                    (((thread_id * 100 + v) + 1) * 1000) as i64,
                                                ),
                                                TimeRange::from(0),
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
                                TimeRange::new((i * 1000) as i64, ((i + 1) * 1000) as i64),
                                TimeRange::from(0),
                            ),
                        )
                        .unwrap();
                }

                // Query range in the middle
                let query_range = TimeRange::new(5_000_000, 5_001_000);

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
                                                    ((thread_id * 100 + v) * 1000) as i64,
                                                    (((thread_id * 100 + v) + 1) * 1000) as i64,
                                                ),
                                                TimeRange::from(0),
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
                        TimeRange::new((i * 1000) as i64, ((i + 1) * 1000) as i64),
                        TimeRange::from(0),
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
                                        let range = TimeRange::new(time, time + 1);
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
                            TimeRange::new((i * 1000) as i64, ((i + 1) * 1000) as i64),
                            TimeRange::from(0),
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
                                    let range = TimeRange::new(time, time + 1);
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
                                                    ((1000 + writer_id * 10 + w) * 1000) as i64,
                                                    ((1001 + writer_id * 10 + w) * 1000) as i64,
                                                ),
                                                TimeRange::from(0),
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

fn bench_batch_vs_loop_time_travel(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_vs_loop_time_travel");

    // Benchmark the primary value proposition: batch API vs loop
    // This validates that get_nodes_at_time is actually faster than N individual calls
    for batch_size in [10, 50, 100, 500] {
        // Setup: Create database with historical data
        let db = GallifreyDB::new();

        // Create nodes with some historical versions
        let node_ids: Vec<NodeId> = (0..batch_size)
            .map(|i| {
                db.create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert("name", format!("Person{}", i))
                        .insert("index", i as i64)
                        .build(),
                )
                .unwrap()
            })
            .collect();

        // Use a timestamp that's guaranteed to be after all creates
        // GallifreyDB uses logical timestamps, not wall-clock time
        let query_time = i64::MAX / 2;

        // Benchmark 1: Batch API (single lock acquisition)
        group.bench_with_input(
            BenchmarkId::new("batch_api", batch_size),
            &batch_size,
            |b, _| {
                b.iter(|| {
                    let results = db
                        .get_nodes_at_time(&node_ids, query_time, query_time)
                        .unwrap();
                    black_box(results)
                });
            },
        );

        // Benchmark 2: Loop with individual calls (N lock acquisitions)
        group.bench_with_input(
            BenchmarkId::new("loop_individual", batch_size),
            &batch_size,
            |b, _| {
                b.iter(|| {
                    let results: Vec<_> = node_ids
                        .iter()
                        .map(|&node_id| db.get_node_at_time(node_id, query_time, query_time).ok())
                        .collect();
                    black_box(results)
                });
            },
        );
    }

    group.finish();
}

fn bench_batch_edges_vs_loop(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_edges_vs_loop");

    // Same benchmark but for edges
    for batch_size in [10, 50, 100, 500] {
        let db = GallifreyDB::new();

        // Create nodes
        let source = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();
        let target = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges
        let edge_ids = (0..batch_size)
            .map(|i| {
                db.create_edge(
                    source,
                    target,
                    "LINK",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        // Use a timestamp that's guaranteed to be after all creates
        // GallifreyDB uses logical timestamps, not wall-clock time
        let query_time = i64::MAX / 2;

        // Benchmark 1: Batch API
        group.bench_with_input(
            BenchmarkId::new("batch_api", batch_size),
            &batch_size,
            |b, _| {
                b.iter(|| {
                    let results = db
                        .get_edges_at_time(&edge_ids, query_time, query_time)
                        .unwrap();
                    black_box(results)
                });
            },
        );

        // Benchmark 2: Loop
        group.bench_with_input(
            BenchmarkId::new("loop_individual", batch_size),
            &batch_size,
            |b, _| {
                b.iter(|| {
                    let results: Vec<_> = edge_ids
                        .iter()
                        .map(|&edge_id| db.get_edge_at_time(edge_id, query_time, query_time).ok())
                        .collect();
                    black_box(results)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_valid_at_query,
    bench_insert_performance,
    bench_concurrent_write_throughput,
    bench_read_latency_under_write_contention,
    bench_concurrent_read_same_entity,
    bench_mixed_read_write_same_entity,
    bench_batch_vs_loop_time_travel,
    bench_batch_edges_vs_loop
);
criterion_main!(benches);
