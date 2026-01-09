//! Performance Targets Verification Benchmark
//!
//! This lightweight benchmark suite specifically validates the performance targets
//! defined in benchmarks/performance-targets.json. It's designed to run quickly
//! in CI to catch regressions without the overhead of the full benchmark suite.
//!
//! Run in CI:
//!   - On PRs: Quick smoke test to verify no major regressions
//!   - On scheduled runs: Full validation of all targets
//!
//! For comprehensive benchmarking, use the full benchmark suites:
//!   - benches/current_state.rs
//!   - benches/temporal_query.rs
//!   - benches/durability_modes.rs
//!   - etc.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::api::transaction::WriteOps;
use gallifreydb::storage::version::AnchorConfig;
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

            // Hop 1
            let hop1_edges = storage.get_outgoing_edges(start_node);
            let mut hop1_targets = Vec::new();
            for edge_id in &hop1_edges {
                if let Ok(edge) = storage.get_edge(*edge_id) {
                    hop1_targets.push(edge.target);
                }
            }

            // Hop 2
            let mut hop2_targets = Vec::new();
            for node in &hop1_targets {
                let hop2_edges = storage.get_outgoing_edges(*node);
                for edge_id in &hop2_edges {
                    if let Ok(edge) = storage.get_edge(*edge_id) {
                        hop2_targets.push(edge.target);
                    }
                }
            }

            // Hop 3
            let mut hop3_targets = Vec::new();
            for node in &hop2_targets {
                let hop3_edges = storage.get_outgoing_edges(*node);
                for edge_id in &hop3_edges {
                    if let Ok(edge) = storage.get_edge(*edge_id) {
                        hop3_targets.push(edge.target);
                    }
                }
            }

            black_box(hop3_targets.len())
        })
    });

    group.finish();
}

/// Target: Time-travel at anchor <100µs
fn bench_time_travel_anchor_target(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel_anchor");

    let config = AnchorConfig {
        anchor_interval: 10,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(config);

    // Create a node and update it 10 times to create an anchor
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new().insert("version", 0).build(),
        )
        .unwrap();

    // Update the node 10 times to create an anchor
    for i in 1..=10 {
        db.write(|tx| {
            let props = PropertyMapBuilder::new().insert("version", i).build();
            tx.update_node(node_id, props)?;
            Ok(())
        })
        .unwrap();
    }

    let anchor_timestamp = 10000i64; // Query at the anchor point (version 10)

    group.bench_function("query_at_anchor", |b| {
        b.iter(|| {
            let node = db
                .get_node_at_time(
                    black_box(node_id),
                    black_box(anchor_timestamp),
                    black_box(anchor_timestamp),
                )
                .unwrap();
            black_box(node)
        })
    });

    group.finish();
}

/// Target: Time-travel with deltas (avg 5) <1ms
fn bench_time_travel_deltas_target(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel_deltas");

    let config = AnchorConfig {
        anchor_interval: 10,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(config);

    // Create a node and update it 15 times (anchor at 0, 10; query at 15 = 5 deltas)
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new().insert("version", 0).build(),
        )
        .unwrap();

    for i in 1..=15 {
        db.write(|tx| {
            let props = PropertyMapBuilder::new().insert("version", i).build();
            tx.update_node(node_id, props)?;
            Ok(())
        })
        .unwrap();
    }

    // Query at version 15 (5 deltas from anchor at version 10)
    let query_timestamp = 15000i64;

    group.bench_function("query_with_5_deltas", |b| {
        b.iter(|| {
            let node = db
                .get_node_at_time(
                    black_box(node_id),
                    black_box(query_timestamp),
                    black_box(query_timestamp),
                )
                .unwrap();
            black_box(node)
        })
    });

    group.finish();
}

/// Target: Time-travel worst case (9 deltas) <5ms
fn bench_time_travel_worst_case_target(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_time_travel_worst_case");

    let config = AnchorConfig {
        anchor_interval: 10,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(config);

    // Create a node and update it 19 times (anchor at 0, 10; query at 19 = 9 deltas)
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new().insert("version", 0).build(),
        )
        .unwrap();

    for i in 1..=19 {
        db.write(|tx| {
            let props = PropertyMapBuilder::new().insert("version", i).build();
            tx.update_node(node_id, props)?;
            Ok(())
        })
        .unwrap();
    }

    // Query at version 19 (9 deltas from anchor at version 10)
    let query_timestamp = 19000i64;

    group.bench_function("query_with_9_deltas", |b| {
        b.iter(|| {
            let node = db
                .get_node_at_time(
                    black_box(node_id),
                    black_box(query_timestamp),
                    black_box(query_timestamp),
                )
                .unwrap();
            black_box(node)
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

            // Return edge count instead of reference to storage
            black_box(nodes.len())
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_single_hop_target,
    bench_3_hop_target,
    bench_time_travel_anchor_target,
    bench_time_travel_deltas_target,
    bench_time_travel_worst_case_target,
    bench_batch_insertion_target,
);

criterion_main!(benches);
