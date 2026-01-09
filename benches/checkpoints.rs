//! Benchmarks for checkpoint creation, loading, and recovery operations

mod common;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::{
    property::PropertyMapBuilder,
    temporal::{BiTemporalInterval, time},
};
use gallifreydb::storage::{
    CurrentStorage, HistoricalStorage,
    persistence::{CheckpointConfig, PersistenceManager},
    wal::{
        WalConfig, WalOperation, WriteAheadLog,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
    },
};
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
            let wal_config = WalConfig {
                wal_dir: temp_dir.path().join("wal"),
                ..Default::default()
            };
            let mut wal = WriteAheadLog::new(wal_config).unwrap();

            let checkpoint_config = CheckpointConfig {
                checkpoint_dir: temp_dir.path().join("checkpoints"),
                ..Default::default()
            };
            let mut persistence = PersistenceManager::new(checkpoint_config).unwrap();

            b.iter(|| {
                let lsn = wal.current_lsn();
                persistence
                    .create_checkpoint(lsn, &current, &historical, &mut wal)
                    .unwrap();
                black_box(());
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

        let wal_config = WalConfig {
            wal_dir: temp_dir.path().join("wal"),
            ..Default::default()
        };
        let mut wal = WriteAheadLog::new(wal_config).unwrap();

        let checkpoint_config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().join("checkpoints"),
            ..Default::default()
        };
        let mut persistence = PersistenceManager::new(checkpoint_config.clone()).unwrap();

        // Create a checkpoint
        let lsn = wal.current_lsn();
        persistence
            .create_checkpoint(lsn, &current, &historical, &mut wal)
            .unwrap();

        // Benchmark loading the checkpoint
        group.bench_function(BenchmarkId::from_parameter(node_count), |b| {
            b.iter(|| {
                let manager = PersistenceManager::new(checkpoint_config.clone()).unwrap();
                black_box(manager.find_latest_checkpoint().unwrap());
            });
        });
    }

    group.finish();
}

fn bench_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery");

    // Benchmark recovery with different WAL sizes
    for wal_entries in &[10, 100, 1000] {
        group.bench_function(BenchmarkId::from_parameter(wal_entries), |b| {
            let temp_dir = TempDir::new().unwrap();

            // Setup: Create WAL with entries
            let wal_config = WalConfig {
                wal_dir: temp_dir.path().join("wal"),
                ..Default::default()
            };
            let mut wal = WriteAheadLog::new(wal_config.clone()).unwrap();

            for i in 0..*wal_entries {
                let operation = WalOperation::CreateNode {
                    node_id: gallifreydb::core::id::NodeId::new(i).unwrap(),
                    label: "Person".to_string(),
                    properties: PropertyMapBuilder::new().build(),
                    temporal: BiTemporalInterval::current(time::now()),
                };
                wal.append(operation).unwrap();
            }

            // Create a checkpoint partway through
            let current = CurrentStorage::new();
            let historical = HistoricalStorage::new();
            let checkpoint_config = CheckpointConfig {
                checkpoint_dir: temp_dir.path().join("checkpoints"),
                ..Default::default()
            };
            let mut persistence = PersistenceManager::new(checkpoint_config.clone()).unwrap();

            let mid_lsn = wal.current_lsn();
            persistence
                .create_checkpoint(mid_lsn, &current, &historical, &mut wal)
                .unwrap();

            // Add more WAL entries after checkpoint
            for i in *wal_entries..(*wal_entries + 100) {
                let operation = WalOperation::CreateNode {
                    node_id: gallifreydb::core::id::NodeId::new(i).unwrap(),
                    label: "Person".to_string(),
                    properties: PropertyMapBuilder::new().build(),
                    temporal: BiTemporalInterval::current(time::now()),
                };
                wal.append(operation).unwrap();
            }

            // Benchmark recovery
            b.iter(|| {
                let mut manager = PersistenceManager::new(checkpoint_config.clone()).unwrap();
                // Use ConcurrentWalSystem for recovery (reads same segment files)
                let wal_sys_config = ConcurrentWalSystemConfig::new(wal_config.wal_dir.clone());
                let wal = ConcurrentWalSystem::new(wal_sys_config).unwrap();
                black_box(manager.recover(&wal).unwrap());
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
