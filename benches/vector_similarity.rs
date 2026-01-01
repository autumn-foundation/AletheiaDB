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
use gallifreydb::core::vector::cosine_similarity;

/// Generate a test vector with deterministic values.
fn generate_vector(dim: usize, seed: usize) -> Vec<f32> {
    (0..dim)
        .map(|i| ((i + seed) as f32 / dim as f32) * 2.0 - 1.0)
        .collect()
}

/// Naive scalar implementation for comparison.
fn cosine_similarity_naive(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        (dot / (mag_a * mag_b)).clamp(-1.0, 1.0)
    }
}

/// Benchmark cosine similarity at various embedding dimensions.
fn bench_cosine_similarity_dimensions(c: &mut Criterion) {
    let mut group = c.benchmark_group("cosine_similarity");

    // Common embedding dimensions
    let dimensions = [384, 768, 1024, 1536, 3072];

    for dim in dimensions {
        let a = generate_vector(dim, 0);
        let b = generate_vector(dim, 42);

        group.throughput(Throughput::Elements(dim as u64));

        group.bench_with_input(BenchmarkId::new("optimized", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity(black_box(&a), black_box(&b)).unwrap());
        });

        group.bench_with_input(BenchmarkId::new("naive", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity_naive(black_box(&a), black_box(&b)));
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
/// This is the common case when vectors are pre-normalized.
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

        group.bench_with_input(BenchmarkId::new("unit_vectors", dim), &dim, |bencher, _| {
            bencher.iter(|| cosine_similarity(black_box(&a), black_box(&b)).unwrap());
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
