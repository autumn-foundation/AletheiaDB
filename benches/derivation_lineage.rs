//! Benchmarks for fact-to-fact derivation lineage (Issue #3371).
//!
//! Covers the issue's success metrics:
//! - **Blast radius**: downstream closure over a ~10K-fact lineage graph
//!   (depth ≤ 5) — target < 100ms p99.
//! - **Single-hop**: direct upstream/downstream lookup (depth 1) — target
//!   < 10µs p99.
//! - **Write overhead**: `create_node` carrying 10 `derived_from` refs vs a
//!   baseline `create_node` — target ≥ 90% of baseline write throughput.
//!
//! These are intentionally cheap to sample so CI/local runs stay fast; the
//! numeric targets are validated on the performance rig, not gated here.

use aletheiadb::AletheiaDB;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::lineage::{LineageQueryOptions, LineageRef};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Build a ~`levels * per_level`-fact lineage graph in `levels` waves. Wave 0
/// nodes derive from the single root; wave `k` nodes each derive from one wave
/// `k-1` node (round-robin), so the root's downstream closure spans exactly
/// `levels` hops. Returns `(db, root_ref, a_leaf_ref, a_penultimate_ref)`.
fn build_graph(
    levels: usize,
    per_level: usize,
) -> (AletheiaDB, LineageRef, LineageRef, LineageRef) {
    let db = AletheiaDB::new().expect("db init");
    let root = db
        .create_node("Root", PropertyMapBuilder::new().build())
        .expect("root");
    let root_ref = db.node_lineage_ref(root).expect("root ref");

    let mut prev: Vec<LineageRef> = vec![root_ref];
    let mut leaf_ref = root_ref;
    let mut penultimate_ref = root_ref;
    for level in 0..levels {
        let mut this: Vec<LineageRef> = Vec::with_capacity(per_level);
        for i in 0..per_level {
            let parent = prev[i % prev.len()];
            let id = db
                .create_node_with_lineage(
                    "Fact",
                    PropertyMapBuilder::new()
                        .insert("lvl", level as i64)
                        .build(),
                    &[parent],
                )
                .expect("derived node");
            this.push(db.node_lineage_ref(id).expect("ref"));
        }
        if level == levels - 1 {
            leaf_ref = this[0];
        }
        if level == levels - 2 {
            penultimate_ref = this[0];
        }
        prev = this;
    }
    (db, root_ref, leaf_ref, penultimate_ref)
}

/// Blast radius: downstream closure over ~10K facts, depth ≤ 5.
fn bench_downstream_blast_radius(c: &mut Criterion) {
    // 5 waves × 2000 = 10_000 derived facts, all within 5 hops of the root.
    let (db, root_ref, _leaf, _pen) = build_graph(5, 2000);
    let options = LineageQueryOptions::new()
        .with_max_depth(5)
        .with_limit(20_000);

    let mut group = c.benchmark_group("lineage_downstream_blast_radius");
    group.bench_function("10k_facts_depth5", |b| {
        b.iter(|| black_box(db.downstream_lineage(black_box(root_ref), options)));
    });
    group.finish();
}

/// Single-hop direct lineage lookups (depth 1) in both directions.
fn bench_single_hop(c: &mut Criterion) {
    let (db, _root_ref, leaf_ref, penultimate_ref) = build_graph(5, 2000);
    let one_hop = LineageQueryOptions::new().with_max_depth(1).with_limit(64);

    let mut group = c.benchmark_group("lineage_single_hop");
    // Direct parents of a leaf (exactly one upstream source).
    group.bench_function("direct_upstream", |b| {
        b.iter(|| black_box(db.upstream_lineage(black_box(leaf_ref), one_hop)));
    });
    // Direct children of a penultimate-level node (a small handful).
    group.bench_function("direct_downstream", |b| {
        b.iter(|| black_box(db.downstream_lineage(black_box(penultimate_ref), one_hop)));
    });
    group.finish();
}

/// Write overhead: `create_node` with 10 `derived_from` refs vs baseline.
fn bench_write_overhead(c: &mut Criterion) {
    let db = AletheiaDB::new().expect("db init");
    // Seed 10 source facts to derive from.
    let mut sources: Vec<LineageRef> = Vec::with_capacity(10);
    for i in 0..10 {
        let id = db
            .create_node(
                "Src",
                PropertyMapBuilder::new().insert("i", i as i64).build(),
            )
            .expect("src");
        sources.push(db.node_lineage_ref(id).expect("ref"));
    }

    let mut group = c.benchmark_group("lineage_write_overhead");
    group.bench_function("baseline_create_node", |b| {
        b.iter(|| {
            let id = db
                .create_node(black_box("N"), PropertyMapBuilder::new().build())
                .expect("create");
            black_box(id);
        });
    });
    group.bench_function("create_node_with_10_derived_from", |b| {
        b.iter(|| {
            let id = db
                .create_node_with_lineage(
                    black_box("N"),
                    PropertyMapBuilder::new().build(),
                    black_box(&sources),
                )
                .expect("create with lineage");
            black_box(id);
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_downstream_blast_radius,
    bench_single_hop,
    bench_write_overhead
);
criterion_main!(benches);
