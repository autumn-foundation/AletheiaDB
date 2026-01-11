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
use gallifreydb::{CurrentStorage, GallifreyDB, PropertyMapBuilder};
use gallifreydb::api::transaction::WriteOps;

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
fn bench_time_travel_at_anchor(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel");

    // Setup: Create database with anchored versions
    // Anchor interval is 10, so anchors are at v0, v10, v20, etc.
    let db = GallifreyDB::new();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().insert("name", "Alice").build())
        .unwrap();

    // Create 10 additional versions (v1-v10)
    // This creates anchors at v0 (initial create) and v10
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
        .unwrap();
    }

    group.bench_function("at_anchor", |b| {
        b.iter(|| {
            // Query at anchor point v10 (transaction time 10)
            // This should hit the anchor directly with no delta reconstruction
            let result = db.get_node_at_time(black_box(node_id), black_box(10), black_box(10));
            black_box(result)
        })
    });

    group.finish();
}

/// Target: Time-travel with deltas (avg 5) <1ms
/// This tests mid-range reconstruction requiring delta application
fn bench_time_travel_with_deltas(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel");

    // Setup: Create database with 15 versions
    // Anchors at v0, v10, v20 (default anchor_interval = 10)
    let db = GallifreyDB::new();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().insert("name", "Alice").build())
        .unwrap();

    // Create 15 additional versions (v1-v15)
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
        .unwrap();
    }

    group.bench_function("with_5_deltas", |b| {
        b.iter(|| {
            // Query at v5 (transaction time 5)
            // This requires: anchor@v0 + 5 deltas (v1, v2, v3, v4, v5)
            let result = db.get_node_at_time(black_box(node_id), black_box(5), black_box(5));
            black_box(result)
        })
    });

    group.finish();
}

/// Target: Time-travel worst case (9 deltas) <5ms
/// This tests worst-case reconstruction just before next anchor
fn bench_time_travel_worst_case(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel");

    // Setup: Create database with 19 versions
    // Anchors at v0, v10, v20 (default anchor_interval = 10)
    let db = GallifreyDB::new();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().insert("name", "Alice").build())
        .unwrap();

    // Create 19 additional versions (v1-v19)
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
        .unwrap();
    }

    group.bench_function("worst_case_9_deltas", |b| {
        b.iter(|| {
            // Query at v9 (transaction time 9)
            // This is worst case: anchor@v0 + 9 deltas (v1-v9), just before next anchor@v10
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
