//! Benchmarks for GallifreyDB with temporal queries.
//!
//! These benchmarks test:
//! - Fast path: Current state queries (should match current_state.rs performance)
//! - Slow path: Time-travel queries with version reconstruction
//! - Version creation: Overhead of maintaining temporal history
//!
//! NOTE: These benchmarks use Async durability mode to isolate graph performance
//! from WAL flush overhead. See durability_modes.rs for durability-focused benchmarks.

mod common;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::{
    GallifreyDB, NodeId, PropertyMapBuilder, WalConfigBuilder, WriteOps,
    storage::wal::DurabilityMode,
};

/// Create a test database configured for benchmarking (Async mode, no waiting).
fn create_benchmark_db() -> GallifreyDB {
    let wal_config = WalConfigBuilder::new()
        .durability_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        })
        .build();
    GallifreyDB::with_wal_config(wal_config).unwrap()
}

/// Create a test database with versioned history.
///
/// Creates nodes and then updates them multiple times to build version chains.
fn create_versioned_graph(
    node_count: usize,
    versions_per_node: usize,
) -> (GallifreyDB, Vec<NodeId>) {
    let db = create_benchmark_db();
    let mut node_ids = Vec::new();

    // Create initial nodes
    for i in 0..node_count {
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("value", 0i64)
            .build();

        let node_id = db.create_node("Node", props).unwrap();
        node_ids.push(node_id);
    }

    // Create edges between nodes
    for i in 0..node_count {
        let target = (i + 1) % node_count;
        db.create_edge(
            node_ids[i],
            node_ids[target],
            "LINKS_TO",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();
    }

    // Create version history by updating nodes
    for version in 1..versions_per_node {
        for &node_id in &node_ids {
            let mut tx = db.write_transaction().unwrap();
            let new_props = PropertyMapBuilder::new()
                .insert("id", node_id.as_u64() as i64)
                .insert("value", version as i64)
                .insert("version", version as i64)
                .build();
            tx.update_node(node_id, new_props).unwrap();
            tx.commit().unwrap();
        }
    }

    (db, node_ids)
}

/// Benchmark node creation (with versioning overhead).
fn bench_node_creation_with_versioning(c: &mut Criterion) {
    c.bench_function("gallifreydb_node_creation", |b| {
        b.iter_batched(
            create_benchmark_db,
            |db| {
                let props = PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build();
                let node = db.create_node("Person", props);
                black_box(node)
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark edge creation (with versioning overhead).
fn bench_edge_creation_with_versioning(c: &mut Criterion) {
    c.bench_function("gallifreydb_edge_creation", |b| {
        b.iter_batched(
            || {
                let db = create_benchmark_db();
                let n1 = db
                    .create_node("Person", PropertyMapBuilder::new().build())
                    .unwrap();
                let n2 = db
                    .create_node("Person", PropertyMapBuilder::new().build())
                    .unwrap();
                (db, n1, n2)
            },
            |(db, n1, n2)| {
                let props = PropertyMapBuilder::new().insert("since", 2020i64).build();
                let edge = db.create_edge(n1, n2, "KNOWS", props);
                black_box(edge)
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark current state queries (fast path).
fn bench_current_state_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("gallifreydb_fast_path");

    for graph_size in [100, 1000, 10000] {
        let (db, node_ids) = create_versioned_graph(graph_size, 1);
        let first_node = node_ids[0];

        group.bench_with_input(
            BenchmarkId::new("node_lookup", graph_size),
            &(db, first_node),
            |b, (db, node_id)| {
                b.iter(|| {
                    let node = db.get_node(black_box(*node_id));
                    black_box(node)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark single-hop traversal through the high-level API.
///
/// Includes transaction overhead and API layer. Compare to
/// `current_state::bench_single_hop_traversal` for raw storage performance
/// without API layer overhead.
fn bench_api_single_hop_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("gallifreydb_single_hop");

    for graph_size in [100, 1000, 10000] {
        let (db, node_ids) = create_versioned_graph(graph_size, 1);
        let first_node = node_ids[0];

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_nodes", graph_size)),
            &(db, first_node),
            |b, (db, node_id)| {
                b.iter(|| {
                    let edges = db.get_outgoing_edges(black_box(*node_id));
                    black_box(edges)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 3-hop traversal through the high-level API.
///
/// Includes transaction overhead and API layer. Compare to
/// `current_state::bench_multi_hop_traversal` for raw storage performance
/// without API layer overhead.
fn bench_api_multi_hop_traversal(c: &mut Criterion) {
    let (db, node_ids) = create_versioned_graph(1000, 1);
    let start_node = node_ids[0];

    c.bench_function("gallifreydb_3_hop_traversal", |b| {
        b.iter(|| {
            let mut visited = 0;

            // 1st hop
            let hop1 = db.get_outgoing_edges(black_box(start_node));
            for edge_id in &hop1 {
                if let Ok(edge) = db.get_edge(*edge_id) {
                    // 2nd hop
                    let hop2 = db.get_outgoing_edges(edge.target);
                    for edge_id2 in &hop2 {
                        if let Ok(edge2) = db.get_edge(*edge_id2) {
                            // 3rd hop
                            let hop3 = db.get_outgoing_edges(edge2.target);
                            visited += hop3.len();
                        }
                    }
                }
            }

            black_box(visited)
        });
    });
}

/// Benchmark time-travel queries (slow path).
///
/// Tests historical reconstruction with anchor+delta system.
/// With anchor interval of 10, querying at v15 requires:
/// 1. Find anchor at v10
/// 2. Apply 5 deltas to reconstruct state at v15
fn bench_time_travel_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("time_travel");

    // Create graph with 50 versions per node (tests anchor+delta)
    // This will create anchors at v10, v20, v30, v40
    let (db, node_ids) = create_versioned_graph(100, 50);
    let node_id = node_ids[0];

    // Benchmark querying at an anchor point (should be fast - direct lookup)
    group.bench_function("at_anchor_v20", |b| {
        b.iter(|| {
            // Query at version 20 (anchor point)
            let node = db.get_node_at_time(
                black_box(node_id),
                black_box(20.into()),
                black_box(20.into()),
            );
            black_box(node)
        });
    });

    // Benchmark querying between anchors (requires delta application)
    group.bench_function("with_deltas_v15", |b| {
        b.iter(|| {
            // Query at version 15 (anchor@v10 + 5 deltas)
            let node = db.get_node_at_time(
                black_box(node_id),
                black_box(15.into()),
                black_box(15.into()),
            );
            black_box(node)
        });
    });

    // Benchmark worst case (just before next anchor)
    group.bench_function("worst_case_v19", |b| {
        b.iter(|| {
            // Query at version 19 (anchor@v10 + 9 deltas, worst case)
            let node = db.get_node_at_time(
                black_box(node_id),
                black_box(19.into()),
                black_box(19.into()),
            );
            black_box(node)
        });
    });

    // Benchmark deep history query
    group.bench_function("deep_history_v5", |b| {
        b.iter(|| {
            // Query at very old version (tests temporal index performance)
            let node =
                db.get_node_at_time(black_box(node_id), black_box(5.into()), black_box(5.into()));
            black_box(node)
        });
    });

    group.finish();
}

/// Benchmark degree queries.
fn bench_degree_queries(c: &mut Criterion) {
    let (db, node_ids) = create_versioned_graph(1000, 1);
    let node_id = node_ids[0];

    c.bench_function("gallifreydb_out_degree", |b| {
        b.iter(|| {
            let degree = db.out_degree(black_box(node_id));
            black_box(degree)
        });
    });

    c.bench_function("gallifreydb_in_degree", |b| {
        b.iter(|| {
            let degree = db.in_degree(black_box(node_id));
            black_box(degree)
        });
    });
}

/// Benchmark labeled edge traversal through the high-level API.
///
/// Includes transaction overhead and API layer. Compare to
/// `current_state::bench_labeled_traversal` for raw storage performance
/// without API layer overhead.
fn bench_api_labeled_traversal(c: &mut Criterion) {
    let (db, node_ids) = create_versioned_graph(1000, 1);
    let node_id = node_ids[0];

    c.bench_function("gallifreydb_labeled_traversal", |b| {
        b.iter(|| {
            let edges = db.get_outgoing_edges_with_label(black_box(node_id), "LINKS_TO");
            black_box(edges)
        });
    });
}

/// Benchmark statistics retrieval.
fn bench_stats(c: &mut Criterion) {
    let (db, _) = create_versioned_graph(1000, 1);

    c.bench_function("gallifreydb_historical_stats", |b| {
        b.iter(|| {
            let stats = db.historical_stats();
            black_box(stats)
        });
    });
}

/// Benchmark batch node creation.
fn bench_batch_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("gallifreydb_batch");

    for batch_size in [10, 100, 1000] {
        group.bench_with_input(
            BenchmarkId::new("batch_node_creation", batch_size),
            &batch_size,
            |b, &size| {
                b.iter_batched(
                    || GallifreyDB::new().unwrap(),
                    |db| {
                        for i in 0..size {
                            let props = PropertyMapBuilder::new().insert("id", i as i64).build();
                            db.create_node("Node", props).unwrap();
                        }
                        black_box(db.node_count())
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_node_creation_with_versioning,
    bench_edge_creation_with_versioning,
    bench_current_state_queries,
    bench_api_single_hop_traversal,
    bench_api_multi_hop_traversal,
    bench_time_travel_queries,
    bench_degree_queries,
    bench_api_labeled_traversal,
    bench_stats,
    bench_batch_operations,
);

criterion_main!(benches);
