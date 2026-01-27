//! Performance benchmarks for database recovery operations.
//!
//! These benchmarks measure recovery time across various scenarios to ensure
//! GallifreyDB meets its performance targets for system restart and disaster recovery.
//!
//! ## Benchmark Scenarios
//!
//! 1. **Small Dataset Recovery**: 100 nodes + 500 edges (target: <100ms)
//! 2. **Medium Dataset Recovery**: 10,000 nodes + 50,000 edges (target: <5s)
//! 3. **Large Dataset Recovery**: 100,000 nodes + 500,000 edges (target: <30s)
//! 4. **WAL Replay**: 100,000 operations without checkpoints (target: <10s)
//! 5. **Vector-Indexed Recovery**: 10,000 nodes with 384-dim embeddings (target: <10s)
//!
//! ## Implementation Notes
//!
//! - Uses CheckpointManager for recovery from persisted state
//! - WAL replay tests measure incremental recovery after checkpoints
//! - Vector recovery includes HNSW index rebuilding time
//! - All benchmarks use realistic data patterns

mod common;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::NodeId;
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::{BiTemporalInterval, time};
use gallifreydb::index::vector::DistanceMetric;
use gallifreydb::index::vector::hnsw::HnswConfig;
use gallifreydb::storage::current::CurrentStorage;
use gallifreydb::storage::historical::HistoricalStorage;
use gallifreydb::storage::wal::WalOperation;
use gallifreydb::storage::wal::concurrent_system::{
    ConcurrentWalSystem, ConcurrentWalSystemConfig,
};
use gallifreydb::storage::{CheckpointManager, UnifiedCheckpointConfig};
use std::time::Duration;
use tempfile::TempDir;

// ============================================================================
// Helper Functions
// ============================================================================

/// Generate a random f32 vector of given dimensions for vector benchmarks.
fn generate_random_vector(dimensions: usize) -> Vec<f32> {
    use rand::Rng as _;
    let mut rng = rand::thread_rng();
    (0..dimensions).map(|_| rng.gen_range(0.0..1.0)).collect()
}

/// Create a populated database with nodes and edges, then checkpoint it.
///
/// Returns (temp_dir, wal, checkpoint_manager) for recovery benchmarking.
fn create_checkpointed_database(
    node_count: usize,
    edge_count: usize,
) -> (TempDir, ConcurrentWalSystem, CheckpointManager) {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL system
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Create current and historical storage
    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();

    // Create nodes
    for i in 0..node_count {
        // Use modulo to cycle through a limited set of name values
        // to avoid hitting string interner capacity limits
        let name_id = i % 1000; // Cycle through 1000 unique names
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("name", format!("Node{}", name_id))
            .build();

        current.create_node("TestNode", props).unwrap();
    }

    // Create edges (exactly edge_count edges)
    for i in 0..edge_count {
        let source_idx = i % node_count;
        // Distribute edges across nodes, ensuring different targets
        let target_idx = (source_idx + (i / node_count) + 1) % node_count;

        let source_id = NodeId::new(source_idx as u64).unwrap();
        let target_id = NodeId::new(target_idx as u64).unwrap();

        current
            .create_edge(
                source_id,
                target_id,
                "CONNECTS",
                PropertyMapBuilder::new().insert("weight", i as i64).build(),
            )
            .unwrap();
    }

    // Create checkpoint
    let checkpoint_config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(checkpoint_config).unwrap();

    let checkpoint_lsn = wal.current_lsn();
    manager
        .create_checkpoint(checkpoint_lsn, &current, &historical)
        .unwrap();

    (temp_dir, wal, manager)
}

/// Create a database with vector-indexed nodes, then checkpoint it.
///
/// Returns (temp_dir, wal, checkpoint_manager) for recovery benchmarking.
fn create_vector_checkpointed_database(
    node_count: usize,
    dimensions: usize,
) -> (TempDir, ConcurrentWalSystem, CheckpointManager) {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL system
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Create current storage with vector index enabled
    let current = CurrentStorage::new();

    // Enable vector index
    let vector_config = HnswConfig::new(dimensions, DistanceMetric::Cosine)
        .with_m(16)
        .with_ef_construction(200)
        .with_capacity(node_count);

    current
        .enable_vector_index("embedding", vector_config)
        .unwrap();

    // Create nodes with embeddings
    for i in 0..node_count {
        let embedding = generate_random_vector(dimensions);
        // Use modulo to cycle through a limited set of name values
        let name_id = i % 1000;
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("name", format!("VectorNode{}", name_id))
            .insert_vector("embedding", &embedding)
            .build();

        current.create_node("VectorNode", props).unwrap();
    }

    let historical = HistoricalStorage::new();

    // Create checkpoint (this will persist the vector index)
    let checkpoint_config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(checkpoint_config).unwrap();

    let checkpoint_lsn = wal.current_lsn();
    manager
        .create_checkpoint(checkpoint_lsn, &current, &historical)
        .unwrap();

    (temp_dir, wal, manager)
}

/// Create a WAL with operations but no checkpoint for WAL replay benchmarking.
fn create_wal_only_database(operation_count: usize) -> (TempDir, ConcurrentWalSystem) {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    // Create WAL system
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Write operations to WAL
    for i in 0..operation_count {
        let op = if i % 2 == 0 {
            // Create node operation
            WalOperation::CreateNode {
                node_id: NodeId::new(i as u64).unwrap(),
                label: GLOBAL_INTERNER.intern("WalNode").unwrap(),
                properties: PropertyMapBuilder::new().insert("id", i as i64).build(),
                temporal: BiTemporalInterval::current(time::now()),
            }
        } else {
            // Update node operation (update the previous node)
            // Version ID increments with each update (starts at 2 after creation)
            let node_id = (i - 1) as u64;
            let update_count = (i / 2) + 1; // How many times has this been updated
            WalOperation::UpdateNode {
                node_id: NodeId::new(node_id).unwrap(),
                version_id: gallifreydb::core::id::VersionId::new((update_count + 1) as u64)
                    .unwrap(),
                label: GLOBAL_INTERNER.intern("WalNode").unwrap(),
                properties: PropertyMapBuilder::new()
                    .insert("id", node_id as i64)
                    .insert("updated", true)
                    .build(),
                temporal: BiTemporalInterval::current(time::now()),
            }
        };

        wal.append(op).unwrap();

        // Flush every 1000 operations to simulate realistic batching
        if i % 1000 == 0 {
            wal.flush().unwrap();
        }
    }

    // Final flush
    wal.flush().unwrap();

    (temp_dir, wal)
}

// ============================================================================
// Benchmark 1: Small Dataset Recovery (100 nodes, 500 edges, target: <100ms)
// ============================================================================

fn bench_recovery_small_dataset(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery_small_dataset");
    group.sample_size(20); // Reduce sample size for faster benchmarking

    group.bench_function("100_nodes_500_edges", |b| {
        // Setup: Create checkpointed database once
        let (_temp_dir, wal, mut manager) = create_checkpointed_database(100, 500);

        b.iter(|| {
            // Benchmark: Recovery from checkpoint
            let (recovered_current, _recovered_historical, _final_lsn) =
                manager.recover(&wal).unwrap();

            // Verify recovery succeeded
            assert_eq!(
                recovered_current.node_count(),
                100,
                "Expected 100 nodes after recovery"
            );
            assert_eq!(
                recovered_current.edge_count(),
                500,
                "Expected 500 edges after recovery"
            );

            black_box(recovered_current);
        });
    });

    // Measure and verify performance target
    group.finish();
}

// ============================================================================
// Benchmark 2: Medium Dataset Recovery (10k nodes, 50k edges, target: <5s)
// ============================================================================

fn bench_recovery_medium_dataset(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery_medium_dataset");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30)); // Allow longer measurement time

    group.bench_function("10k_nodes_50k_edges", |b| {
        // Setup: Create checkpointed database once
        let (_temp_dir, wal, mut manager) = create_checkpointed_database(10_000, 50_000);

        b.iter(|| {
            // Benchmark: Recovery from checkpoint
            let (recovered_current, _recovered_historical, _final_lsn) =
                manager.recover(&wal).unwrap();

            // Verify recovery succeeded
            assert_eq!(
                recovered_current.node_count(),
                10_000,
                "Expected 10,000 nodes after recovery"
            );
            assert_eq!(
                recovered_current.edge_count(),
                50_000,
                "Expected 50,000 edges after recovery"
            );

            black_box(recovered_current);
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark 3: Large Dataset Recovery (100k nodes, 500k edges, target: <30s)
// ============================================================================

fn bench_recovery_large_dataset(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery_large_dataset");
    group.sample_size(10); // Minimum sample size for large dataset
    group.measurement_time(Duration::from_secs(60)); // Allow even longer measurement time

    group.bench_function("100k_nodes_500k_edges", |b| {
        // Setup: Create checkpointed database once
        let (_temp_dir, wal, mut manager) = create_checkpointed_database(100_000, 500_000);

        b.iter(|| {
            // Benchmark: Recovery from checkpoint
            let (recovered_current, _recovered_historical, _final_lsn) =
                manager.recover(&wal).unwrap();

            // Verify recovery succeeded
            assert_eq!(
                recovered_current.node_count(),
                100_000,
                "Expected 100,000 nodes after recovery"
            );
            assert_eq!(
                recovered_current.edge_count(),
                500_000,
                "Expected 500,000 edges after recovery"
            );

            black_box(recovered_current);
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark 4: WAL Replay (100k operations, target: <10s)
// ============================================================================

fn bench_recovery_wal_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery_wal_replay");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    // Setup: Create WAL with operations but no checkpoint (shared across iterations)
    let (_temp_dir, wal) = create_wal_only_database(100_000);

    group.bench_function("100k_operations", |b| {
        b.iter_batched(
            || {
                // Setup: Create unique temp dir and manager per iteration (not timed)
                let iter_dir = TempDir::new().unwrap();
                let checkpoint_config = UnifiedCheckpointConfig::with_data_dir(iter_dir.path());
                let manager = CheckpointManager::new(checkpoint_config).unwrap();
                (iter_dir, manager)
            },
            |(_iter_dir, mut manager)| {
                // Measured routine: Recovery from WAL only (no checkpoint)
                let (recovered_current, _recovered_historical, _final_lsn) =
                    manager.recover(&wal).unwrap();

                // Verify recovery succeeded (50k nodes created, 50k updates)
                assert_eq!(
                    recovered_current.node_count(),
                    50_000,
                    "Expected 50,000 nodes after WAL replay"
                );

                black_box(recovered_current);
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.finish();
}

// ============================================================================
// Benchmark 5: Vector-Indexed Recovery (10k nodes, 384-dim, target: <10s)
// ============================================================================

fn bench_recovery_vector_indexed(c: &mut Criterion) {
    let mut group = c.benchmark_group("recovery_vector_indexed");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("10k_nodes_384_dimensions", |b| {
        // Setup: Create vector-indexed database
        let (_temp_dir, wal, mut manager) = create_vector_checkpointed_database(10_000, 384);

        b.iter(|| {
            // Benchmark: Recovery with vector index rebuilding
            let (recovered_current, _recovered_historical, _final_lsn) =
                manager.recover(&wal).unwrap();

            // Verify recovery succeeded
            assert_eq!(
                recovered_current.node_count(),
                10_000,
                "Expected 10,000 nodes after vector recovery"
            );

            // Note: Vector index persistence/restoration is tested in unit tests.
            // This benchmark focuses on measuring recovery time performance.

            black_box(recovered_current);
        });
    });

    group.finish();
}

// ============================================================================
// Criterion Configuration
// ============================================================================

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_recovery_small_dataset,
        bench_recovery_medium_dataset,
        bench_recovery_large_dataset,
        bench_recovery_wal_replay,
        bench_recovery_vector_indexed
);

criterion_main!(benches);
