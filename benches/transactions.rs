//! Transaction overhead benchmarks
//!
//! These benchmarks measure the overhead introduced by the transaction layer
//! compared to direct operations.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::{GallifreyDB, PropertyMapBuilder, ReadOps, WriteOps};

fn bench_read_transaction_creation(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("read_transaction_creation", |b| {
        b.iter(|| {
            let _tx = db.read_transaction();
        });
    });
}

fn bench_write_transaction_creation(c: &mut Criterion) {
    let db = GallifreyDB::new();

    c.bench_function("write_transaction_creation", |b| {
        b.iter(|| {
            let _tx = db.write_transaction();
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
            let mut tx = db.write_transaction();
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
            let mut tx = db.write_transaction();
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
    let edge_ids: Vec<_> = db.write(|tx| {
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
                let edge_ids: Vec<_> = db.write(|tx| {
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
);

criterion_main!(benches);
