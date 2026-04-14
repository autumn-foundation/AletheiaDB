//! Benchmarks for label-checking methods on `Node` and `Edge`.
//!
//! This compares the current `has_label_str` path against a candidate
//! resolve-and-compare path to guide optimization decisions with measurement.

use aletheiadb::core::graph::{Edge, Node};
use aletheiadb::core::id::{EdgeId, NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMapBuilder;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_node_label_checks(c: &mut Criterion) {
    let person = GLOBAL_INTERNER.intern("Person").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        person,
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    let mut group = c.benchmark_group("node_label_checks");
    group.bench_function("has_label_str_hit", |b| {
        b.iter(|| black_box(node.has_label_str("Person")))
    });
    group.bench_function("resolve_with_hit", |b| {
        b.iter(|| {
            black_box(
                GLOBAL_INTERNER
                    .resolve_with(node.label, |s| s == "Person")
                    .unwrap_or(false),
            )
        })
    });
    group.bench_function("has_label_str_miss", |b| {
        b.iter(|| black_box(node.has_label_str("Company")))
    });
    group.bench_function("resolve_with_miss", |b| {
        b.iter(|| {
            black_box(
                GLOBAL_INTERNER
                    .resolve_with(node.label, |s| s == "Company")
                    .unwrap_or(false),
            )
        })
    });
    group.finish();
}

fn bench_edge_label_checks(c: &mut Criterion) {
    let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        knows,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    let mut group = c.benchmark_group("edge_label_checks");
    group.bench_function("has_label_str_hit", |b| {
        b.iter(|| black_box(edge.has_label_str("KNOWS")))
    });
    group.bench_function("resolve_with_hit", |b| {
        b.iter(|| {
            black_box(
                GLOBAL_INTERNER
                    .resolve_with(edge.label, |s| s == "KNOWS")
                    .unwrap_or(false),
            )
        })
    });
    group.bench_function("has_label_str_miss", |b| {
        b.iter(|| black_box(edge.has_label_str("LIKES")))
    });
    group.bench_function("resolve_with_miss", |b| {
        b.iter(|| {
            black_box(
                GLOBAL_INTERNER
                    .resolve_with(edge.label, |s| s == "LIKES")
                    .unwrap_or(false),
            )
        })
    });
    group.finish();
}

criterion_group!(benches, bench_node_label_checks, bench_edge_label_checks);
criterion_main!(benches);
