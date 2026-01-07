//! Benchmarks for persistence layer (WAL, checkpoints, recovery)

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::{
    property::PropertyMapBuilder,
    temporal::{BiTemporalInterval, time},
};
use gallifreydb::storage::{
    CurrentStorage, HistoricalStorage,
    persistence::{CheckpointConfig, PersistenceManager},
    wal::{WalConfig, WalOperation, WriteAheadLog},
};
use tempfile::TempDir;

fn bench_wal_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_append");

    // Benchmark appending different operation types
    for op_type in &["create_node", "create_edge", "update_node"] {
        group.bench_function(BenchmarkId::from_parameter(op_type), |b| {
            let temp_dir = TempDir::new().unwrap();
            let config = WalConfig {
                wal_dir: temp_dir.path().to_path_buf(),
                ..Default::default()
            };
            let mut wal = WriteAheadLog::new(config).unwrap();

            b.iter(|| {
                let operation = match *op_type {
                    "create_node" => WalOperation::CreateNode {
                        node_id: black_box(gallifreydb::core::id::NodeId::new(1).unwrap()),
                        label: "Person".to_string(),
                        properties: PropertyMapBuilder::new().build(),
                        temporal: BiTemporalInterval::current(time::now()),
                    },
                    "create_edge" => WalOperation::CreateEdge {
                        edge_id: black_box(gallifreydb::core::id::EdgeId::new(1).unwrap()),
                        source: gallifreydb::core::id::NodeId::new(1).unwrap(),
                        target: gallifreydb::core::id::NodeId::new(2).unwrap(),
                        label: "KNOWS".to_string(),
                        properties: PropertyMapBuilder::new().build(),
                        temporal: BiTemporalInterval::current(time::now()),
                    },
                    _ => WalOperation::UpdateNode {
                        node_id: gallifreydb::core::id::NodeId::new(1).unwrap(),
                        version_id: gallifreydb::core::id::VersionId::new(2).unwrap(),
                        label: "Person".to_string(),
                        properties: PropertyMapBuilder::new().build(),
                        temporal: BiTemporalInterval::current(time::now()),
                    },
                };
                wal.append(operation).unwrap();
            });
        });
    }

    group.finish();
}

fn bench_wal_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_throughput");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("batch_1000_operations", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let config = WalConfig {
                wal_dir: temp_dir.path().to_path_buf(),
                ..Default::default()
            };
            let mut wal = WriteAheadLog::new(config).unwrap();

            for i in 0..1000 {
                let operation = WalOperation::CreateNode {
                    node_id: gallifreydb::core::id::NodeId::new(i).unwrap(),
                    label: "Person".to_string(),
                    properties: PropertyMapBuilder::new().build(),
                    temporal: BiTemporalInterval::current(time::now()),
                };
                black_box(wal.append(operation).unwrap());
            }
        });
    });

    group.finish();
}

fn bench_wal_with_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_sync_modes");

    for sync_enabled in &[false, true] {
        group.bench_function(
            BenchmarkId::from_parameter(if *sync_enabled { "sync_on" } else { "sync_off" }),
            |b| {
                let temp_dir = TempDir::new().unwrap();
                let config = WalConfig {
                    wal_dir: temp_dir.path().to_path_buf(),
                    ..Default::default()
                };
                let mut wal = WriteAheadLog::new(config).unwrap();

                b.iter(|| {
                    let operation = WalOperation::CreateNode {
                        node_id: gallifreydb::core::id::NodeId::new(1).unwrap(),
                        label: "Person".to_string(),
                        properties: PropertyMapBuilder::new().build(),
                        temporal: BiTemporalInterval::current(time::now()),
                    };
                    wal.append(operation).unwrap();
                });
            },
        );
    }

    group.finish();
}

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
                let wal = WriteAheadLog::new(wal_config.clone()).unwrap();
                black_box(manager.recover(&wal).unwrap());
            });
        });
    }

    group.finish();
}

fn bench_checkpoint_frequency(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_frequency");

    // Test different checkpoint intervals
    for interval_secs in &[1, 60, 300] {
        group.bench_function(BenchmarkId::from_parameter(interval_secs), |b| {
            let temp_dir = TempDir::new().unwrap();
            let wal_config = WalConfig {
                wal_dir: temp_dir.path().join("wal"),
                ..Default::default()
            };
            let mut wal = WriteAheadLog::new(wal_config).unwrap();

            let checkpoint_config = CheckpointConfig {
                checkpoint_dir: temp_dir.path().join("checkpoints"),
                checkpoint_interval: std::time::Duration::from_secs(*interval_secs),
                ..Default::default()
            };
            let mut persistence = PersistenceManager::new(checkpoint_config).unwrap();

            let current = CurrentStorage::new();
            let historical = HistoricalStorage::new();

            b.iter(|| {
                let lsn = wal.current_lsn();
                if persistence.should_checkpoint(lsn) {
                    persistence
                        .create_checkpoint(lsn, &current, &historical, &mut wal)
                        .unwrap();
                    black_box(());
                }
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_wal_append,
    bench_wal_throughput,
    bench_wal_with_sync,
    bench_checkpoint_creation,
    bench_checkpoint_load,
    bench_recovery,
    bench_checkpoint_frequency
);
criterion_main!(benches);
