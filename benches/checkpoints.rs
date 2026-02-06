//! Benchmarks for checkpoint creation, loading, and recovery operations

mod common;

use aletheiadb::core::{interning::GLOBAL_INTERNER, property::PropertyMapBuilder, temporal::time};
use aletheiadb::storage::{
    CurrentStorage, HistoricalStorage, LSN,
    persistence::{Checkpoint, CheckpointConfig, PersistenceManager},
    wal::{
        WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
    },
};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
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
            let checkpoint_path = temp_dir.path().join("benchmark.dat");

            b.iter(|| {
                let checkpoint = Checkpoint::new(LSN(100), &current, &historical);
                checkpoint.save(&checkpoint_path).unwrap();
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

        // Create a checkpoint file
        let checkpoint_path = temp_dir.path().join("checkpoint_000001.dat");
        let checkpoint = Checkpoint::new(LSN(100), &current, &historical);
        checkpoint.save(&checkpoint_path).unwrap();

        let checkpoint_config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };

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

            // Setup: Create WAL with entries using ConcurrentWalSystem
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path().join("wal"));
            let wal = ConcurrentWalSystem::new(wal_config.clone()).unwrap();

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
            let checkpoint_config = CheckpointConfig {
                checkpoint_dir: temp_dir.path().join("checkpoints"),
                ..Default::default()
            };

            // Create checkpoint directory and save checkpoint
            std::fs::create_dir_all(&checkpoint_config.checkpoint_dir).unwrap();
            let checkpoint_path = checkpoint_config
                .checkpoint_dir
                .join("checkpoint_000001.dat");
            let mid_lsn = wal.current_lsn();
            let checkpoint = Checkpoint::new(mid_lsn, &current, &historical);
            checkpoint.save(&checkpoint_path).unwrap();

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
