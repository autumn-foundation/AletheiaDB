//! Large dataset recovery tests (Issue #294 - Scenario 6)
//!
//! These tests verify that recovery can handle large datasets efficiently:
//! - 10,000 nodes and 50,000 edges
//! - Recovery time target: under 10 seconds
//! - Memory efficiency during replay
//!
//! These tests are marked as `#[ignore]` by default because they are resource-intensive.
//! Run with: `cargo test --test recovery large_dataset -- --ignored`

use gallifreydb::{
    GLOBAL_INTERNER,
    core::{
        id::{EdgeId, NodeId},
        property::{PropertyMap, PropertyMapBuilder},
        temporal::{BiTemporalInterval, time},
    },
    storage::{
        persistence::{CheckpointConfig, PersistenceManager},
        wal::{
            WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
    utils::error::Result,
};
use std::time::Instant;
use tempfile::TempDir;

/// Target recovery time for large datasets (10 seconds)
const RECOVERY_TIME_TARGET_SECS: u64 = 10;

/// Number of nodes for large dataset tests
const LARGE_DATASET_NODE_COUNT: u64 = 10_000;

/// Number of edges for large dataset tests
const LARGE_DATASET_EDGE_COUNT: u64 = 50_000;

#[test]
#[ignore] // Run with: cargo test --test recovery large_dataset_recovery_10k_nodes -- --ignored
fn test_large_dataset_recovery_10k_nodes() -> Result<()> {
    // Given: 10,000 nodes in WAL
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    println!("Creating {} nodes...", LARGE_DATASET_NODE_COUNT);
    let create_start = Instant::now();

    for i in 1..=LARGE_DATASET_NODE_COUNT {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("LargeNode").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("index", i as i64)
                .insert("batch", (i / 1000) as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        if i % 1000 == 0 {
            println!("  Created {} nodes...", i);
        }
    }
    wal.flush()?;

    let create_duration = create_start.elapsed();
    println!(
        "Created {} nodes in {:.2}s",
        LARGE_DATASET_NODE_COUNT,
        create_duration.as_secs_f64()
    );

    // When: Recover from WAL
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };

    let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
    let wal2 = ConcurrentWalSystem::new(wal_config2)?;

    println!("Starting recovery...");
    let recovery_start = Instant::now();

    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal2)?;

    let recovery_duration = recovery_start.elapsed();
    println!(
        "Recovery completed in {:.2}s",
        recovery_duration.as_secs_f64()
    );

    // Then: All nodes should be recovered
    assert_eq!(
        current.node_count(),
        LARGE_DATASET_NODE_COUNT as usize,
        "All {} nodes should be recovered",
        LARGE_DATASET_NODE_COUNT
    );

    // Verify historical versions
    let hist_stats = historical.stats();
    assert_eq!(
        hist_stats.total_node_versions,
        LARGE_DATASET_NODE_COUNT as usize
    );

    // Verify recovery time target
    assert!(
        recovery_duration.as_secs() < RECOVERY_TIME_TARGET_SECS,
        "Recovery should complete in under {} seconds, took {:.2}s",
        RECOVERY_TIME_TARGET_SECS,
        recovery_duration.as_secs_f64()
    );

    // Sample verification
    for i in [1, 1000, 5000, 10000] {
        let node = current.get_node(NodeId::new(i).unwrap())?;
        assert!(node.has_label_str("LargeNode"));
    }

    Ok(())
}

#[test]
#[ignore] // Run with: cargo test --test recovery large_dataset_recovery_50k_edges -- --ignored
fn test_large_dataset_recovery_50k_edges() -> Result<()> {
    // Given: Nodes and 50,000 edges in WAL
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create enough nodes for edges (need at least sqrt(50000) * 2 = ~500 nodes for random edges)
    // We'll use 1000 nodes for better edge distribution
    let node_count = 1000_u64;

    println!("Creating {} nodes...", node_count);
    for i in 1..=node_count {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Node").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    println!("Creating {} edges...", LARGE_DATASET_EDGE_COUNT);
    let create_start = Instant::now();

    for i in 1..=LARGE_DATASET_EDGE_COUNT {
        // Create edges with deterministic but varied connectivity
        let source = ((i - 1) % node_count) + 1;
        let target = (((i - 1) * 7) % node_count) + 1; // Different pattern for target

        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(i).unwrap(),
            source: NodeId::new(source).unwrap(),
            target: NodeId::new(target).unwrap(),
            label: GLOBAL_INTERNER.intern("CONNECTS").unwrap(),
            properties: PropertyMapBuilder::new().insert("index", i as i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        if i % 10000 == 0 {
            println!("  Created {} edges...", i);
        }
    }
    wal.flush()?;

    let create_duration = create_start.elapsed();
    println!(
        "Created {} edges in {:.2}s",
        LARGE_DATASET_EDGE_COUNT,
        create_duration.as_secs_f64()
    );

    // When: Recover
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };

    let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
    let wal2 = ConcurrentWalSystem::new(wal_config2)?;

    println!("Starting recovery...");
    let recovery_start = Instant::now();

    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal2)?;

    let recovery_duration = recovery_start.elapsed();
    println!(
        "Recovery completed in {:.2}s",
        recovery_duration.as_secs_f64()
    );

    // Then: All nodes and edges should be recovered
    assert_eq!(current.node_count(), node_count as usize);
    assert_eq!(
        current.edge_count(),
        LARGE_DATASET_EDGE_COUNT as usize,
        "All {} edges should be recovered",
        LARGE_DATASET_EDGE_COUNT
    );

    // Verify historical versions
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, node_count as usize);
    assert_eq!(
        hist_stats.total_edge_versions,
        LARGE_DATASET_EDGE_COUNT as usize
    );

    // Verify recovery time target
    assert!(
        recovery_duration.as_secs() < RECOVERY_TIME_TARGET_SECS,
        "Recovery should complete in under {} seconds, took {:.2}s",
        RECOVERY_TIME_TARGET_SECS,
        recovery_duration.as_secs_f64()
    );

    Ok(())
}

#[test]
#[ignore] // Run with: cargo test --test recovery large_dataset_full -- --ignored
fn test_large_dataset_full_recovery() -> Result<()> {
    // Given: 10,000 nodes and 50,000 edges (full test from Issue #294)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    println!("=== Large Dataset Recovery Test ===");
    println!(
        "Target: {} nodes, {} edges",
        LARGE_DATASET_NODE_COUNT, LARGE_DATASET_EDGE_COUNT
    );
    println!(
        "Recovery time target: {} seconds\n",
        RECOVERY_TIME_TARGET_SECS
    );

    let total_start = Instant::now();

    // Create nodes
    println!("Phase 1: Creating {} nodes...", LARGE_DATASET_NODE_COUNT);
    let node_start = Instant::now();

    for i in 1..=LARGE_DATASET_NODE_COUNT {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Entity").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert("type", "node")
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        if i % 2000 == 0 {
            print!(".");
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
    }
    println!(" Done ({:.2}s)", node_start.elapsed().as_secs_f64());

    // Create edges
    println!("Phase 2: Creating {} edges...", LARGE_DATASET_EDGE_COUNT);
    let edge_start = Instant::now();

    for i in 1..=LARGE_DATASET_EDGE_COUNT {
        // Create varied connectivity pattern
        let source = ((i - 1) % LARGE_DATASET_NODE_COUNT) + 1;
        let target = (((i - 1) * 31) % LARGE_DATASET_NODE_COUNT) + 1; // Prime multiplier for variety

        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(i).unwrap(),
            source: NodeId::new(source).unwrap(),
            target: NodeId::new(target).unwrap(),
            label: GLOBAL_INTERNER.intern("RELATES_TO").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("weight", (i % 100) as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;

        if i % 10000 == 0 {
            print!(".");
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
    }
    println!(" Done ({:.2}s)", edge_start.elapsed().as_secs_f64());

    wal.flush()?;

    let total_create_time = total_start.elapsed();
    println!(
        "\nTotal creation time: {:.2}s\n",
        total_create_time.as_secs_f64()
    );

    // Recovery phase
    println!("Phase 3: Recovery...");
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };

    let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
    let wal2 = ConcurrentWalSystem::new(wal_config2)?;

    let recovery_start = Instant::now();

    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, final_lsn) = manager.recover(&wal2)?;

    let recovery_duration = recovery_start.elapsed();

    // Print results
    println!("\n=== Recovery Results ===");
    println!("Recovery time: {:.2}s", recovery_duration.as_secs_f64());
    println!("Final LSN: {:?}", final_lsn);
    println!("Nodes recovered: {}", current.node_count());
    println!("Edges recovered: {}", current.edge_count());

    let hist_stats = historical.stats();
    println!(
        "Historical node versions: {}",
        hist_stats.total_node_versions
    );
    println!(
        "Historical edge versions: {}",
        hist_stats.total_edge_versions
    );

    // Calculate throughput
    let total_operations = LARGE_DATASET_NODE_COUNT + LARGE_DATASET_EDGE_COUNT;
    let ops_per_sec = total_operations as f64 / recovery_duration.as_secs_f64();
    println!("Recovery throughput: {:.0} ops/sec", ops_per_sec);

    // Assertions
    assert_eq!(
        current.node_count(),
        LARGE_DATASET_NODE_COUNT as usize,
        "All nodes should be recovered"
    );
    assert_eq!(
        current.edge_count(),
        LARGE_DATASET_EDGE_COUNT as usize,
        "All edges should be recovered"
    );
    assert_eq!(
        hist_stats.total_node_versions,
        LARGE_DATASET_NODE_COUNT as usize
    );
    assert_eq!(
        hist_stats.total_edge_versions,
        LARGE_DATASET_EDGE_COUNT as usize
    );

    // Recovery time assertion
    assert!(
        recovery_duration.as_secs() < RECOVERY_TIME_TARGET_SECS,
        "FAILED: Recovery took {:.2}s, target is {} seconds",
        recovery_duration.as_secs_f64(),
        RECOVERY_TIME_TARGET_SECS
    );

    println!("\n=== Test PASSED ===");

    Ok(())
}

#[test]
#[ignore] // Run with: cargo test --test recovery large_dataset_with_updates -- --ignored
fn test_large_dataset_with_updates() -> Result<()> {
    // Given: Large dataset with updates (tests version chain efficiency)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_count = 5000_u64;
    let updates_per_node = 3_u64; // Each node gets 3 updates

    println!(
        "Creating {} nodes with {} updates each...",
        node_count, updates_per_node
    );
    let start = Instant::now();

    // Create nodes
    for i in 1..=node_count {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("VersionedNode").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("version", 0_i64)
                .insert("value", (i * 10) as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Update each node multiple times
    let mut version_id = node_count + 1;
    for update_round in 1..=updates_per_node {
        for i in 1..=node_count {
            wal.append(WalOperation::UpdateNode {
                node_id: NodeId::new(i).unwrap(),
                version_id: gallifreydb::core::id::VersionId::new(version_id).unwrap(),
                label: GLOBAL_INTERNER.intern("VersionedNode").unwrap(),
                properties: PropertyMapBuilder::new()
                    .insert("version", update_round as i64)
                    .insert("value", (i * 10 + update_round) as i64)
                    .build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
            version_id += 1;
        }
        println!("  Completed update round {}", update_round);
    }

    wal.flush()?;
    println!("Data creation: {:.2}s", start.elapsed().as_secs_f64());

    // Recovery
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };

    let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
    let wal2 = ConcurrentWalSystem::new(wal_config2)?;

    println!("Starting recovery...");
    let recovery_start = Instant::now();

    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal2)?;

    let recovery_duration = recovery_start.elapsed();
    println!("Recovery time: {:.2}s", recovery_duration.as_secs_f64());

    // Verify
    assert_eq!(current.node_count(), node_count as usize);

    // Each node should be at final version
    let sample_node = current.get_node(NodeId::new(1).unwrap())?;
    use gallifreydb::core::property::PropertyValue;
    assert!(matches!(
        sample_node.properties.get("version"),
        Some(PropertyValue::Int(v)) if *v == updates_per_node as i64
    ));

    // Historical versions: creates + (updates_per_node * node_count)
    let expected_versions = node_count + (updates_per_node * node_count);
    let hist_stats = historical.stats();
    assert_eq!(
        hist_stats.total_node_versions, expected_versions as usize,
        "Should have {} historical versions",
        expected_versions
    );

    // Verify recovery time
    assert!(
        recovery_duration.as_secs() < RECOVERY_TIME_TARGET_SECS,
        "Recovery should complete in under {} seconds, took {:.2}s",
        RECOVERY_TIME_TARGET_SECS,
        recovery_duration.as_secs_f64()
    );

    Ok(())
}

#[test]
#[ignore] // Run with: cargo test --test recovery large_dataset_with_deletions -- --ignored
fn test_large_dataset_with_deletions() -> Result<()> {
    // Given: Large dataset with many deletions (tests tombstone handling)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_count = 10000_u64;
    let deletion_percentage = 50; // Delete 50% of nodes

    println!(
        "Creating {} nodes, then deleting {}%...",
        node_count, deletion_percentage
    );
    let start = Instant::now();

    // Create all nodes
    for i in 1..=node_count {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Node").unwrap(),
            properties: PropertyMapBuilder::new().insert("index", i as i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Delete every other node (50%)
    let nodes_to_delete = node_count * deletion_percentage / 100;
    for i in 1..=nodes_to_delete {
        let node_id = i * 2; // Delete even-numbered nodes
        wal.append(WalOperation::DeleteNode {
            node_id: NodeId::new(node_id).unwrap(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    wal.flush()?;
    println!("Data creation: {:.2}s", start.elapsed().as_secs_f64());

    // Recovery
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };

    let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
    let wal2 = ConcurrentWalSystem::new(wal_config2)?;

    println!("Starting recovery...");
    let recovery_start = Instant::now();

    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal2)?;

    let recovery_duration = recovery_start.elapsed();
    println!("Recovery time: {:.2}s", recovery_duration.as_secs_f64());

    // Verify
    let expected_remaining = node_count - nodes_to_delete;
    assert_eq!(
        current.node_count(),
        expected_remaining as usize,
        "Should have {} nodes remaining after deletion",
        expected_remaining
    );

    // Verify deleted nodes don't exist
    assert!(current.get_node(NodeId::new(2).unwrap()).is_err());
    assert!(current.get_node(NodeId::new(100).unwrap()).is_err());

    // Verify remaining nodes exist
    assert!(current.get_node(NodeId::new(1).unwrap()).is_ok());
    assert!(current.get_node(NodeId::new(99).unwrap()).is_ok());

    // Historical: creates + delete tombstones
    let expected_versions = node_count + nodes_to_delete;
    let hist_stats = historical.stats();
    assert_eq!(
        hist_stats.total_node_versions, expected_versions as usize,
        "Should have {} historical versions (creates + tombstones)",
        expected_versions
    );

    // Verify recovery time
    assert!(
        recovery_duration.as_secs() < RECOVERY_TIME_TARGET_SECS,
        "Recovery should complete in under {} seconds, took {:.2}s",
        RECOVERY_TIME_TARGET_SECS,
        recovery_duration.as_secs_f64()
    );

    Ok(())
}

// ============================================================================
// Benchmarking utilities
// ============================================================================

#[test]
#[ignore]
fn bench_recovery_throughput() -> Result<()> {
    // Benchmark recovery throughput at various dataset sizes
    let sizes = [1000, 5000, 10000, 25000];

    println!("=== Recovery Throughput Benchmark ===\n");
    println!(
        "{:>10} | {:>12} | {:>15}",
        "Nodes", "Time (s)", "Throughput"
    );
    println!("{:-<10}-+-{:-<12}-+-{:-<15}", "", "", "");

    for &size in &sizes {
        let temp_dir = TempDir::new().unwrap();
        let wal_dir = temp_dir.path().join("wal");

        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create nodes
        for i in 1..=size {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i as u64).unwrap(),
                label: GLOBAL_INTERNER.intern("BenchNode").unwrap(),
                properties: PropertyMapBuilder::new().insert("i", i as i64).build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Measure recovery
        let config = CheckpointConfig {
            checkpoint_dir: temp_dir.path().join("checkpoints"),
            ..Default::default()
        };

        let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
        let wal2 = ConcurrentWalSystem::new(wal_config2)?;

        let start = Instant::now();
        let mut manager = PersistenceManager::new(config)?;
        let (current, _, _) = manager.recover(&wal2)?;
        let duration = start.elapsed();

        assert_eq!(current.node_count(), size);

        let throughput = size as f64 / duration.as_secs_f64();
        println!(
            "{:>10} | {:>12.3} | {:>12.0} ops/s",
            size,
            duration.as_secs_f64(),
            throughput
        );
    }

    println!("\n=== Benchmark Complete ===");

    Ok(())
}
