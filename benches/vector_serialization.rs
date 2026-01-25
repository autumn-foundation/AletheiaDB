//! Benchmarks for vector serialization optimization
//!
//! Issue #203: Vector serialization uses inefficient per-element byte copying.
//! This benchmark measures the performance improvement from bulk byte copy on little-endian.
//!
//! Expected improvement: 2-10x speedup for typical embedding sizes (384-1536 dimensions)

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::property::{deserialize_vector, serialize_vector};

/// Benchmark serialization for various embedding sizes
fn bench_serialize_vector(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_serialize");

    // Test common embedding dimensions:
    // - 384: sentence-transformers/all-MiniLM-L6-v2
    // - 768: BERT-base
    // - 1536: OpenAI text-embedding-ada-002
    // - 3072: OpenAI text-embedding-3-large
    let dimensions = vec![
        1,    // Minimal
        10,   // Small
        128,  // Small embedding
        384,  // MiniLM
        768,  // BERT
        1536, // OpenAI ada-002
        3072, // OpenAI large
    ];

    for dim in dimensions {
        group.throughput(Throughput::Bytes((dim * 4) as u64)); // f32 = 4 bytes
        group.bench_function(BenchmarkId::new("serialize", dim), |b| {
            let vector: Vec<f32> = (0..dim).map(|i| (i as f32) / dim as f32).collect();

            b.iter(|| {
                black_box(serialize_vector(black_box(&vector)));
            });
        });
    }

    group.finish();
}

/// Benchmark deserialization for various embedding sizes
fn bench_deserialize_vector(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_deserialize");

    let dimensions = vec![1, 10, 128, 384, 768, 1536, 3072];

    for dim in dimensions {
        group.throughput(Throughput::Bytes((dim * 4) as u64));
        group.bench_function(BenchmarkId::new("deserialize", dim), |b| {
            let vector: Vec<f32> = (0..dim).map(|i| (i as f32) / dim as f32).collect();
            let bytes = serialize_vector(&vector);

            b.iter(|| {
                black_box(deserialize_vector(black_box(&bytes)).unwrap());
            });
        });
    }

    group.finish();
}

/// Benchmark round-trip (serialize + deserialize) for realistic workloads
fn bench_vector_round_trip(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_round_trip");

    let dimensions = vec![384, 768, 1536, 3072];

    for dim in dimensions {
        group.throughput(Throughput::Bytes((dim * 4 * 2) as u64)); // 2x for round trip
        group.bench_function(BenchmarkId::new("round_trip", dim), |b| {
            let vector: Vec<f32> = (0..dim).map(|i| (i as f32) / dim as f32).collect();

            b.iter(|| {
                let bytes = serialize_vector(black_box(&vector));
                let (recovered, _) = deserialize_vector(black_box(&bytes)).unwrap();
                black_box(recovered);
            });
        });
    }

    group.finish();
}

/// Benchmark batch serialization (RAG workload simulation)
fn bench_batch_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_batch_serialize");

    // Simulate RAG workload: processing 100 embeddings at 1536 dimensions
    let batch_size = 100;
    let dim = 1536;

    group.throughput(Throughput::Bytes((batch_size * dim * 4) as u64));
    group.bench_function(
        BenchmarkId::new("rag_workload", format!("{}x{}", batch_size, dim)),
        |b| {
            let vectors: Vec<Vec<f32>> = (0..batch_size)
                .map(|j| (0..dim).map(|i| ((i + j) as f32) / dim as f32).collect())
                .collect();

            b.iter(|| {
                for vector in &vectors {
                    black_box(serialize_vector(black_box(vector)));
                }
            });
        },
    );

    group.finish();
}

/// Benchmark batch deserialization (retrieval workload simulation)
fn bench_batch_deserialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_batch_deserialize");

    // Simulate retrieval: deserializing 100 stored embeddings
    let batch_size = 100;
    let dim = 1536;

    group.throughput(Throughput::Bytes((batch_size * dim * 4) as u64));
    group.bench_function(
        BenchmarkId::new("retrieval_workload", format!("{}x{}", batch_size, dim)),
        |b| {
            let vectors: Vec<Vec<f32>> = (0..batch_size)
                .map(|j| (0..dim).map(|i| ((i + j) as f32) / dim as f32).collect())
                .collect();
            let serialized: Vec<_> = vectors.iter().map(|v| serialize_vector(v)).collect();

            b.iter(|| {
                for bytes in &serialized {
                    black_box(deserialize_vector(black_box(bytes)).unwrap());
                }
            });
        },
    );

    group.finish();
}

/// Benchmark serialize_into pattern (common in WAL)
fn bench_serialize_into(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_serialize_into");

    let dimensions = vec![384, 768, 1536];

    for dim in dimensions {
        group.throughput(Throughput::Bytes((dim * 4) as u64));
        group.bench_function(BenchmarkId::new("serialize_into", dim), |b| {
            let vector: Vec<f32> = (0..dim).map(|i| (i as f32) / dim as f32).collect();

            b.iter(|| {
                let mut buffer = Vec::new();
                gallifreydb::core::property::serialize_vector_into(black_box(&vector), &mut buffer);
                black_box(buffer);
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_serialize_vector,
    bench_deserialize_vector,
    bench_vector_round_trip,
    bench_batch_serialize,
    bench_batch_deserialize,
    bench_serialize_into,
);
criterion_main!(benches);
