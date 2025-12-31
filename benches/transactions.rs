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
);

criterion_main!(benches);
