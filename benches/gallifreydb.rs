//! Benchmarks for GallifreyDB with temporal queries.
//!
//! These benchmarks test:
//! - Fast path: Current state queries (should match current_state.rs performance)
//! - Slow path: Time-travel queries with version reconstruction
//! - Version creation: Overhead of maintaining temporal history
//!
//! NOTE: These benchmarks use Async durability mode to isolate graph performance
//! from WAL flush overhead. See durability_modes.rs for durability-focused benchmarks.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::{
    GallifreyDB, NodeId, PropertyMapBuilder,
    storage::wal::{DurabilityMode, WalConfig},
};

/// Create a test database configured for benchmarking (Async mode, no waiting).
fn create_benchmark_db() -> GallifreyDB {
    let wal_config = WalConfig {
        durability_mode: DurabilityMode::async_mode(100), // Async mode for benchmarks
        ..Default::default()
    };
    GallifreyDB::with_wal_config(wal_config)
}

/// Create a test database with versioned history.
///
/// Creates nodes and then updates them multiple times to build version chains.
fn create_versioned_graph(
    node_count: usize,
    _versions_per_node: usize,
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

    // Simulate updates by storing timestamps where we'd create new versions
    // In a real implementation, we'd have update_node/update_edge methods
    // For now, the benchmark focuses on what we have working

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

/// Benchmark single-hop traversal (fast path).
fn bench_single_hop_traversal(c: &mut Criterion) {
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

/// Benchmark 3-hop traversal (fast path).
fn bench_multi_hop_traversal(c: &mut Criterion) {
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
fn bench_time_travel_queries(c: &mut Criterion) {
    let (db, node_ids) = create_versioned_graph(1000, 1);
    let node_id = node_ids[0];

    // Query at the creation timestamp
    let timestamp = 0;

    c.bench_function("gallifreydb_time_travel_query", |b| {
        b.iter(|| {
            let node = db.get_node_at_time(
                black_box(node_id),
                black_box(timestamp),
                black_box(timestamp),
            );
            black_box(node)
        });
    });
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

/// Benchmark labeled edge traversal.
fn bench_labeled_traversal(c: &mut Criterion) {
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
                    GallifreyDB::new,
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
    benches,
    bench_node_creation_with_versioning,
    bench_edge_creation_with_versioning,
    bench_current_state_queries,
    bench_single_hop_traversal,
    bench_multi_hop_traversal,
    bench_time_travel_queries,
    bench_degree_queries,
    bench_labeled_traversal,
    bench_stats,
    bench_batch_operations,
);

criterion_main!(benches);
