use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::temporal::{BiTemporalInterval, TimeRange};
use gallifreydb::index::temporal::TemporalIndexes;
use std::sync::Arc;
use std::thread;

fn bench_valid_at_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("valid_at_query");

    // Test with 100, 1K, 10K versions per entity
    for version_count in [100, 1000, 10000] {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        // Insert sequential versions
        // Each version is valid for 1000 ticks
        for i in 0..version_count {
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

        // Query for a time in the middle of the history
        let query_time = (version_count / 2 * 1000) + 500; // Middle of a version

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_versions", version_count)),
            &query_time,
            |b, &time| {
                b.iter(|| {
                    // New efficient query: "valid at time T"
                    // Since the index now supports overlaps, we can just query the point interval.
                    let range = TimeRange::new(time, time + 1);
                    let valid = indexes.find_node_versions_in_valid_time_range(node_id, range);
                    black_box(valid)
                });
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

criterion_group!(
    benches,
    bench_valid_at_query,
    bench_insert_performance,
    bench_concurrent_write_throughput,
    bench_read_latency_under_write_contention
);
criterion_main!(benches);
