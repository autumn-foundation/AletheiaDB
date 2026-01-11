//! Performance Targets Verification Benchmark
//!
//! This lightweight benchmark suite validates the performance targets
//! defined in benchmarks/performance-targets.json. It's designed to run quickly
//! in CI (<30 seconds) to catch regressions without the overhead of the full suite.
//!
//! **Targets Validated**:
//! - Current-state single-hop traversal (<1µs)
//! - Current-state 3-hop traversal (<100µs)
//! - Batch insertion throughput (>100k edges/sec)
//! - Time-travel at anchor (<100µs)
//! - Time-travel with deltas (avg 5) (<1ms)
//! - Time-travel worst case (9 deltas) (<5ms)
//!
//! **Targets NOT Validated** (require complex temporal setup):
//! - Storage overhead → use full benchmark suite
//!
//! Run in CI:
//!   - On PRs: Quick smoke test to verify no major regressions
//!   - On scheduled runs: Full validation with comprehensive suite

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::api::transaction::WriteOps;
use gallifreydb::{CurrentStorage, GallifreyDB, PropertyMapBuilder};

/// Target: Current-state single-hop traversal <1µs
fn bench_single_hop_target(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_single_hop");

    // Create a small graph for single-hop testing
    let storage = CurrentStorage::new();
    let node1 = storage
        .create_node("Person", PropertyMapBuilder::new().insert("id", 1).build())
        .unwrap();
    let node2 = storage
        .create_node("Person", PropertyMapBuilder::new().insert("id", 2).build())
        .unwrap();
    storage
        .create_edge(node1, node2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    group.bench_function("traverse_one_hop", |b| {
        b.iter(|| {
            let edges = storage.get_outgoing_edges(black_box(node1));
            black_box(edges.len())
        })
    });

    group.finish();
}

/// Target: Current-state 3-hop traversal <100µs
fn bench_3_hop_target(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_3_hop");

    // Create a chain for 3-hop testing: n1 -> n2 -> n3 -> n4
    let storage = CurrentStorage::new();
    let mut nodes = Vec::new();
    for i in 0..4 {
        nodes.push(
            storage
                .create_node("Person", PropertyMapBuilder::new().insert("id", i).build())
                .unwrap(),
        );
    }

    // Create chain
    for i in 0..3 {
        storage
            .create_edge(
                nodes[i],
                nodes[i + 1],
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
    }

    group.bench_function("traverse_three_hops", |b| {
        b.iter(|| {
            let start_node = black_box(nodes[0]);

            // Perform 3-hop traversal using get_outgoing_targets for efficiency
            let mut current_nodes = vec![start_node];
            for _ in 0..3 {
                current_nodes = current_nodes
                    .iter()
                    .flat_map(|&node| storage.get_outgoing_targets(node))
                    .collect();
            }

            black_box(current_nodes.len())
        })
    });

    group.finish();
}

/// Target: Batch insertion throughput >100k edges/sec
fn bench_batch_insertion_target(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_batch_insertion");

    // Configure for faster iterations since this measures throughput
    group.sample_size(20);

    group.bench_function("insert_1000_edges", |b| {
        b.iter(|| {
            let storage = CurrentStorage::new();

            // Create 100 nodes
            let nodes: Vec<_> = (0..100)
                .map(|i| {
                    storage
                        .create_node("Person", PropertyMapBuilder::new().insert("id", i).build())
                        .unwrap()
                })
                .collect();

            // Create 1000 edges (each node connects to 10 others)
            for i in 0..100 {
                for j in 0..10 {
                    let target = (i + j + 1) % 100;
                    storage
                        .create_edge(
                            nodes[i],
                            nodes[target],
                            "KNOWS",
                            PropertyMapBuilder::new().build(),
                        )
                        .unwrap();
                }
            }

            // Return edge count to prevent optimization
            black_box(storage.edge_count())
        })
    });

    group.finish();
}

/// Target: Time-travel at anchor <100µs
/// This tests the best-case scenario where we reconstruct directly from an anchor
///
/// Version/Timestamp Semantics:
/// - create_node happens at tx_time=0 (creates current state only, no historical version)
/// - First update creates v1 at tx_time=1 (first historical version, anchor)
/// - Second update creates v2 at tx_time=2 (delta)
/// - ...
/// - 10th update creates v10 at tx_time=10 (anchor)
/// - Anchor interval is 10, so anchors are created at tx_time 1, 11, 21, etc.
/// - In this benchmark, anchors are at tx_time 1 and 10 (after 10 updates)
fn bench_time_travel_at_anchor(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel");

    // Setup: Create database with anchored versions
    let db = GallifreyDB::new();
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Benchmark setup: create_node should succeed with valid input");

    // Create 10 additional versions (v1-v10)
    // This creates anchors at tx_time=0 (initial create) and tx_time=10
    for i in 1..=10 {
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("version", i)
                    .build(),
            )?;
            Ok(())
        })
        .expect("Benchmark setup: update_node should succeed with valid input");
    }

    group.bench_function("at_anchor", |b| {
        b.iter(|| {
            // Query at anchor point (tx_time=10, valid_time=10)
            // This should hit the anchor directly with no delta reconstruction
            let result = db.get_node_at_time(black_box(node_id), black_box(10), black_box(10));
            black_box(result)
        })
    });

    group.finish();
}

/// Target: Time-travel with deltas (avg 5) <1ms
/// This tests mid-range reconstruction requiring delta application
///
/// Query at tx_time=5 requires:
/// 1. Find nearest anchor ≤ 5 → anchor@tx_time=1
/// 2. Apply deltas for tx_time 2, 3, 4, 5
/// 3. Reconstruct state at tx_time=5 (4 deltas applied)
fn bench_time_travel_with_deltas(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel");

    // Setup: Create database with 15 versions
    // Anchors at tx_time 1, 11 (default anchor_interval = 10, after 15 updates)
    let db = GallifreyDB::new();
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Benchmark setup: create_node should succeed with valid input");

    // Create 15 additional versions (tx_time 1-15)
    for i in 1..=15 {
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("version", i)
                    .build(),
            )?;
            Ok(())
        })
        .expect("Benchmark setup: update_node should succeed with valid input");
    }

    group.bench_function("with_5_deltas", |b| {
        b.iter(|| {
            // Query at tx_time=5, valid_time=5
            // This requires: anchor@tx_time=0 + 5 deltas (tx_time 1-5)
            let result = db.get_node_at_time(black_box(node_id), black_box(5), black_box(5));
            black_box(result)
        })
    });

    group.finish();
}

/// Target: Time-travel worst case (9 deltas) <5ms
/// This tests worst-case reconstruction just before next anchor
///
/// Query at tx_time=9 (just before anchor@10) requires:
/// 1. Find nearest anchor ≤ 9 → anchor@tx_time=1
/// 2. Apply maximum delta chain: tx_time 2, 3, 4, 5, 6, 7, 8, 9
/// 3. Reconstruct state at tx_time=9 (8 deltas applied)
///
/// This is the worst case for anchor_interval=10 (9 versions between anchors)
fn bench_time_travel_worst_case(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel");

    // Setup: Create database with 19 versions
    // Anchors at tx_time 1, 11 (default anchor_interval = 10, after 19 updates)
    let db = GallifreyDB::new();
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Benchmark setup: create_node should succeed with valid input");

    // Create 19 additional versions (tx_time 1-19)
    for i in 1..=19 {
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("version", i)
                    .build(),
            )?;
            Ok(())
        })
        .expect("Benchmark setup: update_node should succeed with valid input");
    }

    group.bench_function("worst_case_9_deltas", |b| {
        b.iter(|| {
            // Query at tx_time=9, valid_time=9
            // Worst case: anchor@tx_time=0 + 9 deltas (tx_time 1-9), just before next anchor@10
            let result = db.get_node_at_time(black_box(node_id), black_box(9), black_box(9));
            black_box(result)
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_single_hop_target,
    bench_3_hop_target,
    bench_batch_insertion_target,
    bench_time_travel_at_anchor,
    bench_time_travel_with_deltas,
    bench_time_travel_worst_case,
);

criterion_main!(benches);
