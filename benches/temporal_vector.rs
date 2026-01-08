//! Benchmarks for temporal vector operations (Phase 3).
//!
//! Measures performance of:
//! - Snapshot creation with different strategies
//! - Point-in-time vector queries
//! - Time-range vector queries
//! - Snapshot pruning
//! - Temporal vector index overhead vs current-only index

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::NodeId;
use gallifreydb::core::temporal::TimeRange;
use gallifreydb::index::vector::temporal::{
    RetentionPolicy, SnapshotStrategy, TemporalVectorConfig, TemporalVectorIndex,
};
use gallifreydb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorIndex};
use std::sync::Arc;

/// Helper to create a test temporal index
fn create_temporal_index(dimensions: usize) -> TemporalVectorIndex {
    let hnsw_config = HnswConfig::new(dimensions, DistanceMetric::Cosine);
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 10,
        hnsw_config,
    };
    TemporalVectorIndex::new(config).unwrap()
}

/// Helper to generate a vector (avoids allocation noise in bench loops)
fn gen_vector(dim: usize, seed: usize) -> Vec<f32> {
    (0..dim).map(|j| (seed + j) as f32 / 1000.0).collect()
}

/// Benchmark snapshot creation with different strategies
fn bench_snapshot_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_creation");

    for strategy in [
        (
            "transaction_interval_10",
            SnapshotStrategy::TransactionInterval(10),
        ),
        (
            "transaction_interval_100",
            SnapshotStrategy::TransactionInterval(100),
        ),
        ("time_interval_1s", SnapshotStrategy::TimeInterval(1)),
    ] {
        group.bench_with_input(
            BenchmarkId::from_parameter(strategy.0),
            &strategy.1,
            |b, snapshot_strategy| {
                let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
                let config = TemporalVectorConfig {
                    snapshot_strategy: snapshot_strategy.clone(),
                    retention_policy: RetentionPolicy::KeepN(10),
                    max_snapshots: 100,
                    full_snapshot_interval: 10,
                    hnsw_config,
                };
                let index = TemporalVectorIndex::new(config).unwrap();

                // Populate index with some vectors
                for i in 0..100 {
                    let node_id = NodeId::new(i).unwrap();
                    let vector = gen_vector(384, i as usize);
                    let _ = index.add(node_id, &vector, i as i64 * 1000);
                }

                b.iter(|| {
                    let _ = index.on_transaction();
                });
            },
        );
    }

    group.finish();
}

/// Benchmark point-in-time vector queries
fn bench_point_in_time_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("point_in_time_queries");

    for size in [100, 1000, 10000] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}vectors", size)),
            &size,
            |b, &size| {
                let index = create_temporal_index(128);

                // Populate index and create snapshots
                for i in 0..size {
                    let node_id = NodeId::new(i).unwrap();
                    let vector = gen_vector(128, i as usize);
                    let _ = index.add(node_id, &vector, i as i64 * 1000);

                    if i % 100 == 0 {
                        let _ = index.on_transaction();
                    }
                }

                let query_vector: Vec<f32> = gen_vector(128, 0);
                let query_timestamp = (size / 2) as i64 * 1000;

                b.iter(|| {
                    let _ = index.find_similar_as_of(
                        black_box(&query_vector),
                        black_box(10),
                        black_box(query_timestamp),
                    );
                });
            },
        );
    }

    group.finish();
}

/// Benchmark time-range vector queries
fn bench_time_range_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("time_range_queries");
    group.throughput(Throughput::Elements(1));

    for snapshot_count in [5, 10, 20] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}snapshots", snapshot_count)),
            &snapshot_count,
            |b, &snapshot_count| {
                let index = create_temporal_index(128);

                // Create multiple snapshots
                let vectors_per_snapshot = 100;
                for snapshot_idx in 0..snapshot_count {
                    for i in 0..vectors_per_snapshot {
                        let node_id =
                            NodeId::new((snapshot_idx * vectors_per_snapshot + i) as u64).unwrap();
                        let vector = gen_vector(128, (snapshot_idx + i) as usize);
                        let timestamp = (snapshot_idx * 10000 + i * 100) as i64;
                        let _ = index.add(node_id, &vector, timestamp);
                    }
                    let _ = index.on_transaction();
                }

                let query_vector = gen_vector(128, 0);
                let time_range = TimeRange::between(0, (snapshot_count * 10000) as i64);

                b.iter(|| {
                    let _ = index.find_similar_in_range(
                        black_box(&query_vector),
                        black_box(10),
                        black_box(time_range),
                    );
                });
            },
        );
    }

    group.finish();
}

/// Benchmark snapshot pruning
fn bench_snapshot_pruning(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_pruning");

    for snapshot_count in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}snapshots", snapshot_count)),
            &snapshot_count,
            |b, &snapshot_count| {
                let vectors: Vec<Vec<f32>> =
                    (0..snapshot_count).map(|i| gen_vector(128, i)).collect();

                b.iter_batched(
                    || {
                        // Setup: create index with many snapshots
                        let hnsw_config = HnswConfig::new(128, DistanceMetric::Cosine);
                        let config = TemporalVectorConfig {
                            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                            retention_policy: RetentionPolicy::KeepN(snapshot_count / 2),
                            max_snapshots: snapshot_count * 2,
                            full_snapshot_interval: 10,
                            hnsw_config,
                        };
                        let index = TemporalVectorIndex::new(config).unwrap();

                        // Create snapshots
                        for (i, vector) in vectors.iter().enumerate().take(snapshot_count) {
                            let node_id = NodeId::new(i as u64).unwrap();
                            let _ = index.add(node_id, vector, i as i64 * 1000);
                            let _ = index.on_transaction();
                        }

                        index
                    },
                    |index| {
                        // Measure pruning
                        let _ = index.prune_snapshots();
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Compare overhead of temporal index vs current-only index
fn bench_temporal_vs_current_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("temporal_vs_current_overhead");
    group.throughput(Throughput::Elements(1));

    // Benchmark current-only HNSW index (baseline)
    group.bench_function("current_only_add", |b| {
        let index = HnswIndex::new(HnswConfig::new(128, DistanceMetric::Cosine)).unwrap();

        let mut counter = 0u64;
        let vectors: Vec<Vec<f32>> = (0..1000).map(|i| gen_vector(128, i)).collect();
        b.iter(|| {
            let idx = (counter % 1000) as usize;
            let node_id = NodeId::new(counter).unwrap();
            let _ = index.add(black_box(node_id), black_box(&vectors[idx]));
            counter += 1;
        });
    });

    // Benchmark temporal index add (with overhead)
    group.bench_function("temporal_add", |b| {
        let index = create_temporal_index(128);

        let mut counter = 0i64;
        let vectors: Vec<Vec<f32>> = (0..1000).map(|i| gen_vector(128, i)).collect();
        b.iter(|| {
            let idx = (counter % 1000) as usize;
            let node_id = NodeId::new(counter as u64).unwrap();
            let _ = index.add(
                black_box(node_id),
                black_box(&vectors[idx]),
                black_box(counter * 1000),
            );
            counter += 1;
        });
    });

    // Benchmark current-only search
    group.bench_function("current_only_search", |b| {
        let index = Arc::new(HnswIndex::new(HnswConfig::new(128, DistanceMetric::Cosine)).unwrap());

        // Populate index
        for i in 0..1000 {
            let _ = index.add(NodeId::new(i).unwrap(), &gen_vector(128, i as usize));
        }

        let query_vector = gen_vector(128, 0);

        b.iter(|| {
            let _ = index.search(black_box(&query_vector), black_box(10));
        });
    });

    // Benchmark temporal index search (current state)
    group.bench_function("temporal_search_current", |b| {
        let index = create_temporal_index(128);

        // Populate index
        for i in 0..1000 {
            let node_id = NodeId::new(i).unwrap();
            let vector = gen_vector(128, i as usize);
            let _ = index.add(node_id, &vector, i as i64 * 1000);
        }

        let query_vector = gen_vector(128, 0);

        b.iter(|| {
            let _ = index
                .current_index()
                .search(black_box(&query_vector), black_box(10));
        });
    });

    group.finish();
}

/// Benchmark snapshot creation with different vector counts
fn bench_snapshot_creation_by_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_creation_by_size");

    for vector_count in [100, 500, 1000, 5000] {
        group.throughput(Throughput::Elements(vector_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}vectors", vector_count)),
            &vector_count,
            |b, &vector_count| {
                // Pre-allocate vectors
                let vectors: Vec<Vec<f32>> =
                    (0..vector_count).map(|i| gen_vector(128, i)).collect();

                b.iter_batched(
                    || {
                        // Setup: create index with vectors
                        let index = create_temporal_index(128);
                        for (i, vector) in vectors.iter().enumerate().take(vector_count) {
                            let _ =
                                index.add(NodeId::new(i as u64).unwrap(), vector, i as i64 * 1000);
                        }
                        index
                    },
                    |index| {
                        // Measure manual snapshot creation
                        let _ = index.create_manual_snapshot();
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark semantic evolution queries (VS-045)
fn bench_semantic_evolution(c: &mut Criterion) {
    let mut group = c.benchmark_group("semantic_evolution");

    for snapshot_count in [10, 50, 100] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}snapshots", snapshot_count)),
            &snapshot_count,
            |b, &snapshot_count| {
                let index = create_temporal_index(384);

                // Create multiple snapshots with evolving vectors
                let node_id = NodeId::new(42).unwrap();
                for i in 0..snapshot_count {
                    let vector = gen_vector(384, i);
                    let _ = index.add(node_id, &vector, i as i64 * 1000);
                    let _ = index.on_transaction();
                }

                let time_range = TimeRange::between(0, snapshot_count as i64 * 1000);

                b.iter(|| {
                    let _ = index.semantic_evolution(black_box(node_id), black_box(time_range));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark semantic drift tracking with reference vector (VS-045)
fn bench_track_semantic_drift(c: &mut Criterion) {
    let mut group = c.benchmark_group("track_semantic_drift");

    for snapshot_count in [10, 50, 100] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}snapshots", snapshot_count)),
            &snapshot_count,
            |b, &snapshot_count| {
                let index = create_temporal_index(384);

                // Create snapshots with vectors drifting from reference
                let node_id = NodeId::new(42).unwrap();
                for i in 0..snapshot_count {
                    let vector: Vec<f32> = (0..384)
                        .map(|j| {
                            let angle =
                                (i as f32 / snapshot_count as f32) * std::f32::consts::PI / 4.0;
                            if j == 0 {
                                angle.cos()
                            } else if j == 1 {
                                angle.sin()
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    let _ = index.add(node_id, &vector, i as i64 * 1000);
                    let _ = index.on_transaction();
                }

                let reference: Vec<f32> = {
                    let mut v = vec![0.0f32; 384];
                    v[0] = 1.0;
                    v
                };
                let time_range = TimeRange::between(0, snapshot_count as i64 * 1000);

                b.iter(|| {
                    let _ = index.track_semantic_drift(
                        black_box(node_id),
                        black_box(&reference),
                        black_box(time_range),
                    );
                });
            },
        );
    }

    group.finish();
}

/// Benchmark consecutive drift calculation (VS-045)
fn bench_calculate_consecutive_drift(c: &mut Criterion) {
    let mut group = c.benchmark_group("calculate_consecutive_drift");

    for snapshot_count in [10, 50, 100] {
        group.throughput(Throughput::Elements(snapshot_count as u64 - 1)); // N-1 pairs
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}snapshots", snapshot_count)),
            &snapshot_count,
            |b, &snapshot_count| {
                let index = create_temporal_index(384);

                // Create snapshots with gradual rotation
                let node_id = NodeId::new(42).unwrap();
                for i in 0..snapshot_count {
                    let angle = (i as f32 / snapshot_count as f32) * std::f32::consts::PI;
                    let vector: Vec<f32> = (0..384)
                        .map(|j| {
                            if j == 0 {
                                angle.cos()
                            } else if j == 1 {
                                angle.sin()
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    let _ = index.add(node_id, &vector, i as i64 * 1000);
                    let _ = index.on_transaction();
                }

                let time_range = TimeRange::between(0, snapshot_count as i64 * 1000);

                b.iter(|| {
                    let _ = index
                        .calculate_consecutive_drift(black_box(node_id), black_box(time_range));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark memory overhead of semantic evolution vs regular snapshots
fn bench_semantic_evolution_memory_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("semantic_evolution_memory_overhead");

    for vector_count in [100, 500, 1000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}vectors_10snapshots", vector_count)),
            &vector_count,
            |b, &vector_count| {
                // Pre-allocate vectors for all snapshots
                let all_snapshots_vectors: Vec<Vec<Vec<f32>>> = (0..10)
                    .map(|snapshot_idx| {
                        (0..vector_count)
                            .map(|i| gen_vector(384, snapshot_idx + i))
                            .collect()
                    })
                    .collect();

                b.iter_batched(
                    || {
                        // Setup: create index with many vectors across multiple snapshots
                        let index = create_temporal_index(384);

                        // Create 10 snapshots
                        for (snapshot_idx, vectors) in all_snapshots_vectors.iter().enumerate() {
                            for (i, vector) in vectors.iter().enumerate() {
                                let node_id =
                                    NodeId::new((snapshot_idx * vector_count + i) as u64).unwrap();
                                let _ =
                                    index.add(node_id, vector, (snapshot_idx * 1000 + i) as i64);
                            }
                            let _ = index.on_transaction();
                        }

                        index
                    },
                    |index| {
                        // Measure semantic evolution retrieval
                        let node_id = NodeId::new(vector_count as u64 / 2).unwrap();
                        let time_range = TimeRange::between(0, 10000);
                        let _ = index.semantic_evolution(node_id, time_range);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn configure_criterion() -> Criterion {
    let sample_size = std::env::var("BENCH_SAMPLE_SIZE")
        .map(|s| s.parse().unwrap_or(50))
        .unwrap_or(50);

    Criterion::default().sample_size(sample_size)
}

criterion_group!(
    name = benches;
    config = configure_criterion();
    targets = bench_snapshot_creation,
    bench_point_in_time_queries,
    bench_time_range_queries,
    bench_snapshot_pruning,
    bench_temporal_vs_current_overhead,
    bench_snapshot_creation_by_size,
    bench_semantic_evolution,
    bench_track_semantic_drift,
    bench_calculate_consecutive_drift,
    bench_semantic_evolution_memory_overhead,
);
criterion_main!(benches);
