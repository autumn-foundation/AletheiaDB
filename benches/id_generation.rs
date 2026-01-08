//! Benchmarks for ID generation.
//!
//! These benchmarks measure the performance impact of memory ordering choices
//! in the IdGenerator, particularly the `SeqCst` ordering used to ensure
//! cross-thread visibility and correctness.
//!
//! Performance Context:
//! - ID generation is not a hot path (occurs infrequently)
//! - ~5-10% overhead from SeqCst vs AcqRel is acceptable
//! - Correctness is prioritized over micro-optimizations

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::IdGenerator;
use std::sync::Arc;
use std::thread;

/// Benchmark single-threaded ID generation.
fn bench_single_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_single_thread");

    group.bench_function("sequential_1000", |b| {
        let generator = IdGenerator::new();
        b.iter(|| {
            for _ in 0..1000 {
                black_box(generator.next().unwrap());
            }
        });
    });

    group.bench_function("sequential_10000", |b| {
        let generator = IdGenerator::new();
        b.iter(|| {
            for _ in 0..10000 {
                black_box(generator.next().unwrap());
            }
        });
    });

    group.finish();
}

/// Benchmark concurrent ID generation with varying thread counts.
fn bench_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_concurrent");

    for thread_count in [2, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::new("threads", thread_count),
            &thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let generator = Arc::new(IdGenerator::new());
                    let ids_per_thread = 1000;

                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let gen_clone = Arc::clone(&generator);
                            thread::spawn(move || {
                                for _ in 0..ids_per_thread {
                                    black_box(gen_clone.next().unwrap());
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark current() method (read-only).
fn bench_current(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_current");

    group.bench_function("read_current", |b| {
        let generator = IdGenerator::new();
        // Generate some IDs first
        for _ in 0..1000 {
            generator.next().unwrap();
        }

        b.iter(|| {
            black_box(generator.current());
        });
    });

    group.finish();
}

/// Benchmark the full lifecycle: create generator, generate IDs, get current.
fn bench_full_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_lifecycle");

    group.bench_function("create_and_generate_1000", |b| {
        b.iter(|| {
            let generator = IdGenerator::new();
            for _ in 0..1000 {
                black_box(generator.next().unwrap());
            }
            black_box(generator.current());
        });
    });

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
    targets = bench_single_thread,
    bench_concurrent,
    bench_current,
    bench_full_lifecycle
);
criterion_main!(benches);
