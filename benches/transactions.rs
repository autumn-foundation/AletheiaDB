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

    // Test different batch sizes (DEFAULT_MAX_OPERATIONS is 50K)
    // Each iteration creates batch_size nodes + (batch_size - 1) edges = ~2*batch_size ops
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
    c.bench_function("batch_update_1000_edges", |b| {
        b.iter_batched(
            || {
                // Setup: Create DB with 1000 edges
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
                // Benchmark: Update all edges
                db.write(|tx| {
                    for edge_id in &edge_ids {
                        tx.update_edge(
                            *edge_id,
                            PropertyMapBuilder::new().insert("weight", 1i64).build(),
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
    // Note: stays under DEFAULT_MAX_OPERATIONS (50K) limit defined in
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
    // Note: stays under DEFAULT_MAX_OPERATIONS (50K) limit defined in
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
    // Note: stays under DEFAULT_MAX_OPERATIONS (50K) limit defined in
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

/// Benchmark concurrent transaction creation with many active transactions.
///
/// This benchmark measures the overhead of `capture_snapshot` when there are many
/// active transactions. Issue #221 identified that cloning the HashSet of active
/// transactions creates O(N²) scaling: with N concurrent transactions, each new
/// transaction clones a HashSet containing N elements.
///
/// This benchmark simulates the worst-case scenario where many long-running
/// transactions are active while new transactions are being created.
fn bench_concurrent_transaction_creation_with_active_txs(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_tx_creation");

    // Test with different numbers of concurrent active transactions
    for active_count in [10, 50, 100] {
        group.bench_function(format!("{}_active_txs", active_count), |b| {
            b.iter_batched(
                || {
                    // Setup: create DB and start many active write transactions
                    let db = Arc::new(GallifreyDB::new());
                    let mut active_txs = Vec::new();

                    for _ in 0..active_count {
                        let tx = db.write_transaction().unwrap();
                        active_txs.push(tx);
                    }

                    (db, active_txs)
                },
                |(db, _active_txs)| {
                    // Measure: create new transactions while others are active
                    // Each creation calls capture_snapshot which clones active tx set
                    for _ in 0..10 {
                        let _tx = db.write_transaction().unwrap();
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
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

/// Benchmark commit overhead for transactions with and without vector properties.
///
/// This benchmark measures the performance improvement from Issue #231, which optimized
/// the commit path to only call on_temporal_vector_transaction() when vector properties
/// were actually modified. Previously, this function was called on every commit, causing
/// unnecessary overhead for non-vector transactions.
///
/// The benchmark compares:
/// - Non-vector transactions: Should skip vector index notification (optimized path)
/// - Vector transactions: Should trigger vector index notification (necessary overhead)
///
/// Expected result: Non-vector commits should be faster due to skipping the vector
/// notification overhead.
fn bench_commit_vector_vs_nonvector(c: &mut Criterion) {
    let mut group = c.benchmark_group("commit_vector_overhead");

    // Benchmark 1: Non-vector transaction (should be optimized)
    group.bench_function("commit_without_vectors", |b| {
        b.iter(|| {
            let db = GallifreyDB::new();
            db.write(|tx| {
                // Create nodes with scalar properties only (no vectors)
                for i in 0..10 {
                    tx.create_node(
                        "Person",
                        PropertyMapBuilder::new()
                            .insert("name", format!("Person{}", i))
                            .insert("age", (20 + i) as i64)
                            .insert("active", true)
                            .build(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        });
    });

    // Benchmark 2: Vector transaction (will call vector notification)
    group.bench_function("commit_with_vectors", |b| {
        b.iter(|| {
            let db = GallifreyDB::new();
            db.write(|tx| {
                // Create nodes with vector properties
                let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
                for i in 0..10 {
                    tx.create_node(
                        "Document",
                        PropertyMapBuilder::new()
                            .insert("title", format!("Doc{}", i))
                            .insert_vector("embedding", &embedding)
                            .build(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        });
    });

    // Benchmark 3: Large non-vector transaction (amplifies the optimization benefit)
    group.bench_function("commit_100_ops_without_vectors", |b| {
        b.iter(|| {
            let db = GallifreyDB::new();
            db.write(|tx| {
                // Create 100 nodes with scalar properties only
                for i in 0..100 {
                    tx.create_node(
                        "Person",
                        PropertyMapBuilder::new()
                            .insert("id", i as i64)
                            .insert("name", format!("User{}", i))
                            .build(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        });
    });

    // Benchmark 4: Large vector transaction
    group.bench_function("commit_100_ops_with_vectors", |b| {
        b.iter(|| {
            let db = GallifreyDB::new();
            db.write(|tx| {
                let embedding = vec![0.1f32; 128]; // Realistic embedding size
                for i in 0..100 {
                    tx.create_node(
                        "Document",
                        PropertyMapBuilder::new()
                            .insert("id", i as i64)
                            .insert_vector("embedding", &embedding)
                            .build(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
        });
    });

    group.finish();
}

fn configure_criterion() -> Criterion {
    let sample_size = std::env::var("BENCH_SAMPLE_SIZE")
        .map(|s| s.parse().unwrap_or(50))
        .unwrap_or(50);

    Criterion::default().sample_size(sample_size)
}

criterion_group!(
    name = benches;
    config = configure_criterion();
    targets = bench_read_transaction_creation,
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
    bench_concurrent_transaction_creation_with_active_txs,
    bench_apply_changes_large_tx,
    bench_commit_vector_vs_nonvector,
);

criterion_main!(benches);
