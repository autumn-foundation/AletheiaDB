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

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::{
    core::{
        PropertyMapBuilder,
        id::NodeId,
        temporal::{BiTemporalInterval, time},
    },
    storage::wal::{
        WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        durability::DurabilityMode,
    },
};
use tempfile::TempDir;

/// Helper to create a WAL instance for benchmarking
fn create_wal() -> (ConcurrentWalSystem, TempDir) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = ConcurrentWalSystemConfig::new(temp_dir.path().to_path_buf())
        .with_durability_mode(DurabilityMode::Async {
            flush_interval_ms: 10_000, // Long interval to avoid background flush
        });
    let wal = ConcurrentWalSystem::new(config).expect("failed to create WAL");
    (wal, temp_dir)
}

/// Helper to create test operations
fn create_test_operations(count: usize) -> Vec<WalOperation> {
    (0..count)
        .map(|i| WalOperation::CreateNode {
            node_id: NodeId::new(i as u64 + 1).unwrap(),
            label: format!("Node{}", i),
            properties: PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert("name", format!("Node {}", i))
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })
        .collect()
}

/// Benchmark individual appends vs batch append for different batch sizes
fn bench_batch_append_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_batch_append_comparison");

    for batch_size in &[1, 5, 10, 20, 50, 100] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        // Baseline: individual appends
        group.bench_function(BenchmarkId::new("individual_appends", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter(|| {
                for i in 0..*batch_size {
                    let operation = WalOperation::CreateNode {
                        node_id: NodeId::new(i as u64 + 1).unwrap(),
                        label: format!("Node{}", i),
                        properties: PropertyMapBuilder::new().insert("id", i as i64).build(),
                        temporal: BiTemporalInterval::current(time::now()),
                    };
                    black_box(wal.append_async(operation).unwrap());
                }
            });
        });

        // Optimized: batch append
        group.bench_function(BenchmarkId::new("batch_append", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter(|| {
                let ops = create_test_operations(*batch_size);
                black_box(wal.append_batch(ops).unwrap());
            });
        });
    }

    group.finish();
}

/// Benchmark high-throughput scenario with large batches
fn bench_batch_append_high_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_batch_append_high_throughput");
    group.throughput(Throughput::Elements(1000));

    // Individual appends - 1000 operations
    group.bench_function("individual_1000_ops", |b| {
        let (wal, _guard) = create_wal();

        b.iter(|| {
            for i in 0..1000 {
                let operation = WalOperation::CreateNode {
                    node_id: NodeId::new(i + 1).unwrap(),
                    label: "Node".to_string(),
                    properties: PropertyMapBuilder::new().insert("id", i as i64).build(),
                    temporal: BiTemporalInterval::current(time::now()),
                };
                black_box(wal.append_async(operation).unwrap());
            }
        });
    });

    // Batch append - 1000 operations in a single batch
    group.bench_function("batch_1000_ops", |b| {
        let (wal, _guard) = create_wal();

        b.iter(|| {
            let ops = create_test_operations(1000);
            black_box(wal.append_batch(ops).unwrap());
        });
    });

    group.finish();
}

/// Benchmark LSN allocation overhead specifically
fn bench_lsn_allocation_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_lsn_allocation_overhead");

    for batch_size in &[10, 100, 1000] {
        group.throughput(Throughput::Elements(*batch_size as u64));

        // Individual LSN allocations
        group.bench_function(BenchmarkId::new("individual_lsn", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter(|| {
                // Just append minimal operations to focus on LSN allocation
                for i in 0..*batch_size {
                    let operation = WalOperation::CreateNode {
                        node_id: NodeId::new(i as u64 + 1).unwrap(),
                        label: "N".to_string(),
                        properties: PropertyMapBuilder::new().build(),
                        temporal: BiTemporalInterval::current(time::now()),
                    };
                    black_box(wal.append_async(operation).unwrap());
                }
            });
        });

        // Batch LSN allocation
        group.bench_function(BenchmarkId::new("batch_lsn", batch_size), |b| {
            let (wal, _guard) = create_wal();

            b.iter(|| {
                let ops: Vec<_> = (0..*batch_size)
                    .map(|i| WalOperation::CreateNode {
                        node_id: NodeId::new(i as u64 + 1).unwrap(),
                        label: "N".to_string(),
                        properties: PropertyMapBuilder::new().build(),
                        temporal: BiTemporalInterval::current(time::now()),
                    })
                    .collect();
                black_box(wal.append_batch(ops).unwrap());
            });
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

        b.iter(|| {
            // 10 individual operations
            for i in 0..10 {
                let operation = WalOperation::CreateNode {
                    node_id: NodeId::new(i + 1).unwrap(),
                    label: "Node".to_string(),
                    properties: PropertyMapBuilder::new().build(),
                    temporal: BiTemporalInterval::current(time::now()),
                };
                black_box(wal.append_async(operation).unwrap());
            }

            // 1 batch of 90 operations
            let ops = create_test_operations(90);
            black_box(wal.append_batch(ops).unwrap());
        });
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
