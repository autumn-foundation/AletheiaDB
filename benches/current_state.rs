//! Benchmarks for current-state graph operations.
//!
//! These benchmarks test the performance of the "hot path" - queries that only
//! touch the current state without any temporal operations.
//!
//! Performance Targets:
//! - Single-hop traversal: <1µs
//! - 3-hop traversal: <100µs
//! - Node lookup: <100ns
//! - Edge creation: <10µs

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::{CurrentStorage, PropertyMapBuilder};

/// Create a test graph with a specified number of nodes and edges.
///
/// Creates a directed graph where each node has `out_degree` outgoing edges.
fn create_test_graph(node_count: usize, out_degree: usize) -> CurrentStorage {
    let storage = CurrentStorage::new();

    // Create nodes
    let node_ids: Vec<_> = (0..node_count)
        .map(|i| {
            storage
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap()
        })
        .collect();

    // Create edges
    for i in 0..node_count {
        for j in 0..out_degree.min(node_count - 1) {
            let target = (i + j + 1) % node_count;
            storage
                .create_edge(
                    node_ids[i],
                    node_ids[target],
                    if j % 2 == 0 { "KNOWS" } else { "FOLLOWS" },
                    PropertyMapBuilder::new()
                        .insert("weight", (i + j) as i64)
                        .build(),
                )
                .unwrap();
        }
    }

    storage
}

/// Benchmark single-hop traversal (critical hot path).
///
/// Target: <1µs per operation
fn bench_single_hop_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_hop_traversal");

    for graph_size in [100, 1000, 10000] {
        let storage = create_test_graph(graph_size, 10);
        let first_node = gallifreydb::NodeId::new(0).unwrap();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_nodes", graph_size)),
            &(storage, first_node),
            |b, (storage, node)| {
                b.iter(|| {
                    // Get outgoing edges - this is the critical operation
                    let edges = storage.get_outgoing_edges(black_box(*node));
                    black_box(edges)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 3-hop traversal.
///
/// Target: <100µs per operation
fn bench_multi_hop_traversal(c: &mut Criterion) {
    let storage = create_test_graph(1000, 10);
    let start_node = gallifreydb::NodeId::new(0).unwrap();

    c.bench_function("3_hop_traversal", |b| {
        b.iter(|| {
            let mut visited = 0;

            // 1st hop
            let hop1 = storage.get_outgoing_edges(black_box(start_node));
            for edge_id in &hop1 {
                if let Ok(edge) = storage.get_edge(*edge_id) {
                    // 2nd hop
                    let hop2 = storage.get_outgoing_edges(edge.target);
                    for edge_id2 in &hop2 {
                        if let Ok(edge2) = storage.get_edge(*edge_id2) {
                            // 3rd hop
                            let hop3 = storage.get_outgoing_edges(edge2.target);
                            visited += hop3.len();
                        }
                    }
                }
            }

            black_box(visited)
        });
    });
}

/// Benchmark node lookup by ID.
///
/// Target: <100ns per operation
fn bench_node_lookup(c: &mut Criterion) {
    let storage = create_test_graph(10000, 10);
    let node_id = gallifreydb::NodeId::new(5000).unwrap();

    c.bench_function("node_lookup", |b| {
        b.iter(|| {
            let node = storage.get_node(black_box(node_id));
            black_box(node)
        });
    });
}

/// Benchmark edge lookup by ID.
fn bench_edge_lookup(c: &mut Criterion) {
    let storage = create_test_graph(1000, 10);
    let edge_id = gallifreydb::EdgeId::new(5000).unwrap();

    c.bench_function("edge_lookup", |b| {
        b.iter(|| {
            let edge = storage.get_edge(black_box(edge_id));
            black_box(edge)
        });
    });
}

/// Benchmark labeled edge traversal.
fn bench_labeled_traversal(c: &mut Criterion) {
    let storage = create_test_graph(1000, 10);
    let node_id = gallifreydb::NodeId::new(0).unwrap();

    c.bench_function("labeled_traversal", |b| {
        b.iter(|| {
            let edges = storage.get_outgoing_edges_with_label(black_box(node_id), "KNOWS");
            black_box(edges)
        });
    });
}

/// Benchmark node creation.
///
/// Target: <10µs per operation
fn bench_node_creation(c: &mut Criterion) {
    c.bench_function("node_creation", |b| {
        b.iter_batched(
            CurrentStorage::new,
            |storage| {
                let props = PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build();
                let node = storage.create_node("Person", props);
                black_box(node)
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark edge creation.
///
/// Target: <10µs per operation (including adjacency rebuild)
fn bench_edge_creation(c: &mut Criterion) {
    c.bench_function("edge_creation", |b| {
        b.iter_batched(
            || {
                let storage = CurrentStorage::new();
                let n1 = storage
                    .create_node("Person", PropertyMapBuilder::new().build())
                    .unwrap();
                let n2 = storage
                    .create_node("Person", PropertyMapBuilder::new().build())
                    .unwrap();
                (storage, n1, n2)
            },
            |(storage, n1, n2)| {
                let props = PropertyMapBuilder::new().insert("since", 2020i64).build();
                let edge = storage.create_edge(n1, n2, "KNOWS", props);
                black_box(edge)
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark graph degree queries.
fn bench_degree_queries(c: &mut Criterion) {
    let storage = create_test_graph(1000, 10);
    let node_id = gallifreydb::NodeId::new(0).unwrap();

    c.bench_function("out_degree", |b| {
        b.iter(|| {
            let degree = storage.out_degree(black_box(node_id));
            black_box(degree)
        });
    });

    c.bench_function("in_degree", |b| {
        b.iter(|| {
            let degree = storage.in_degree(black_box(node_id));
            black_box(degree)
        });
    });
}

/// Benchmark finding neighbors (targets of outgoing edges).
fn bench_find_neighbors(c: &mut Criterion) {
    let storage = create_test_graph(1000, 10);
    let node_id = gallifreydb::NodeId::new(0).unwrap();

    c.bench_function("find_neighbors", |b| {
        b.iter(|| {
            let edges = storage.get_outgoing_edges(black_box(node_id));
            let neighbors: Vec<_> = edges
                .iter()
                .filter_map(|edge_id| storage.get_edge(*edge_id).ok())
                .map(|edge| edge.target)
                .collect();
            black_box(neighbors)
        });
    });
}

/// Benchmark concurrent read performance (lock-free advantage).
///
/// This benchmark demonstrates the benefit of lock-free adjacency reads
/// by measuring throughput with multiple concurrent readers.
fn bench_concurrent_reads(c: &mut Criterion) {
    use std::sync::Arc;
    use std::thread;

    let storage = Arc::new(create_test_graph(1000, 10));
    let node_ids: Vec<_> = (0..100).map(|i| gallifreydb::NodeId::new(i).unwrap()).collect();

    c.bench_function("concurrent_reads_4_threads", |b| {
        b.iter(|| {
            let mut handles = vec![];

            // Spawn 4 concurrent reader threads
            for _ in 0..4 {
                let storage_clone = Arc::clone(&storage);
                let nodes = node_ids.clone();

                let handle = thread::spawn(move || {
                    let mut total_edges = 0;
                    for _ in 0..25 {  // 25 iterations per thread = 100 total per benchmark iteration
                        for node_id in &nodes {
                            let edges = storage_clone.get_outgoing_edges(*node_id);
                            total_edges += edges.len();
                        }
                    }
                    total_edges
                });

                handles.push(handle);
            }

            // Wait for all threads and sum results
            let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
            black_box(total)
        });
    });
}

criterion_group!(
    benches,
    bench_single_hop_traversal,
    bench_multi_hop_traversal,
    bench_node_lookup,
    bench_edge_lookup,
    bench_labeled_traversal,
    bench_node_creation,
    bench_edge_creation,
    bench_degree_queries,
    bench_find_neighbors,
    bench_concurrent_reads,
);

criterion_main!(benches);
