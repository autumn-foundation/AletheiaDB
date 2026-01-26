//! Benchmarks for HNSW vector index operations.
//!
//! This benchmark suite measures the performance of:
//! - Index creation with various configurations
//! - Single and batch vector insertion
//! - k-NN search with different k values
//! - Filtered search (by label)
//! - HNSW parameter tuning (M, ef_construction, ef_search)

mod common;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::hnsw::{HnswConfig, HnswIndex};
use gallifreydb::index::vector::{DistanceMetric, VectorIndex};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// ============================================================================
// Helper Functions
// ============================================================================

/// Generate a random f32 vector of given dimensions
fn generate_random_vector(dimensions: usize) -> Vec<f32> {
    use rand::Rng as _;
    let mut rng = rand::thread_rng();
    (0..dimensions).map(|_| rng.gen_range(0.0..1.0)).collect()
}

/// Generate a batch of random vectors
fn generate_random_vectors(count: usize, dimensions: usize) -> Vec<Vec<f32>> {
    (0..count)
        .map(|_| generate_random_vector(dimensions))
        .collect()
}

/// Create a pre-populated HNSW index for search benchmarks
fn create_populated_index(
    dimensions: usize,
    metric: DistanceMetric,
    node_count: usize,
    m: usize,
    ef_construction: usize,
) -> HnswIndex {
    let config = HnswConfig::new(dimensions, metric)
        .with_m(m)
        .with_ef_construction(ef_construction)
        .with_capacity(node_count);

    let index = HnswIndex::new(config).expect("Failed to create index");

    // Populate with random vectors
    for i in 0..node_count {
        let node_id = NodeId::new(i as u64).expect("Valid node ID");
        let vector = generate_random_vector(dimensions);
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    index
}

// ============================================================================
// Benchmark: Index Creation
// ============================================================================

fn bench_index_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_creation");

    for &dimensions in &[128, 384, 768, 1536] {
        group.bench_with_input(
            BenchmarkId::new("cosine", dimensions),
            &dimensions,
            |b, &dims| {
                b.iter(|| {
                    let config = HnswConfig::new(dims, DistanceMetric::Cosine);
                    HnswIndex::new(black_box(config))
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Vector Addition (Single)
// ============================================================================

fn bench_vector_addition_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_addition_single");
    // Default sample size via config (10 for PRs, 100 for main)

    for &dimensions in &[128, 384, 768] {
        let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(1000);
        let index = HnswIndex::new(config).expect("Failed to create index");

        // Pre-populate with some vectors to make insertion more realistic
        for i in 0..100 {
            let node_id = NodeId::new(i).expect("Valid node ID");
            let vector = generate_random_vector(dimensions);
            index.add(node_id, &vector).expect("Failed to add vector");
        }

        // Use atomic counter to avoid benchmark contamination from shared mutable state
        let next_id = Arc::new(AtomicU64::new(100));

        group.bench_with_input(
            BenchmarkId::new("dimensions", dimensions),
            &dimensions,
            |b, &dims| {
                let next_id = Arc::clone(&next_id);
                b.iter(|| {
                    let id = next_id.fetch_add(1, Ordering::Relaxed);
                    let node_id = NodeId::new(id).expect("Valid node ID");
                    let vector = generate_random_vector(dims);
                    index.add(black_box(node_id), black_box(&vector))
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Vector Addition (Batch)
// ============================================================================

fn bench_vector_addition_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_addition_batch");
    // Default sample size via config (10 for PRs, 100 for main)

    for &batch_size in &[10, 50, 100, 500] {
        let dimensions = 384;
        let vectors = generate_random_vectors(batch_size, dimensions);

        group.throughput(Throughput::Elements(batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("batch_size", batch_size),
            &batch_size,
            |b, &size| {
                b.iter(|| {
                    let config =
                        HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(size);
                    let index = HnswIndex::new(config).expect("Failed to create index");

                    for (i, vector) in vectors.iter().enumerate() {
                        let node_id = NodeId::new(i as u64).expect("Valid node ID");
                        index.add(black_box(node_id), black_box(vector)).ok();
                    }
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: k-NN Search
// ============================================================================

fn bench_knn_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("knn_search");
    // Configured via configure_criterion()

    let dimensions = 384;
    let index_size = 1000;

    // Create pre-populated index
    let index = create_populated_index(dimensions, DistanceMetric::Cosine, index_size, 16, 128);
    let query_vector = generate_random_vector(dimensions);

    for &k in &[1, 5, 10, 20, 50] {
        group.bench_with_input(BenchmarkId::new("k", k), &k, |b, &k_val| {
            b.iter(|| index.search(black_box(&query_vector), black_box(k_val)));
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: k-NN Search with Different Index Sizes
// ============================================================================

fn bench_knn_search_index_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("knn_search_index_size");
    // Default sample size via config (10 for PRs, 100 for main)

    let dimensions = 384;
    let k = 10;

    for &index_size in &[100, 500, 1000, 5000, 10000] {
        let index = create_populated_index(dimensions, DistanceMetric::Cosine, index_size, 16, 128);
        let query_vector = generate_random_vector(dimensions);

        group.bench_with_input(
            BenchmarkId::new("index_size", index_size),
            &index_size,
            |b, _| {
                b.iter(|| index.search(black_box(&query_vector), black_box(k)));
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: HNSW Parameter Tuning - M (connectivity)
// ============================================================================

fn bench_hnsw_parameter_m(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_param_m");
    // Default sample size via config (10 for PRs, 100 for main)

    let dimensions = 384;
    let index_size = 1000;
    let k = 10;

    for &m in &[8, 16, 32, 64] {
        let index = create_populated_index(dimensions, DistanceMetric::Cosine, index_size, m, 128);
        let query_vector = generate_random_vector(dimensions);

        group.bench_with_input(BenchmarkId::new("M", m), &m, |b, _| {
            b.iter(|| index.search(black_box(&query_vector), black_box(k)));
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: HNSW Parameter Tuning - ef_construction
// ============================================================================

fn bench_hnsw_parameter_ef_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_param_ef_construction");
    // Very reduced sample size (10) for extremely slow operations
    // (building full index with different ef_construction values, extended measurement time)
    // Configured via configure_criterion()

    let dimensions = 384;
    let batch_size = 100;
    let vectors = generate_random_vectors(batch_size, dimensions);

    for &ef_construction in &[64, 128, 200, 400] {
        group.throughput(Throughput::Elements(batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("ef_construction", ef_construction),
            &ef_construction,
            |b, &ef_c| {
                b.iter(|| {
                    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine)
                        .with_ef_construction(ef_c)
                        .with_capacity(batch_size);
                    let index = HnswIndex::new(config).expect("Failed to create index");

                    for (i, vector) in vectors.iter().enumerate() {
                        let node_id = NodeId::new(i as u64).expect("Valid node ID");
                        index.add(black_box(node_id), black_box(vector)).ok();
                    }
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: HNSW Parameter Tuning - ef_search
// ============================================================================

fn bench_hnsw_parameter_ef_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_param_ef_search");

    let dimensions = 384;
    let index_size = 1000;
    let k = 10;

    let index = create_populated_index(dimensions, DistanceMetric::Cosine, index_size, 16, 128);
    let query_vector = generate_random_vector(dimensions);

    for &ef_search in &[10, 50, 100, 200] {
        // Set ef_search parameter
        index.set_ef_search(ef_search);

        group.bench_with_input(
            BenchmarkId::new("ef_search", ef_search),
            &ef_search,
            |b, _| {
                b.iter(|| index.search(black_box(&query_vector), black_box(k)));
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Distance Metrics Comparison
// ============================================================================

fn bench_distance_metrics(c: &mut Criterion) {
    let mut group = c.benchmark_group("distance_metrics");

    let dimensions = 384;
    let index_size = 1000;
    let k = 10;

    for &metric in &[DistanceMetric::Cosine, DistanceMetric::Euclidean] {
        let index = create_populated_index(dimensions, metric, index_size, 16, 128);
        let query_vector = generate_random_vector(dimensions);

        group.bench_with_input(
            BenchmarkId::new("metric", format!("{:?}", metric)),
            &metric,
            |b, _| {
                b.iter(|| index.search(black_box(&query_vector), black_box(k)));
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Filtered Search (Issue #206 - Hot Path Optimization)
// ============================================================================

fn bench_search_with_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_with_filter");

    let dimensions = 384;
    let index_size = 10000; // Large index to exercise the hot path
    let k = 10;

    // Create a populated index
    let index = create_populated_index(dimensions, DistanceMetric::Cosine, index_size, 16, 128);
    let query_vector = generate_random_vector(dimensions);

    // Benchmark 1: Filter accepting all nodes (baseline)
    group.bench_function("filter_accept_all", |b| {
        b.iter(|| index.search_with_filter(black_box(&query_vector), black_box(k), |_| true));
    });

    // Benchmark 2: Filter accepting ~50% of nodes (realistic use case)
    group.bench_function("filter_50_percent", |b| {
        b.iter(|| {
            index.search_with_filter(black_box(&query_vector), black_box(k), |id| {
                id.as_u64() % 2 == 0
            })
        });
    });

    // Benchmark 3: Filter accepting ~10% of nodes (selective filter)
    group.bench_function("filter_10_percent", |b| {
        b.iter(|| {
            index.search_with_filter(black_box(&query_vector), black_box(k), |id| {
                id.as_u64() % 10 == 0
            })
        });
    });

    // Benchmark 4: More complex filter with multiple conditions
    group.bench_function("filter_complex", |b| {
        b.iter(|| {
            index.search_with_filter(black_box(&query_vector), black_box(k), |id| {
                let raw = id.as_u64();
                raw % 2 == 0 && raw < 8000 && raw > 100
            })
        });
    });

    // Benchmark 5: Compare against unfiltered search (baseline)
    group.bench_function("no_filter_baseline", |b| {
        b.iter(|| index.search(black_box(&query_vector), black_box(k)));
    });

    group.finish();
}

// ============================================================================
// Benchmark: Filter Hot Path with Different Index Sizes
// ============================================================================

fn bench_filter_vs_index_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("filter_vs_index_size");

    let dimensions = 384;
    let k = 10;

    for &index_size in &[1000, 5000, 10000, 20000] {
        let index = create_populated_index(dimensions, DistanceMetric::Cosine, index_size, 16, 128);
        let query_vector = generate_random_vector(dimensions);

        group.bench_with_input(
            BenchmarkId::new("index_size", index_size),
            &index_size,
            |b, _| {
                b.iter(|| {
                    index.search_with_filter(black_box(&query_vector), black_box(k), |id| {
                        id.as_u64() % 2 == 0
                    })
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark Groups
// ============================================================================

// ============================================================================
// Benchmark: Vector Updates (Issue #207)
// ============================================================================

/// Benchmark vector updates to validate Issue #207 optimization.
///
/// This benchmark measures the performance of updating existing vectors,
/// which exercises the optimized path that checks if a key exists before
/// calling remove().
fn bench_vector_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_updates");

    for &update_count in &[10, 50, 100, 500] {
        let dimensions = 384;
        let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(1000);
        let index = HnswIndex::new(config).expect("Failed to create index");

        // Pre-populate with initial vectors
        for i in 0..update_count {
            let node_id = NodeId::new(i as u64).expect("Valid node ID");
            let vector = generate_random_vector(dimensions);
            index
                .add(node_id, &vector)
                .expect("Failed to add initial vector");
        }

        // Generate update vectors
        let update_vectors = generate_random_vectors(update_count, dimensions);

        group.throughput(Throughput::Elements(update_count as u64));

        group.bench_with_input(
            BenchmarkId::new("update_count", update_count),
            &update_count,
            |b, &count| {
                b.iter(|| {
                    // Update all vectors
                    for (i, vector) in update_vectors.iter().enumerate().take(count) {
                        let node_id = NodeId::new(i as u64).expect("Valid node ID");
                        index.add(black_box(node_id), black_box(vector)).ok();
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = index_ops;
    config = common::configure_criterion();
    targets = bench_index_creation,
    bench_vector_addition_single,
    bench_vector_addition_batch,
    bench_vector_updates,
);

criterion_group!(
    name = search_ops;
    config = common::configure_criterion();
    targets = bench_knn_search,
    bench_knn_search_index_size,
);

criterion_group!(
    name = tuning_ops;
    config = common::configure_criterion();
    targets = bench_hnsw_parameter_m,
    bench_hnsw_parameter_ef_construction,
    bench_hnsw_parameter_ef_search,
    bench_distance_metrics,
);

criterion_group!(
    name = filter_ops;
    config = common::configure_criterion();
    targets = bench_search_with_filter,
    bench_filter_vs_index_size,
);

criterion_main!(index_ops, search_ops, tuning_ops, filter_ops);
