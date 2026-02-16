//! Benchmarks for checkpoint creation, loading, and recovery operations

mod common;

use aletheiadb::core::{interning::GLOBAL_INTERNER, property::PropertyMapBuilder, temporal::time};
use aletheiadb::storage::{
    CurrentStorage, HistoricalStorage, LSN,
    checkpoint::{CheckpointConfig, CheckpointManager},
    wal::{
        WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
    },
};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;

fn bench_checkpoint_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_creation");

    // Benchmark checkpoint creation with different database sizes
    for node_count in &[100, 1000, 10000] {
        group.bench_function(BenchmarkId::from_parameter(node_count), |b| {
            // Setup database with nodes
            let current = CurrentStorage::new();
            let historical = HistoricalStorage::new();

            for i in 0..*node_count {
                current
                    .create_node(
                        "Person",
                        PropertyMapBuilder::new().insert("id", i as i64).build(),
                    )
                    .unwrap();
            }

            // Verify setup succeeded before benchmarking
            assert_eq!(
                current.node_count(),
                *node_count,
                "Setup failed - expected {} nodes, got {}",
                node_count,
                current.node_count()
            );

            let temp_dir = TempDir::new().unwrap();
            let mut manager =
                CheckpointManager::new(CheckpointConfig::with_data_dir(temp_dir.path())).unwrap();

            b.iter(|| {
                let stats = manager
                    .create_checkpoint(LSN(100), &current, &historical)
                    .unwrap();
                black_box(stats.bytes_written);
            });
        });
    }

    group.finish();
}

fn bench_checkpoint_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_load");

    // Setup: Create checkpoints with different sizes
    for node_count in &[100, 1000, 10000] {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().join("data");
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        for i in 0..*node_count {
            current
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
        }

        // Verify setup succeeded
        assert_eq!(
            current.node_count(),
            *node_count,
            "Setup failed - expected {} nodes",
            node_count
        );

        // Create persisted checkpoint state once
        let mut setup_manager =
            CheckpointManager::new(CheckpointConfig::with_data_dir(&data_dir)).unwrap();
        setup_manager
            .create_checkpoint(LSN(100), &current, &historical)
            .unwrap();

        // Empty WAL for replay step
        let wal =
            ConcurrentWalSystem::new(ConcurrentWalSystemConfig::new(temp_dir.path().join("wal")))
                .unwrap();
        wal.flush().unwrap();

        // Benchmark loading the checkpoint
        group.bench_function(BenchmarkId::from_parameter(node_count), |b| {
            b.iter(|| {
                let mut manager =
                    CheckpointManager::new(CheckpointConfig::with_data_dir(&data_dir)).unwrap();
                let (loaded_current, loaded_historical, lsn) = manager.recover(&wal).unwrap();
                black_box((
                    loaded_current.node_count(),
                    loaded_historical.stats().total_node_versions,
                    lsn.0,
                ));
            });
        });
    }

    group.finish();
}

fn bench_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery");

    // Benchmark recovery with different WAL sizes
    for wal_entries in &[10u64, 100, 1000] {
        group.bench_function(BenchmarkId::from_parameter(wal_entries), |b| {
            let temp_dir = TempDir::new().unwrap();
            let wal_dir = temp_dir.path().join("wal");
            let checkpoint_data_dir = temp_dir.path().join("checkpoints");

            // Setup: Create WAL with entries using ConcurrentWalSystem
            let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
            let wal = ConcurrentWalSystem::new(wal_config).unwrap();

            for i in 0..*wal_entries {
                let operation = WalOperation::CreateNode {
                    node_id: aletheiadb::core::id::NodeId::new(i).unwrap(),
                    label: GLOBAL_INTERNER.intern("Person").unwrap(),
                    properties: PropertyMapBuilder::new().build(),
                    valid_from: time::now(),
                };
                wal.append_async(operation).unwrap();
            }
            // Ensure all entries are flushed
            wal.commit().unwrap();

            // Create a checkpoint partway through
            let current = CurrentStorage::new();
            let historical = HistoricalStorage::new();
            let mut setup_manager =
                CheckpointManager::new(CheckpointConfig::with_data_dir(&checkpoint_data_dir))
                    .unwrap();
            let mid_lsn = wal.current_lsn();
            setup_manager
                .create_checkpoint(mid_lsn, &current, &historical)
                .unwrap();

            // Add more WAL entries after checkpoint
            for i in *wal_entries..(*wal_entries + 100) {
                let operation = WalOperation::CreateNode {
                    node_id: aletheiadb::core::id::NodeId::new(i).unwrap(),
                    label: GLOBAL_INTERNER.intern("Person").unwrap(),
                    properties: PropertyMapBuilder::new().build(),
                    valid_from: time::now(),
                };
                wal.append_async(operation).unwrap();
            }
            wal.commit().unwrap();

            // Benchmark recovery
            b.iter(|| {
                let mut manager =
                    CheckpointManager::new(CheckpointConfig::with_data_dir(&checkpoint_data_dir))
                        .unwrap();
                // Re-open WAL from the same directory for each benchmark iteration
                let wal_for_recovery =
                    ConcurrentWalSystem::new(ConcurrentWalSystemConfig::new(wal_dir.clone()))
                        .unwrap();
                let (recovered_current, recovered_historical, lsn) =
                    manager.recover(&wal_for_recovery).unwrap();
                black_box((
                    recovered_current.node_count(),
                    recovered_historical.stats().total_node_versions,
                    lsn.0,
                ));
            });
        });
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_checkpoint_creation,
    bench_checkpoint_load,
    bench_recovery
);
criterion_main!(benches);
