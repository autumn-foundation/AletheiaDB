//! Benchmarks for HNSW vector index operations.
//!
//! This benchmark suite measures the performance of:
//! - Index creation with various configurations
//! - Single and batch vector insertion
//! - k-NN search with different k values
//! - Filtered search (by label)
//! - HNSW parameter tuning (M, ef_construction, ef_search)

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::hnsw::{HnswConfig, HnswIndex};
use gallifreydb::index::vector::{DistanceMetric, VectorIndex};
use std::time::Duration;

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
    group.sample_size(50); // Reduce sample size for slower operations

    for &dimensions in &[128, 384, 768] {
        let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(1000);
        let index = HnswIndex::new(config).expect("Failed to create index");

        // Pre-populate with some vectors to make insertion more realistic
        for i in 0..100 {
            let node_id = NodeId::new(i).expect("Valid node ID");
            let vector = generate_random_vector(dimensions);
            index.add(node_id, &vector).expect("Failed to add vector");
        }

        let mut next_id = 100u64;

        group.bench_with_input(
            BenchmarkId::new("dimensions", dimensions),
            &dimensions,
            |b, &dims| {
                b.iter(|| {
                    let node_id = NodeId::new(next_id).expect("Valid node ID");
                    next_id += 1;
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
    group.sample_size(20); // Reduce for batch operations

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
    group.measurement_time(Duration::from_secs(10));

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
    group.sample_size(30);

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
    group.sample_size(20);

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
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

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
// Benchmark Groups
// ============================================================================

criterion_group!(
    index_ops,
    bench_index_creation,
    bench_vector_addition_single,
    bench_vector_addition_batch,
);

criterion_group!(search_ops, bench_knn_search, bench_knn_search_index_size,);

criterion_group!(
    parameter_tuning,
    bench_hnsw_parameter_m,
    bench_hnsw_parameter_ef_construction,
    bench_hnsw_parameter_ef_search,
    bench_distance_metrics,
);

criterion_main!(index_ops, search_ops, parameter_tuning);
