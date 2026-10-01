//! Counterfactual exclusion replay performance gate (Issue #3357).
//!
//! Success metric (design §Success Metric): counterfactual replay over the
//! 10K-node / 50K-edge reference history with ~10% of writes excluded completes
//! in **< 30 s**, and current-state view reads meet ≤ 10× the live single-lookup
//! target (< 10 µs).
//!
//! The reference sizes are the [`REF_NODES`] / [`REF_EDGES`] consts. They are
//! deliberately parameterised: building the full reference history dominates
//! wall-clock here, so a fast local/CI run can shrink them, but the perf gate is
//! defined at 10_000 / 50_000. The history is built ONCE in setup (outside the
//! measured closure); only `counterfactual_replay` / `get_node` are timed.
//!
//! Gated behind `semantic-temporal`; run with:
//! `cargo bench --no-default-features --features semantic-temporal --bench counterfactual_replay`.

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::experimental::temporal::counterfactual::{
    CounterfactualConfig, ExclusionPredicate,
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Reference node count for the perf gate (design Success Metric).
const REF_NODES: usize = 10_000;
/// Reference edge count for the perf gate.
const REF_EDGES: usize = 50_000;
/// Fraction of writes attributed to the excluded ("poison") source (~10%).
const POISON_EVERY: usize = 10;

fn prov(source: &str) -> Provenance {
    Provenance::builder()
        .source(source)
        .build()
        .expect("valid provenance")
}

/// Build a ~`nodes`-node / `edges`-edge attributed history where roughly one in
/// `POISON_EVERY` writes comes from the `"poison"` source (the exclusion target)
/// and the rest are round-robin clean feeds. Returns the created node ids.
fn build_history(db: &AletheiaDB, nodes: usize, edges: usize) -> Vec<NodeId> {
    let feeds = ["feed-0", "feed-1", "feed-2", "feed-3"];
    let mut ids = Vec::with_capacity(nodes);
    for i in 0..nodes {
        let src = if i % POISON_EVERY == 0 {
            "poison"
        } else {
            feeds[i % feeds.len()]
        };
        let id = db
            .create_node_with_options(
                "Doc",
                PropertyMapBuilder::new().insert("v", i as i64).build(),
                WriteRequestOptions::new().with_provenance(prov(src)),
            )
            .expect("create node");
        ids.push(id);
    }
    for e in 0..edges {
        let src_node = ids[e % nodes];
        let tgt_node = ids[(e * 7 + 1) % nodes];
        let src = if e % POISON_EVERY == 0 {
            "poison"
        } else {
            feeds[e % feeds.len()]
        };
        let _ = db.create_edge_with_options(
            src_node,
            tgt_node,
            "LINKS",
            PropertyMapBuilder::new().insert("w", e as i64).build(),
            WriteRequestOptions::new().with_provenance(prov(src)),
        );
    }
    ids
}

fn bench_materialize(c: &mut Criterion) {
    let db = AletheiaDB::new().expect("db");
    let _ids = build_history(&db, REF_NODES, REF_EDGES);

    let mut group = c.benchmark_group("counterfactual_replay");
    // Materialization is a heavy one-shot; keep the sample count modest.
    group.sample_size(10);
    group.bench_function("materialize_10k_50k_exclude_poison", |b| {
        b.iter(|| {
            let view = db
                .counterfactual_replay(
                    "no-poison",
                    ExclusionPredicate::source("poison"),
                    CounterfactualConfig::default(),
                )
                .expect("view materializes");
            black_box(view.report().excluded_writes());
        });
    });
    group.finish();
}

fn bench_view_read(c: &mut Criterion) {
    let db = AletheiaDB::new().expect("db");
    let ids = build_history(&db, REF_NODES, REF_EDGES);
    let view = db
        .counterfactual_replay(
            "no-poison",
            ExclusionPredicate::source("poison"),
            CounterfactualConfig::default(),
        )
        .expect("view materializes");
    // Pick a surviving node (not a poison-created one) for the read bench.
    let probe = ids
        .iter()
        .copied()
        .find(|&id| view.get_node(id).is_ok())
        .expect("at least one surviving node");

    c.bench_function("counterfactual_view_get_node", |b| {
        b.iter(|| {
            black_box(view.get_node(black_box(probe)).expect("present"));
        });
    });
}

criterion_group!(benches, bench_materialize, bench_view_read);
criterion_main!(benches);
