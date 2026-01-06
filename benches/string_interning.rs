//! Benchmarks for string interning performance.
//!
//! These benchmarks measure the performance difference between `resolve()` and
//! `with_str()` methods, demonstrating the Arc clone overhead that `with_str()`
//! avoids through its callback-based API.
//!
//! Performance Context:
//! - `resolve()` clones the Arc on every call (atomic increment/decrement)
//! - `with_str()` provides direct &str access without Arc cloning
//! - Performance gain is most significant for frequently accessed strings
//! - Real-world use cases: serialization, logging, display operations

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use gallifreydb::core::interning::StringInterner;
use std::sync::Arc;

/// Benchmark single string access using resolve() vs with_str().
fn bench_single_string_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_access_single");

    let interner = StringInterner::new();
    let id = interner.intern("test_string").unwrap();

    group.bench_function("resolve", |b| {
        b.iter(|| {
            let s = interner.resolve(id).unwrap();
            black_box(s.len())
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| interner.with_str(id, |s| black_box(s.len())));
    });

    group.finish();
}

/// Benchmark string length computation (common read-only operation).
fn bench_string_length(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_length");

    let interner = StringInterner::new();
    let id = interner
        .intern("this is a test string for benchmarking")
        .unwrap();

    group.bench_function("resolve", |b| {
        b.iter(|| {
            let s = interner.resolve(id).unwrap();
            black_box(s.len())
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| interner.with_str(id, |s| black_box(s.len())));
    });

    group.finish();
}

/// Benchmark string comparison (another common read-only operation).
fn bench_string_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_comparison");

    let interner = StringInterner::new();
    let id = interner.intern("Person").unwrap();

    group.bench_function("resolve", |b| {
        b.iter(|| {
            let s = interner.resolve(id).unwrap();
            black_box(s.as_ref() == "Person")
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| interner.with_str(id, |s| black_box(s == "Person")));
    });

    group.finish();
}

/// Benchmark multiple string accesses in a loop (simulates real workload).
fn bench_multiple_accesses(c: &mut Criterion) {
    let mut group = c.benchmark_group("multiple_accesses");

    let interner = StringInterner::new();
    let ids: Vec<_> = (0..100)
        .map(|i| interner.intern(format!("string_{}", i)).unwrap())
        .collect();

    group.bench_function("resolve_100", |b| {
        b.iter(|| {
            for &id in &ids {
                let s = interner.resolve(id).unwrap();
                black_box(s.len());
            }
        });
    });

    group.bench_function("with_str_100", |b| {
        b.iter(|| {
            for &id in &ids {
                interner.with_str(id, |s| black_box(s.len()));
            }
        });
    });

    group.finish();
}

/// Benchmark simulated serialization workload (common use case).
fn bench_serialization_simulation(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialization_simulation");

    let interner = StringInterner::new();
    let property_keys: Vec<_> = ["name", "age", "email", "address", "phone"]
        .iter()
        .map(|&s| interner.intern(s).unwrap())
        .collect();

    // Simulate serializing 1000 objects with 5 properties each
    group.bench_function("resolve", |b| {
        b.iter(|| {
            let mut total_len = 0;
            for _ in 0..1000 {
                for &key_id in &property_keys {
                    let key = interner.resolve(key_id).unwrap();
                    // Simulate serialization by computing length and hashing
                    total_len += key.len();
                    black_box(key.as_ref());
                }
            }
            black_box(total_len);
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| {
            let mut total_len = 0;
            for _ in 0..1000 {
                for &key_id in &property_keys {
                    interner.with_str(key_id, |key| {
                        // Simulate serialization by computing length and hashing
                        total_len += key.len();
                        black_box(key);
                    });
                }
            }
            black_box(total_len);
        });
    });

    group.finish();
}

/// Benchmark Arc clone overhead isolation (measures just the Arc operations).
fn bench_arc_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("arc_overhead");

    let interner = StringInterner::new();
    let id = interner.intern("overhead_test").unwrap();

    // Measure Arc clone + drop overhead
    group.bench_function("arc_clone_drop", |b| {
        b.iter(|| {
            let arc = interner.resolve(id).unwrap();
            black_box(arc);
            // Arc is dropped here (atomic decrement)
        });
    });

    // Measure direct access without Arc clone
    group.bench_function("direct_access", |b| {
        b.iter(|| {
            interner.with_str(id, |s| {
                black_box(s);
            });
        });
    });

    group.finish();
}

/// Benchmark hot-path scenario: tight loop with repeated accesses.
fn bench_hot_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("hot_path");

    let interner = Arc::new(StringInterner::new());
    let id = interner.intern("hot_path_string").unwrap();

    for iterations in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::new("resolve", iterations),
            &iterations,
            |b, &iterations| {
                b.iter(|| {
                    for _ in 0..iterations {
                        let s = interner.resolve(id).unwrap();
                        black_box(s.len());
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("with_str", iterations),
            &iterations,
            |b, &iterations| {
                b.iter(|| {
                    for _ in 0..iterations {
                        interner.with_str(id, |s| black_box(s.len()));
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_single_string_access,
    bench_string_length,
    bench_string_comparison,
    bench_multiple_accesses,
    bench_serialization_simulation,
    bench_arc_overhead,
    bench_hot_path
);
criterion_main!(benches);
