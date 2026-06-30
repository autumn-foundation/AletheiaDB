//! Uniqueness constraint write-path benchmarks (Issue #3218).
//!
//! Performance target (CLAUDE.md §current-state queries):
//! - Constrained `create_node` must stay within 1µs overhead vs. unconstrained.

mod common;

use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_email() -> String {
    format!("user{}@x", COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn bench_create_node_unconstrained(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();

    c.bench_function("create_node_unconstrained", |b| {
        b.iter(|| {
            db.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("email", black_box(unique_email().as_str()))
                    .build(),
            )
            .unwrap()
        });
    });
}

fn bench_create_node_constrained(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    db.unique_constraint("Person", "email").enable().unwrap();

    c.bench_function("create_node_constrained", |b| {
        b.iter(|| {
            db.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("email", black_box(unique_email().as_str()))
                    .build(),
            )
            .unwrap()
        });
    });
}

fn bench_create_node_constrained_violation(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    db.unique_constraint("Person", "email").enable().unwrap();
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("email", "dup@x").build(),
    )
    .unwrap();

    c.bench_function("create_node_constrained_violation", |b| {
        b.iter(|| {
            let _ = db.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("email", black_box("dup@x"))
                    .build(),
            );
        });
    });
}

criterion_group!(
    benches,
    bench_create_node_unconstrained,
    bench_create_node_constrained,
    bench_create_node_constrained_violation,
);
criterion_main!(benches);
