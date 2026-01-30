//! Benchmarks for WAL entry serialization performance
//!
//! This benchmark measures the performance impact of buffer reuse in WAL entry serialization.
//! Issue #20: Each WAL entry serialization allocates a new Vec, causing ~100-500ns overhead.
//!
//! Expected improvement: 100-500ns reduction per entry with buffer reuse.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::{
    core::{
        PropertyMapBuilder,
        id::{EdgeId, NodeId, VersionId},
        interning::GLOBAL_INTERNER,
        temporal::time,
    },
    storage::wal::{
        WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
    },
};
use tempfile::TempDir;

/// Helper to create a WAL instance for benchmarking
fn create_wal() -> (ConcurrentWalSystem, TempDir) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = ConcurrentWalSystemConfig::new(temp_dir.path().to_path_buf());
    let wal = ConcurrentWalSystem::new(config).expect("failed to create WAL");
    (wal, temp_dir)
}

/// Benchmark serialization of CreateNode operations
fn bench_serialize_create_node(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_serialize_create_node");

    // Benchmark with different property sizes to measure allocation overhead
    for prop_count in &[0, 5, 10, 50] {
        group.bench_function(BenchmarkId::from_parameter(prop_count), |b| {
            let (wal, _guard) = create_wal();

            b.iter(|| {
                // Create operation with varying property counts
                let mut props = PropertyMapBuilder::new();
                for i in 0..*prop_count {
                    let key = format!("prop_{}", i);
                    props = props.insert(&key, i as i64);
                }

                let operation = WalOperation::CreateNode {
                    node_id: black_box(NodeId::new(1).unwrap()),
                    label: GLOBAL_INTERNER.intern("Person").unwrap(),
                    properties: props.build(),
                    valid_from: time::now(),
                };

                // This will call serialize_entry internally
                black_box(wal.append_async(operation).unwrap());
            });
        });
    }

    group.finish();
}

/// Benchmark serialization of CreateEdge operations
fn bench_serialize_create_edge(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_serialize_create_edge");

    for prop_count in &[0, 5, 10, 50] {
        group.bench_function(BenchmarkId::from_parameter(prop_count), |b| {
            let (wal, _guard) = create_wal();

            b.iter(|| {
                let mut props = PropertyMapBuilder::new();
                for i in 0..*prop_count {
                    let key = format!("prop_{}", i);
                    props = props.insert(&key, i as i64);
                }

                let operation = WalOperation::CreateEdge {
                    edge_id: black_box(EdgeId::new(1).unwrap()),
                    source: NodeId::new(1).unwrap(),
                    target: NodeId::new(2).unwrap(),
                    label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
                    properties: props.build(),
                    valid_from: time::now(),
                };

                black_box(wal.append_async(operation).unwrap());
            });
        });
    }

    group.finish();
}

/// Benchmark serialization of UpdateNode operations
fn bench_serialize_update_node(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_serialize_update_node");

    for prop_count in &[0, 5, 10, 50] {
        group.bench_function(BenchmarkId::from_parameter(prop_count), |b| {
            let (wal, _guard) = create_wal();

            b.iter(|| {
                let mut props = PropertyMapBuilder::new();
                for i in 0..*prop_count {
                    let key = format!("prop_{}", i);
                    props = props.insert(&key, i as i64);
                }

                let operation = WalOperation::UpdateNode {
                    node_id: black_box(NodeId::new(1).unwrap()),
                    version_id: VersionId::new(2).unwrap(),
                    label: GLOBAL_INTERNER.intern("Person").unwrap(),
                    properties: props.build(),
                    valid_from: time::now(),
                };

                black_box(wal.append_async(operation).unwrap());
            });
        });
    }

    group.finish();
}

/// Benchmark batch serialization to measure cumulative allocation overhead
fn bench_serialize_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_serialize_batch");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("batch_1000_create_node", |b| {
        let (wal, _guard) = create_wal();

        b.iter(|| {
            for i in 0..1000 {
                let operation = WalOperation::CreateNode {
                    node_id: NodeId::new(i).unwrap(),
                    label: GLOBAL_INTERNER.intern("Person").unwrap(),
                    properties: PropertyMapBuilder::new().insert("id", i as i64).build(),
                    valid_from: time::now(),
                };
                black_box(wal.append_async(operation).unwrap());
            }
        });
    });

    group.finish();
}

/// Benchmark high-frequency serialization (worst case for allocation overhead)
fn bench_serialize_high_frequency(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_serialize_high_frequency");
    group.throughput(Throughput::Elements(10000));

    group.bench_function("sequential_10k_operations", |b| {
        let (wal, _guard) = create_wal();

        b.iter(|| {
            for i in 0..10000 {
                let operation = WalOperation::CreateNode {
                    node_id: NodeId::new(i).unwrap(),
                    label: GLOBAL_INTERNER.intern("Test").unwrap(),
                    properties: PropertyMapBuilder::new().build(),
                    valid_from: time::now(),
                };
                black_box(wal.append_async(operation).unwrap());
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_serialize_create_node,
    bench_serialize_create_edge,
    bench_serialize_update_node,
    bench_serialize_batch,
    bench_serialize_high_frequency,
);
criterion_main!(benches);
