//! Benchmarks for vector similarity and distance operations.
//!
//! These benchmarks test the performance of cosine similarity and Euclidean
//! distance calculations at various vector dimensions commonly used in
//! embedding models.
//!
//! Common embedding dimensions:
//! - 384: Sentence Transformers (all-MiniLM)
//! - 768: BERT base, BGE models
//! - 1024: Cohere embed-v3
//! - 1536: OpenAI text-embedding-3-small
//! - 3072: OpenAI text-embedding-3-large

mod common;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::vector::{
    cosine_similarity, cosine_similarity_normalized, dot_product, euclidean_distance,
    squared_euclidean_distance,
};

/// Generate a test vector with deterministic values.
fn generate_vector(dim: usize, seed: usize) -> Vec<f32> {
    (0..dim)
        .map(|i| ((i + seed) as f32 / dim as f32) * 2.0 - 1.0)
        .collect()
}

/// Single-pass scalar implementation (same algorithm, no SIMD).
/// This measures pure SIMD benefit without algorithmic differences.
fn cosine_similarity_scalar_1pass(a: &[f32], b: &[f32]) -> f32 {
    let (dot, mag_a_sq, mag_b_sq) = a
        .iter()
        .zip(b.iter())
        .fold((0.0f32, 0.0f32, 0.0f32), |(d, ma, mb), (&ai, &bi)| {
            (d + ai * bi, ma + ai * ai, mb + bi * bi)
        });

    let magnitude = (mag_a_sq * mag_b_sq).sqrt();
    if magnitude == 0.0 {
        0.0
    } else {
        (dot / magnitude).clamp(-1.0, 1.0)
    }
}

/// Truly naive 3-pass implementation for comparison.
/// Makes 3 separate passes over the data (dot product, mag_a, mag_b).
/// This shows the combined benefit of SIMD + single-pass algorithm.
fn cosine_similarity_naive_3pass(a: &[f32], b: &[f32]) -> f32 {
    // Pass 1: dot product
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    // Pass 2: magnitude of a
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    // Pass 3: magnitude of b
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        (dot / (mag_a * mag_b)).clamp(-1.0, 1.0)
    }
}

/// Benchmark cosine similarity at various embedding dimensions.
///
/// Compares three implementations:
/// - `optimized`: SIMD + single-pass algorithm (our implementation)
/// - `scalar_1pass`: Single-pass algorithm without SIMD (measures SIMD benefit)
/// - `naive_3pass`: 3 separate passes (measures combined SIMD + algorithmic benefit)
fn bench_cosine_similarity_dimensions(c: &mut Criterion) {
    let mut group = c.benchmark_group("cosine_similarity");

    // Common embedding dimensions
    let dimensions = [384, 768, 1024, 1536, 3072];

    for dim in dimensions {
        let a = generate_vector(dim, 0);
        let b = generate_vector(dim, 42);

        group.throughput(Throughput::Elements(dim as u64));

        // Our optimized SIMD implementation
        group.bench_with_input(BenchmarkId::new("optimized", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity(black_box(&a), black_box(&b)).unwrap());
        });

        // Single-pass scalar (same algorithm, no SIMD) - measures pure SIMD benefit
        group.bench_with_input(BenchmarkId::new("scalar_1pass", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity_scalar_1pass(black_box(&a), black_box(&b)));
        });

        // Naive 3-pass scalar - measures combined SIMD + cache efficiency benefit
        group.bench_with_input(BenchmarkId::new("naive_3pass", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity_naive_3pass(black_box(&a), black_box(&b)));
        });
    }

    group.finish();
}

/// Benchmark the specific case of OpenAI embeddings (1536 dimensions).
fn bench_cosine_similarity_openai(c: &mut Criterion) {
    // OpenAI text-embedding-3-small dimension
    let dim = 1536;
    let a = generate_vector(dim, 0);
    let b = generate_vector(dim, 42);

    c.bench_function("cosine_similarity_openai_1536d", |bencher| {
        bencher.iter(|| cosine_similarity(black_box(&a), black_box(&b)).unwrap());
    });
}

/// Benchmark with normalized (unit) vectors.
/// Compares the general function vs the specialized normalized function.
/// This demonstrates the ~2x speedup from skipping magnitude computation.
fn bench_cosine_similarity_normalized(c: &mut Criterion) {
    let mut group = c.benchmark_group("cosine_similarity_normalized");

    let dimensions = [384, 1536];

    for dim in dimensions {
        // Generate and normalize vectors
        let a_raw = generate_vector(dim, 0);
        let b_raw = generate_vector(dim, 42);

        let mag_a = a_raw.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mag_b = b_raw.iter().map(|x| x * x).sum::<f32>().sqrt();

        let a: Vec<f32> = a_raw.iter().map(|x| x / mag_a).collect();
        let b: Vec<f32> = b_raw.iter().map(|x| x / mag_b).collect();

        group.throughput(Throughput::Elements(dim as u64));

        // General function (computes magnitudes even though vectors are normalized)
        group.bench_with_input(BenchmarkId::new("general", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity(black_box(&a), black_box(&b)).unwrap());
        });

        // Specialized function (skips magnitude computation for unit vectors)
        group.bench_with_input(BenchmarkId::new("specialized", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity_normalized(black_box(&a), black_box(&b)).unwrap());
        });
    }

    group.finish();
}

/// Benchmark batch similarity computations.
/// Simulates finding the most similar vector in a collection.
fn bench_cosine_similarity_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("cosine_similarity_batch");

    let dim = 384; // Sentence Transformers dimension
    let batch_sizes = [10, 100, 1000];

    let query = generate_vector(dim, 0);

    for batch_size in batch_sizes {
        let vectors: Vec<Vec<f32>> = (0..batch_size)
            .map(|i| generate_vector(dim, i + 1))
            .collect();

        group.throughput(Throughput::Elements((batch_size * dim) as u64));

        group.bench_with_input(
            BenchmarkId::new("find_most_similar", batch_size),
            &batch_size,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        vectors
                            .iter()
                            .map(|v| cosine_similarity(black_box(&query), black_box(v)).unwrap())
                            .fold(f32::NEG_INFINITY, f32::max),
                    )
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Euclidean Distance Benchmarks
// ============================================================================

/// Scalar fallback for computing sum of squared differences.
/// Used as a baseline for SIMD comparison.
fn squared_diff_sum_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(&ai, &bi)| {
            let diff = ai - bi;
            diff * diff
        })
        .sum()
}

/// Benchmark Euclidean distance at various embedding dimensions.
///
/// Compares three implementations:
/// - `optimized`: SIMD implementation (our implementation)
/// - `squared_optimized`: Squared distance (avoids sqrt)
/// - `scalar`: Scalar fallback for comparison
fn bench_euclidean_distance_dimensions(c: &mut Criterion) {
    let mut group = c.benchmark_group("euclidean_distance");

    // Common embedding dimensions
    let dimensions = [384, 768, 1024, 1536, 3072];

    for dim in dimensions {
        let a = generate_vector(dim, 0);
        let b = generate_vector(dim, 42);

        group.throughput(Throughput::Elements(dim as u64));

        // Our optimized SIMD implementation
        group.bench_with_input(BenchmarkId::new("optimized", dim), &dim, |bencher, _| {
            bencher.iter(|| euclidean_distance(black_box(&a), black_box(&b)).unwrap());
        });

        // Squared distance (avoids sqrt overhead)
        group.bench_with_input(
            BenchmarkId::new("squared_optimized", dim),
            &dim,
            |bencher, _| {
                bencher.iter(|| squared_euclidean_distance(black_box(&a), black_box(&b)).unwrap());
            },
        );

        // Scalar fallback for comparison
        group.bench_with_input(BenchmarkId::new("scalar", dim), &dim, |bencher, _| {
            bencher.iter(|| squared_diff_sum_scalar(black_box(&a), black_box(&b)).sqrt());
        });
    }

    group.finish();
}

/// Benchmark the specific case of OpenAI embeddings (1536 dimensions).
fn bench_euclidean_distance_openai(c: &mut Criterion) {
    // OpenAI text-embedding-3-small dimension
    let dim = 1536;
    let a = generate_vector(dim, 0);
    let b = generate_vector(dim, 42);

    c.bench_function("euclidean_distance_openai_1536d", |bencher| {
        bencher.iter(|| euclidean_distance(black_box(&a), black_box(&b)).unwrap());
    });
}

/// Benchmark squared vs non-squared distance.
/// This demonstrates the benefit of using squared distance for comparisons.
fn bench_squared_vs_euclidean(c: &mut Criterion) {
    let mut group = c.benchmark_group("squared_vs_euclidean");

    let dimensions = [384, 1536];

    for dim in dimensions {
        let a = generate_vector(dim, 0);
        let b = generate_vector(dim, 42);

        group.throughput(Throughput::Elements(dim as u64));

        // Full Euclidean distance (includes sqrt)
        group.bench_with_input(BenchmarkId::new("euclidean", dim), &dim, |bencher, _| {
            bencher.iter(|| euclidean_distance(black_box(&a), black_box(&b)).unwrap());
        });

        // Squared distance (skips sqrt - faster for comparisons)
        group.bench_with_input(BenchmarkId::new("squared", dim), &dim, |bencher, _| {
            bencher.iter(|| squared_euclidean_distance(black_box(&a), black_box(&b)).unwrap());
        });
    }

    group.finish();
}

/// Benchmark batch distance computations.
/// Simulates finding the nearest neighbor in a collection.
fn bench_euclidean_distance_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("euclidean_distance_batch");

    let dim = 384; // Sentence Transformers dimension
    let batch_sizes = [10, 100, 1000];

    let query = generate_vector(dim, 0);

    for batch_size in batch_sizes {
        let vectors: Vec<Vec<f32>> = (0..batch_size)
            .map(|i| generate_vector(dim, i + 1))
            .collect();

        group.throughput(Throughput::Elements((batch_size * dim) as u64));

        // Using squared distance for comparisons (more efficient)
        group.bench_with_input(
            BenchmarkId::new("find_nearest_squared", batch_size),
            &batch_size,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        vectors
                            .iter()
                            .map(|v| {
                                squared_euclidean_distance(black_box(&query), black_box(v)).unwrap()
                            })
                            .fold(f32::INFINITY, f32::min),
                    )
                });
            },
        );

        // Using full distance
        group.bench_with_input(
            BenchmarkId::new("find_nearest_full", batch_size),
            &batch_size,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        vectors
                            .iter()
                            .map(|v| euclidean_distance(black_box(&query), black_box(v)).unwrap())
                            .fold(f32::INFINITY, f32::min),
                    )
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Dot Product Benchmarks
// ============================================================================

/// Scalar implementation of dot product for comparison.
fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(&ai, &bi)| ai * bi).sum()
}

/// Benchmark dot product at various embedding dimensions.
///
/// Compares two implementations:
/// - `optimized`: SIMD implementation (our implementation)
/// - `scalar`: Scalar fallback for comparison
fn bench_dot_product_dimensions(c: &mut Criterion) {
    let mut group = c.benchmark_group("dot_product");

    // Common embedding dimensions
    let dimensions = [384, 768, 1024, 1536, 3072];

    for dim in dimensions {
        let a = generate_vector(dim, 0);
        let b = generate_vector(dim, 42);

        group.throughput(Throughput::Elements(dim as u64));

        // Our optimized SIMD implementation
        group.bench_with_input(BenchmarkId::new("optimized", dim), &dim, |bencher, _| {
            bencher.iter(|| dot_product(black_box(&a), black_box(&b)).unwrap());
        });

        // Scalar fallback for comparison
        group.bench_with_input(BenchmarkId::new("scalar", dim), &dim, |bencher, _| {
            bencher.iter(|| dot_product_scalar(black_box(&a), black_box(&b)));
        });
    }

    group.finish();
}

/// Benchmark the specific case of OpenAI embeddings (1536 dimensions).
fn bench_dot_product_openai(c: &mut Criterion) {
    // OpenAI text-embedding-3-small dimension
    let dim = 1536;
    let a = generate_vector(dim, 0);
    let b = generate_vector(dim, 42);

    c.bench_function("dot_product_openai_1536d", |bencher| {
        bencher.iter(|| dot_product(black_box(&a), black_box(&b)).unwrap());
    });
}

/// Benchmark self dot product (equivalent to squared magnitude).
/// This is useful for normalization operations.
fn bench_dot_product_self(c: &mut Criterion) {
    let mut group = c.benchmark_group("dot_product_self");

    let dimensions = [384, 1536];

    for dim in dimensions {
        let a = generate_vector(dim, 0);

        group.throughput(Throughput::Elements(dim as u64));

        group.bench_with_input(BenchmarkId::new("self_dot", dim), &dim, |bencher, _| {
            bencher.iter(|| dot_product(black_box(&a), black_box(&a)).unwrap());
        });
    }

    group.finish();
}

/// Benchmark batch dot product computations.
/// Simulates computing dot products against a collection (e.g., for ranking).
fn bench_dot_product_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("dot_product_batch");

    let dim = 384; // Sentence Transformers dimension
    let batch_sizes = [10, 100, 1000];

    let query = generate_vector(dim, 0);

    for batch_size in batch_sizes {
        let vectors: Vec<Vec<f32>> = (0..batch_size)
            .map(|i| generate_vector(dim, i + 1))
            .collect();

        group.throughput(Throughput::Elements((batch_size * dim) as u64));

        group.bench_with_input(
            BenchmarkId::new("batch_max", batch_size),
            &batch_size,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        vectors
                            .iter()
                            .map(|v| dot_product(black_box(&query), black_box(v)).unwrap())
                            .fold(f32::NEG_INFINITY, f32::max),
                    )
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_cosine_similarity_dimensions,
    bench_cosine_similarity_openai,
    bench_cosine_similarity_normalized,
    bench_cosine_similarity_batch,
    bench_euclidean_distance_dimensions,
    bench_euclidean_distance_openai,
    bench_squared_vs_euclidean,
    bench_euclidean_distance_batch,
    bench_dot_product_dimensions,
    bench_dot_product_openai,
    bench_dot_product_self,
    bench_dot_product_batch,
);

criterion_main!(benches);
