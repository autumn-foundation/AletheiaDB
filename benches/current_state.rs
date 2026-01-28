//! Benchmarks for storage layer primitives (fast path)
//!
//! This file covers:
//! - Graph operations (nodes, edges, traversals)
//! - ID generation
//! - String interning
//! - Property storage
//!
//! Performance Targets:
//! - Single-hop traversal: <1µs
//! - 3-hop traversal: <100µs
//! - Node lookup: <100ns
//! - Edge creation: <10µs

mod common;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::IdGenerator;
use gallifreydb::core::interning::StringInterner;
use gallifreydb::{CurrentStorage, PropertyMapBuilder};
use std::sync::Arc;
use std::thread;

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

// ============================================================================
// ID Generation Benchmarks
// ============================================================================

/// Benchmark single-threaded ID generation.
fn bench_id_single_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_single_thread");

    group.bench_function("sequential_1000", |b| {
        let generator = IdGenerator::new();
        b.iter(|| {
            for _ in 0..1000 {
                black_box(generator.next().unwrap());
            }
        });
    });

    group.bench_function("sequential_10000", |b| {
        let generator = IdGenerator::new();
        b.iter(|| {
            for _ in 0..10000 {
                black_box(generator.next().unwrap());
            }
        });
    });

    group.finish();
}

/// Benchmark concurrent ID generation with varying thread counts.
fn bench_id_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_concurrent");

    for thread_count in [2, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::new("threads", thread_count),
            &thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let generator = Arc::new(IdGenerator::new());
                    let ids_per_thread = 1000;

                    let handles: Vec<_> = (0..thread_count)
                        .map(|_| {
                            let gen_clone = Arc::clone(&generator);
                            thread::spawn(move || {
                                for _ in 0..ids_per_thread {
                                    black_box(gen_clone.next().unwrap());
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark current() method (read-only).
fn bench_id_current(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_current");

    group.bench_function("read_current", |b| {
        let generator = IdGenerator::new();
        // Generate some IDs first
        for _ in 0..1000 {
            generator.next().unwrap();
        }

        b.iter(|| {
            black_box(generator.current());
        });
    });

    group.bench_function("read_current_approximate", |b| {
        let generator = IdGenerator::new();
        // Generate some IDs first
        for _ in 0..1000 {
            generator.next().unwrap();
        }

        b.iter(|| {
            black_box(generator.current_approximate());
        });
    });

    group.finish();
}

/// Benchmark the full lifecycle: create generator, generate IDs, get current.
fn bench_id_full_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("id_generation_lifecycle");

    group.bench_function("create_and_generate_1000", |b| {
        b.iter(|| {
            let generator = IdGenerator::new();
            for _ in 0..1000 {
                black_box(generator.next().unwrap());
            }
            black_box(generator.current());
        });
    });

    group.finish();
}

// ============================================================================
// String Interning Benchmarks
// ============================================================================

/// Benchmark single string access using resolve() vs with_str().
fn bench_string_single_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_access_single");

    let interner = StringInterner::new();
    let id = interner.intern("test_string").unwrap();

    group.bench_function("resolve", |b| {
        b.iter(|| {
            let s = interner.resolve(id).unwrap();
            black_box(s.len())
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| interner.with_str(id, |s| black_box(s.len())));
    });

    group.finish();
}

/// Benchmark string length computation (common read-only operation).
fn bench_string_length(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_length");

    let interner = StringInterner::new();
    let id = interner
        .intern("this is a test string for benchmarking")
        .unwrap();

    group.bench_function("resolve", |b| {
        b.iter(|| {
            let s = interner.resolve(id).unwrap();
            black_box(s.len())
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| interner.with_str(id, |s| black_box(s.len())));
    });

    group.finish();
}

/// Benchmark string comparison (another common read-only operation).
fn bench_string_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_comparison");

    let interner = StringInterner::new();
    let id = interner.intern("Person").unwrap();

    group.bench_function("resolve", |b| {
        b.iter(|| {
            let s = interner.resolve(id).unwrap();
            black_box(s.as_ref() == "Person")
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| interner.with_str(id, |s| black_box(s == "Person")));
    });

    group.finish();
}

/// Benchmark multiple string accesses in a loop (simulates real workload).
fn bench_string_multiple_accesses(c: &mut Criterion) {
    let mut group = c.benchmark_group("multiple_accesses");

    let interner = StringInterner::new();
    let ids: Vec<_> = (0..100)
        .map(|i| interner.intern(format!("string_{}", i)).unwrap())
        .collect();

    group.bench_function("resolve_100", |b| {
        b.iter(|| {
            for &id in &ids {
                let s = interner.resolve(id).unwrap();
                black_box(s.len());
            }
        });
    });

    group.bench_function("with_str_100", |b| {
        b.iter(|| {
            for &id in &ids {
                interner.with_str(id, |s| black_box(s.len()));
            }
        });
    });

    group.finish();
}

/// Benchmark simulated serialization workload (common use case).
fn bench_string_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialization_simulation");

    let interner = StringInterner::new();
    let property_keys: Vec<_> = ["name", "age", "email", "address", "phone"]
        .iter()
        .map(|&s| interner.intern(s).unwrap())
        .collect();

    // Simulate serializing 1000 objects with 5 properties each
    group.bench_function("resolve", |b| {
        b.iter(|| {
            let mut total_len = 0;
            for _ in 0..1000 {
                for &key_id in &property_keys {
                    let key = interner.resolve(key_id).unwrap();
                    // Simulate serialization by computing length and hashing
                    total_len += key.len();
                    black_box(key.as_ref());
                }
            }
            black_box(total_len);
        });
    });

    group.bench_function("with_str", |b| {
        b.iter(|| {
            let mut total_len = 0;
            for _ in 0..1000 {
                for &key_id in &property_keys {
                    interner.with_str(key_id, |key| {
                        // Simulate serialization by computing length and hashing
                        total_len += key.len();
                        black_box(key);
                    });
                }
            }
            black_box(total_len);
        });
    });

    group.finish();
}

/// Benchmark Arc clone overhead isolation (measures just the Arc operations).
fn bench_string_arc_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("arc_overhead");

    let interner = StringInterner::new();
    let id = interner.intern("overhead_test").unwrap();

    // Measure Arc clone + drop overhead
    group.bench_function("arc_clone_drop", |b| {
        b.iter(|| {
            let arc = interner.resolve(id).unwrap();
            black_box(arc);
            // Arc is dropped here (atomic decrement)
        });
    });

    // Measure direct access without Arc clone
    group.bench_function("direct_access", |b| {
        b.iter(|| {
            interner.with_str(id, |s| {
                black_box(s);
            });
        });
    });

    group.finish();
}

/// Benchmark hot-path scenario: tight loop with repeated accesses.
fn bench_string_hot_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("hot_path");

    let interner = Arc::new(StringInterner::new());
    let id = interner.intern("hot_path_string").unwrap();

    for iterations in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::new("resolve", iterations),
            &iterations,
            |b, &iterations| {
                b.iter(|| {
                    for _ in 0..iterations {
                        let s = interner.resolve(id).unwrap();
                        black_box(s.len());
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("with_str", iterations),
            &iterations,
            |b, &iterations| {
                b.iter(|| {
                    for _ in 0..iterations {
                        interner.with_str(id, |s| black_box(s.len()));
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark allocation overhead in get_outgoing() at the index level.
///
/// This micro-benchmark isolates the Vec allocation cost by directly
/// accessing CurrentIndexes rather than going through CurrentStorage.
/// Target: Eliminate 100-500ns allocation overhead.
fn bench_get_outgoing_allocation_overhead(c: &mut Criterion) {
    use gallifreydb::core::graph::Edge;
    use gallifreydb::core::id::{EdgeId, VersionId};
    use gallifreydb::core::interning::GLOBAL_INTERNER;
    use gallifreydb::index::current::CurrentIndexes;

    let mut group = c.benchmark_group("get_outgoing_allocation");

    // Create a test graph at the index level with varying degrees
    for out_degree in [1, 10, 100] {
        let indexes = CurrentIndexes::new();
        let node_id = gallifreydb::NodeId::new(0).unwrap();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert edges to create adjacency list
        for i in 0..out_degree {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                node_id,
                gallifreydb::NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }

        // Trigger initial compaction
        indexes.compact_adjacency();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("degree_{}", out_degree)),
            &(indexes, node_id),
            |b, (indexes, node)| {
                b.iter(|| {
                    // After optimization: returns AdjacencyGuard with Arc clone (~5-10ns)
                    // Previously: allocated Vec on every call (100-500ns overhead)
                    let adjacency = indexes.get_outgoing(black_box(*node));
                    black_box(adjacency)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_single_hop_traversal,
    bench_multi_hop_traversal,
    bench_node_lookup,
    bench_edge_lookup,
    bench_labeled_traversal,
    bench_node_creation,
    bench_edge_creation,
    bench_degree_queries,
    bench_find_neighbors,
    bench_get_outgoing_allocation_overhead,
    // ID Generation
    bench_id_single_thread,
    bench_id_concurrent,
    bench_id_current,
    bench_id_full_lifecycle,
    // String Interning
    bench_string_single_access,
    bench_string_length,
    bench_string_comparison,
    bench_string_multiple_accesses,
    bench_string_serialization,
    bench_string_arc_overhead,
    bench_string_hot_path,
    bench_node_update,
    bench_concurrent_node_updates,
);

criterion_main!(benches);

/// Benchmark node update (write lock overhead).
fn bench_node_update(c: &mut Criterion) {
    c.bench_function("node_update", |b| {
        b.iter_batched(
            || {
                let storage = CurrentStorage::new();
                let props = PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .build();
                let node_id = storage.create_node("Person", props).unwrap();
                let node = storage.get_node(node_id).unwrap();
                (storage, node)
            },
            |(storage, node)| {
                // This acquires snapshot_lock.read()
                storage.update_node_direct(node, gallifreydb::core::temporal::time::now()).unwrap();
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark concurrent node updates (contention on read lock).
fn bench_concurrent_node_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_updates");

    for thread_count in [1, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("threads", thread_count),
            &thread_count,
            |b, &thread_count| {
                b.iter_custom(|iters| {
                    let storage = Arc::new(CurrentStorage::new());
                    let mut handles = Vec::new();
                    let start = std::time::Instant::now();

                    // Pre-create nodes
                    let nodes: Vec<_> = (0..thread_count).map(|i| {
                        storage.create_node(
                            "Person",
                            PropertyMapBuilder::new().insert("id", i as i64).build()
                        ).unwrap()
                    }).collect();

                    let nodes = Arc::new(nodes);

                    for t in 0..thread_count {
                        let storage = storage.clone();
                        let nodes = nodes.clone();
                        let my_node_id = nodes[t];

                        handles.push(thread::spawn(move || {
                            let node = storage.get_node(my_node_id).unwrap();
                            for _ in 0..(iters / thread_count as u64) {
                                // Simulate small update
                                storage.update_node_direct(node.clone(), gallifreydb::core::temporal::time::now()).unwrap();
                            }
                        }));
                    }

                    for h in handles {
                        h.join().unwrap();
                    }

                    start.elapsed()
                });
            },
        );
    }
    group.finish();
}
