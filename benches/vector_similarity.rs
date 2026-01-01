//! Benchmarks for vector similarity operations.
//!
//! These benchmarks test the performance of cosine similarity calculations
//! at various vector dimensions commonly used in embedding models.
//!
//! Common embedding dimensions:
//! - 384: Sentence Transformers (all-MiniLM)
//! - 768: BERT base, BGE models
//! - 1024: Cohere embed-v3
//! - 1536: OpenAI text-embedding-3-small
//! - 3072: OpenAI text-embedding-3-large

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use gallifreydb::core::vector::{cosine_similarity, cosine_similarity_normalized};

/// Generate a test vector with deterministic values.
fn generate_vector(dim: usize, seed: usize) -> Vec<f32> {
    (0..dim)
        .map(|i| ((i + seed) as f32 / dim as f32) * 2.0 - 1.0)
        .collect()
}

/// Single-pass scalar implementation (same algorithm, no SIMD).
/// This measures pure SIMD benefit without algorithmic differences.
fn cosine_similarity_scalar_1pass(a: &[f32], b: &[f32]) -> f32 {
    let (dot, mag_a_sq, mag_b_sq) = a.iter().zip(b.iter()).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(d, ma, mb), (&ai, &bi)| (d + ai * bi, ma + ai * ai, mb + bi * bi),
    );

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
                    vectors
                        .iter()
                        .map(|v| cosine_similarity(black_box(&query), black_box(v)).unwrap())
                        .fold(f32::NEG_INFINITY, f32::max)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_cosine_similarity_dimensions,
    bench_cosine_similarity_openai,
    bench_cosine_similarity_normalized,
    bench_cosine_similarity_batch,
);

criterion_main!(benches);
