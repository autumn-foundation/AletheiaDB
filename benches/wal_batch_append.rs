//! Benchmarks for WAL batch append performance (Issue #219)
//!
//! This benchmark compares the performance of:
//! - Individual `append_async()` calls (baseline)
//! - Batch `append_batch()` calls (optimized)
//!
//! Expected improvements with batch append:
//! - Reduced atomic operations (single LSN allocation vs N allocations)
//! - Better CPU cache locality during serialization
//! - Reduced stripe buffer contention
//!
//! Target: 20-50% throughput improvement for batch sizes > 10

use aletheiadb::{
    core::{PropertyMapBuilder, id::NodeId, interning::GLOBAL_INTERNER, temporal::time},
    storage::wal::{
        WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        durability::DurabilityMode,
    },
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;

/// Helper to create a WAL instance for benchmarking
fn create_wal() -> (ConcurrentWalSystem, TempDir) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = ConcurrentWalSystemConfig::new(temp_dir.path().to_path_buf())
        .with_durability_mode(DurabilityMode::Async {
            // Use 1-second interval to avoid background flush during benchmarks
            // while keeping it realistic (10s was excessive)
            flush_interval_ms: 1_000,
        });
    let wal = ConcurrentWalSystem::new(config).expect("failed to create WAL");
    (wal, temp_dir)
}

/// Helper to create test operations
fn create_test_operations(count: usize) -> Vec<WalOperation> {
    (0..count)
        .map(|i| WalOperation::CreateNode {
            node_id: NodeId::new(i as u64 + 1).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert("name", format!("Node {}", i))
                .build(),
            valid_from: time::now(),
        })
        .collect()
}

/// Helper to create minimal test operations (for LSN allocation benchmarks)
fn create_minimal_operations(count: usize) -> Vec<WalOperation> {
    (0..count)
        .map(|i| WalOperation::CreateNode {
            node_id: NodeId::new(i as u64 + 1).unwrap(),
            label: GLOBAL_INTERNER.intern("N").unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
        })
        .collect()
}

/// Benchmark individual appends vs batch append for different batch sizes
fn bench_batch_append_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_batch_append_comparison");

    for batch_size in &[1, 5, 10, 20, 50, 100] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        // Baseline: individual appends
        // Use iter_with_setup to exclude operation creation from measurement
        group.bench_function(BenchmarkId::new("individual_appends", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter_with_setup(
                || create_test_operations(*batch_size),
                |ops| {
                    for op in ops {
                        black_box(wal.append_async(op).unwrap());
                    }
                },
            );
        });

        // Optimized: batch append
        // Use iter_with_setup to exclude operation creation from measurement
        group.bench_function(BenchmarkId::new("batch_append", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter_with_setup(
                || create_test_operations(*batch_size),
                |ops| black_box(wal.append_batch(ops).unwrap()),
            );
        });
    }

    group.finish();
}

/// Benchmark high-throughput scenario with large batches
fn bench_batch_append_high_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_batch_append_high_throughput");
    group.throughput(Throughput::Elements(1000));

    // Individual appends - 1000 operations
    // Use iter_with_setup to exclude operation creation from measurement
    group.bench_function("individual_1000_ops", |b| {
        let (wal, _guard) = create_wal();

        b.iter_with_setup(
            || create_test_operations(1000),
            |ops| {
                for op in ops {
                    black_box(wal.append_async(op).unwrap());
                }
            },
        );
    });

    // Batch append - 1000 operations in a single batch
    // Use iter_with_setup to exclude operation creation from measurement
    group.bench_function("batch_1000_ops", |b| {
        let (wal, _guard) = create_wal();

        b.iter_with_setup(
            || create_test_operations(1000),
            |ops| black_box(wal.append_batch(ops).unwrap()),
        );
    });

    group.finish();
}

/// Benchmark LSN allocation overhead specifically
fn bench_lsn_allocation_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_lsn_allocation_overhead");

    for batch_size in &[10, 100, 1000] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        // Individual LSN allocations
        // Use minimal operations and iter_with_setup for accurate measurement
        group.bench_function(BenchmarkId::new("individual_lsn", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter_with_setup(
                || create_minimal_operations(*batch_size),
                |ops| {
                    for op in ops {
                        black_box(wal.append_async(op).unwrap());
                    }
                },
            );
        });

        // Batch LSN allocation
        // Use minimal operations and iter_with_setup for accurate measurement
        group.bench_function(BenchmarkId::new("batch_lsn", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter_with_setup(
                || create_minimal_operations(*batch_size),
                |ops| black_box(wal.append_batch(ops).unwrap()),
            );
        });
    }

    group.finish();
}

/// Benchmark mixed workload (some individual, some batch)
fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_batch_append_mixed_workload");
    group.throughput(Throughput::Elements(100));

    group.bench_function("mixed_individual_and_batch", |b| {
        let (wal, _guard) = create_wal();

        b.iter_with_setup(
            || {
                // Pre-create operations outside measurement
                let individual_ops = create_test_operations(10);
                let batch_ops = create_test_operations(90);
                (individual_ops, batch_ops)
            },
            |(individual_ops, batch_ops)| {
                // 10 individual operations
                for op in individual_ops {
                    black_box(wal.append_async(op).unwrap());
                }

                // 1 batch of 90 operations
                black_box(wal.append_batch(batch_ops).unwrap());
            },
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_batch_append_comparison,
    bench_batch_append_high_throughput,
    bench_lsn_allocation_overhead,
    bench_mixed_workload,
);
criterion_main!(benches);
