//! Transaction overhead benchmarks
//!
//! These benchmarks measure the overhead introduced by the transaction layer
//! compared to direct operations.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::{GallifreyDB, PropertyMapBuilder, ReadOps, WriteOps};
use std::sync::Arc;
use std::thread;

fn bench_read_transaction_creation(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("read_transaction_creation", |b| {
        b.iter(|| {
            let _tx = db.read_transaction().unwrap();
        });
    });
}

fn bench_write_transaction_creation(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("write_transaction_creation", |b| {
        b.iter(|| {
            let _tx = db.write_transaction().unwrap();
        });
    });
}

fn bench_closure_based_write_empty(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("closure_write_empty_commit", |b| {
        b.iter(|| {
            db.write(|_tx| Ok(())).unwrap();
        });
    });
}

fn bench_closure_based_write_single_node(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("closure_write_single_node", |b| {
        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert("name", "Test")
                        .insert("age", 30i64)
                        .build(),
                )
            })
            .unwrap();
        });
    });
}

fn bench_closure_based_write_10_ops(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("closure_write_10_operations", |b| {
        b.iter(|| {
            db.write(|tx| {
                let mut nodes = Vec::new();
                // Create 5 nodes
                for i in 0..5 {
                    let node = tx.create_node(
                        "Person",
                        PropertyMapBuilder::new().insert("id", i as i64).build(),
                    )?;
                    nodes.push(node);
                }
                // Create 5 edges
                for i in 0..4 {
                    tx.create_edge(
                        nodes[i],
                        nodes[i + 1],
                        "KNOWS",
                        PropertyMapBuilder::new().build(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        });
    });
}

fn bench_explicit_transaction_commit(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("explicit_transaction_commit", |b| {
        b.iter(|| {
            let mut tx = db.write_transaction().unwrap();
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Test").build(),
            )
            .unwrap();
            tx.commit().unwrap();
        });
    });
}

fn bench_implicit_vs_explicit(c: &mut Criterion) {
    let db = GallifreyDB::new();

    let mut group = c.benchmark_group("implicit_vs_explicit");

    group.bench_function("implicit_create_node", |b| {
        b.iter(|| {
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Test").build(),
            )
            .unwrap();
        });
    });

    group.bench_function("explicit_create_node", |b| {
        b.iter(|| {
            let mut tx = db.write_transaction().unwrap();
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Test").build(),
            )
            .unwrap();
            tx.commit().unwrap();
        });
    });

    group.finish();
}

fn bench_read_transaction_overhead(c: &mut Criterion) {
    let db = GallifreyDB::new();

    // Create a node to read
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

    let mut group = c.benchmark_group("read_overhead");

    group.bench_function("direct_get_node", |b| {
        b.iter(|| {
            let _node = db.get_node(black_box(node_id)).unwrap();
        });
    });

    group.bench_function("read_transaction_get_node", |b| {
        b.iter(|| {
            db.read(|tx| {
                let _node = tx.get_node(black_box(node_id))?;
                Ok(())
            })
            .unwrap();
        });
    });

    group.finish();
}

fn bench_wal_overhead(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("write_with_wal_flush", |b| {
        b.iter(|| {
            db.write(|tx| {
                tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Test").build(),
                )
            })
            .unwrap();
        });
    });
}

/// Benchmark batch edge insertions to verify adjacency rebuild optimization.
/// This measures the improvement from batching rebuilds at transaction commit.
fn bench_batch_edge_insertions(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_edge_insertions");

    // Test different batch sizes
    for batch_size in [100, 1000, 10000] {
        group.bench_function(format!("batch_{}_edges", batch_size), |b| {
            b.iter(|| {
                let db = GallifreyDB::new();

                db.write(|tx| {
                    // Create nodes first
                    let mut nodes = Vec::new();
                    for i in 0..batch_size {
                        let node = tx.create_node(
                            "Node",
                            PropertyMapBuilder::new().insert("id", i as i64).build(),
                        )?;
                        nodes.push(node);
                    }

                    // Create edges between consecutive nodes
                    for i in 0..(batch_size - 1) {
                        tx.create_edge(
                            nodes[i],
                            nodes[i + 1],
                            "CONNECTS",
                            PropertyMapBuilder::new().build(),
                        )?;
                    }

                    Ok(())
                })
                .unwrap();
            });
        });
    }

    group.finish();
}

/// Benchmark edge updates in batch to verify optimization.
fn bench_batch_edge_updates(c: &mut Criterion) {
    let db = GallifreyDB::new();

    // Pre-create a graph with 1000 edges
    let edge_ids: Vec<_> = db
        .write(|tx| {
            let mut nodes = Vec::new();
            let mut edges = Vec::new();

            for i in 0..1000 {
                let node = tx.create_node(
                    "Node",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )?;
                nodes.push(node);
            }

            for i in 0..999 {
                let edge = tx.create_edge(
                    nodes[i],
                    nodes[i + 1],
                    "CONNECTS",
                    PropertyMapBuilder::new().build(),
                )?;
                edges.push(edge);
            }

            Ok(edges)
        })
        .unwrap();

    c.bench_function("batch_update_1000_edges", |b| {
        b.iter(|| {
            db.write(|tx| {
                // Update all edges
                for edge_id in &edge_ids {
                    tx.update_edge(
                        *edge_id,
                        PropertyMapBuilder::new().insert("weight", 1i64).build(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        });
    });
}

/// Benchmark edge deletions in batch.
fn bench_batch_edge_deletions(c: &mut Criterion) {
    c.bench_function("batch_delete_1000_edges", |b| {
        b.iter_batched(
            || {
                // Setup: create DB with 1000 edges
                let db = GallifreyDB::new();
                let edge_ids: Vec<_> = db
                    .write(|tx| {
                        let mut nodes = Vec::new();
                        let mut edges = Vec::new();

                        for i in 0..1000 {
                            let node = tx.create_node(
                                "Node",
                                PropertyMapBuilder::new().insert("id", i as i64).build(),
                            )?;
                            nodes.push(node);
                        }

                        for i in 0..999 {
                            let edge = tx.create_edge(
                                nodes[i],
                                nodes[i + 1],
                                "CONNECTS",
                                PropertyMapBuilder::new().build(),
                            )?;
                            edges.push(edge);
                        }

                        Ok(edges)
                    })
                    .unwrap();

                (db, edge_ids)
            },
            |(db, edge_ids)| {
                // Delete all edges in one transaction
                db.write(|tx| {
                    for edge_id in edge_ids {
                        tx.delete_edge(edge_id)?;
                    }
                    Ok(())
                })
                .unwrap();
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark edge insertions with pre-populated graph (at-scale performance).
/// Tests how adjacency rebuild performs when the graph already has existing edges.
fn bench_batch_insertions_with_prepopulated_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_insertions_prepopulated");

    // Test adding edges to graphs of different sizes
    for existing_edges in [1000, 10000] {
        group.bench_function(format!("add_1000_to_{}_existing", existing_edges), |b| {
            b.iter_batched(
                || {
                    // Setup: create DB with existing edges
                    let db = GallifreyDB::new();

                    // Pre-populate with existing edges
                    db.write(|tx| {
                        let mut nodes = Vec::new();
                        for i in 0..existing_edges {
                            let node = tx.create_node(
                                "Node",
                                PropertyMapBuilder::new().insert("id", i as i64).build(),
                            )?;
                            nodes.push(node);
                        }

                        for i in 0..(existing_edges - 1) {
                            tx.create_edge(
                                nodes[i],
                                nodes[i + 1],
                                "EXISTING",
                                PropertyMapBuilder::new().build(),
                            )?;
                        }

                        Ok(())
                    })
                    .unwrap();

                    db
                },
                |db| {
                    // Add 1000 new edges to existing graph
                    db.write(|tx| {
                        let mut new_nodes = Vec::new();
                        for i in 0..1001 {
                            let node = tx.create_node(
                                "NewNode",
                                PropertyMapBuilder::new()
                                    .insert("id", (i + 100000) as i64)
                                    .build(),
                            )?;
                            new_nodes.push(node);
                        }

                        for i in 0..1000 {
                            tx.create_edge(
                                new_nodes[i],
                                new_nodes[i + 1],
                                "NEW",
                                PropertyMapBuilder::new().build(),
                            )?;
                        }

                        Ok(())
                    })
                    .unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

/// Benchmark concurrent read operations during adjacency rebuild.
/// This tests the impact of rebuild on read performance.
fn bench_read_during_rebuild(c: &mut Criterion) {
    // Pre-populate a database with 4K nodes + 4K edges
    // Note: stays under DEFAULT_MAX_OPERATIONS (10K) limit defined in
    // src/api/transaction/write_buffer.rs for DoS protection
    let db = GallifreyDB::new();
    let node_ids: Vec<_> = db
        .write(|tx| {
            let mut nodes = Vec::new();
            for i in 0..4000 {
                let node = tx.create_node(
                    "Node",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )?;
                nodes.push(node);
            }

            for i in 0..3999 {
                tx.create_edge(
                    nodes[i],
                    nodes[i + 1],
                    "CONNECTS",
                    PropertyMapBuilder::new().build(),
                )?;
            }

            Ok(nodes)
        })
        .unwrap();

    c.bench_function("read_traversal_existing_graph", |b| {
        b.iter(|| {
            // Perform graph traversal
            for node_id in &node_ids[..100] {
                let _ = db.get_outgoing_edges(black_box(*node_id));
            }
        });
    });
}

/// Benchmark concurrent read operations to measure visibility check performance.
///
/// This benchmark demonstrates the RwLock optimization in TxVisibilityManager (issue #222).
/// Every read operation (get_node, get_edge) performs a visibility check, which previously
/// used a Mutex causing read contention. With RwLock, concurrent readers can proceed
/// without serialization.
///
/// Performance target from CLAUDE.md: <1µs single-hop traversal includes visibility checks.
fn bench_concurrent_visibility_checks(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_visibility");

    // Pre-populate database with committed data (500 nodes)
    // Note: stays under DEFAULT_MAX_OPERATIONS (10K) limit defined in
    // src/api/transaction/write_buffer.rs for DoS protection
    let db = Arc::new(GallifreyDB::new());
    let node_ids: Vec<_> = db
        .write(|tx| {
            let mut nodes = Vec::new();
            for i in 0..500 {
                let node = tx.create_node(
                    "Node",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )?;
                nodes.push(node);
            }
            Ok(nodes)
        })
        .unwrap();

    // Benchmark with different thread counts to show scalability
    for thread_count in [1, 2, 4, 8] {
        group.bench_function(format!("{}_threads", thread_count), |b| {
            b.iter(|| {
                let handles: Vec<_> = (0..thread_count)
                    .map(|_| {
                        let db_clone = Arc::clone(&db);
                        let nodes = node_ids.clone();
                        thread::spawn(move || {
                            // Each thread performs 100 read operations
                            // Each read triggers a visibility check via is_visible()
                            for node_id in nodes.iter().take(100) {
                                let _ = db_clone.get_node(black_box(*node_id));
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    handle.join().unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark single-threaded visibility check performance (baseline).
fn bench_sequential_visibility_checks(c: &mut Criterion) {
    let db = GallifreyDB::new();

    // Pre-populate with 500 nodes
    // Note: stays under DEFAULT_MAX_OPERATIONS (10K) limit defined in
    // src/api/transaction/write_buffer.rs for DoS protection
    let node_ids: Vec<_> = db
        .write(|tx| {
            let mut nodes = Vec::new();
            for i in 0..500 {
                let node = tx.create_node(
                    "Node",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )?;
                nodes.push(node);
            }
            Ok(nodes)
        })
        .unwrap();

    c.bench_function("sequential_get_node_500", |b| {
        b.iter(|| {
            // Sequential reads - each triggers visibility check
            for node_id in &node_ids {
                let _ = db.get_node(black_box(*node_id));
            }
        });
    });
}

/// Benchmark apply_changes with large transactions to measure lock consolidation impact.
///
/// This benchmark measures commit latency for transactions of varying sizes to verify
/// the performance improvements from Issue #223 (lock consolidation in apply_changes).
///
/// Tests transaction sizes of 10, 100, and 1000 operations with different operation mixes:
/// - Mixed operations (creates, updates, deletes)
/// - Create-heavy (mostly creates)
/// - Delete-heavy (tests tombstone ID pre-generation optimization)
fn bench_apply_changes_large_tx(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_changes_large_tx");

    // Test different transaction sizes
    for size in [10, 100, 1000] {
        // Mixed operations: creates, updates, deletes
        group.bench_function(format!("mixed_{}_ops", size), |b| {
            b.iter_batched(
                || {
                    // Setup: create DB with some existing data for updates/deletes
                    let db = GallifreyDB::new();
                    let (nodes, edges) = db
                        .write(|tx| {
                            let mut nodes = Vec::new();
                            let mut edges = Vec::new();

                            // Create nodes for later updates/deletes
                            for i in 0..size {
                                let node = tx.create_node(
                                    "Person",
                                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                                )?;
                                nodes.push(node);
                            }

                            // Create edges for later deletes
                            for i in 0..(size / 2) {
                                let edge = tx.create_edge(
                                    nodes[i],
                                    nodes[i + 1],
                                    "KNOWS",
                                    PropertyMapBuilder::new().build(),
                                )?;
                                edges.push(edge);
                            }

                            Ok((nodes, edges))
                        })
                        .unwrap();

                    (db, nodes, edges)
                },
                |(db, nodes, edges)| {
                    // Measure: mixed transaction with creates, updates, deletes
                    db.write(|tx| {
                        let ops_per_type = size / 4;

                        // 25% creates
                        for i in 0..ops_per_type {
                            tx.create_node(
                                "NewPerson",
                                PropertyMapBuilder::new()
                                    .insert("id", (i + 10000) as i64)
                                    .build(),
                            )?;
                        }

                        // 25% updates
                        for (i, &node_id) in nodes.iter().enumerate().take(ops_per_type) {
                            tx.update_node(
                                node_id,
                                PropertyMapBuilder::new()
                                    .insert("updated", true)
                                    .insert("age", (i + 20) as i64)
                                    .build(),
                            )?;
                        }

                        // 25% node deletes (tests tombstone ID generation)
                        for i in 0..ops_per_type.min(nodes.len() / 2) {
                            tx.delete_node(nodes[nodes.len() - 1 - i])?;
                        }

                        // 25% edge deletes (tests tombstone ID generation)
                        for &edge_id in edges.iter().take(ops_per_type) {
                            tx.delete_edge(edge_id)?;
                        }

                        Ok(())
                    })
                    .unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });

        // Create-heavy workload
        group.bench_function(format!("creates_{}_ops", size), |b| {
            b.iter(|| {
                let db = GallifreyDB::new();
                db.write(|tx| {
                    for i in 0..size {
                        tx.create_node(
                            "Person",
                            PropertyMapBuilder::new()
                                .insert("id", i as i64)
                                .insert("name", format!("Person{}", i))
                                .build(),
                        )?;
                    }
                    Ok(())
                })
                .unwrap();
            });
        });

        // Delete-heavy workload (tests tombstone ID pre-generation optimization)
        group.bench_function(format!("deletes_{}_ops", size), |b| {
            b.iter_batched(
                || {
                    // Setup: create DB with nodes and edges to delete
                    let db = GallifreyDB::new();
                    let (nodes, edges) = db
                        .write(|tx| {
                            let mut nodes = Vec::new();
                            let mut edges = Vec::new();

                            for i in 0..size {
                                let node = tx.create_node(
                                    "Person",
                                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                                )?;
                                nodes.push(node);
                            }

                            for i in 0..(size - 1) {
                                let edge = tx.create_edge(
                                    nodes[i],
                                    nodes[i + 1],
                                    "KNOWS",
                                    PropertyMapBuilder::new().build(),
                                )?;
                                edges.push(edge);
                            }

                            Ok((nodes, edges))
                        })
                        .unwrap();

                    (db, nodes, edges)
                },
                |(db, nodes, edges)| {
                    // Measure: delete all nodes and edges
                    // This heavily tests the tombstone ID pre-generation optimization
                    db.write(|tx| {
                        // Delete edges first (to avoid referential integrity issues)
                        for edge in edges {
                            tx.delete_edge(edge)?;
                        }

                        // Delete nodes
                        for node in nodes {
                            tx.delete_node(node)?;
                        }

                        Ok(())
                    })
                    .unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_read_transaction_creation,
    bench_write_transaction_creation,
    bench_closure_based_write_empty,
    bench_closure_based_write_single_node,
    bench_closure_based_write_10_ops,
    bench_explicit_transaction_commit,
    bench_implicit_vs_explicit,
    bench_read_transaction_overhead,
    bench_wal_overhead,
    bench_batch_edge_insertions,
    bench_batch_edge_updates,
    bench_batch_edge_deletions,
    bench_batch_insertions_with_prepopulated_graph,
    bench_read_during_rebuild,
    bench_concurrent_visibility_checks,
    bench_sequential_visibility_checks,
    bench_apply_changes_large_tx,
);

criterion_main!(benches);
