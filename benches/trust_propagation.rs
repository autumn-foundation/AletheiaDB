//! Trust-propagation micro-benchmarks (Issue #3382, AC8).
//!
//! Quantifies the **opt-in on-cost** of computed confidence: the lazy read-time
//! `computed_confidence` scalar walk and the explainable `trust_breakdown` tree
//! build, over a fixed derivation-lineage fixture.
//!
//! Zero-overhead **when the feature is DISABLED** is guaranteed by construction —
//! the entire trust module is `#[cfg(feature = "semantic-reasoning")]`-gated and
//! is never called from the write/read path — and is verified by
//! `cargo build --no-default-features` (the code compiles out completely). This
//! benchmark instead measures the cost you pay only when you opt in to the
//! feature, so the on-cost is quantified rather than assumed.

#[cfg(feature = "semantic-reasoning")]
use aletheiadb::AletheiaDB;
#[cfg(feature = "semantic-reasoning")]
use aletheiadb::core::PropertyMapBuilder;
#[cfg(feature = "semantic-reasoning")]
use aletheiadb::core::lineage::LineageRef;
#[cfg(feature = "semantic-reasoning")]
use aletheiadb::experimental::reasoning::trust_propagation::{
    MissingConfidencePolicy, TrustOptions, TrustPolicy,
};
#[cfg(feature = "semantic-reasoning")]
use criterion::{Criterion, criterion_group, criterion_main};
#[cfg(feature = "semantic-reasoning")]
use std::hint::black_box;

/// Build a fixed derivation fixture: `depth` merge levels, each combining the
/// previous conclusion with a fresh confidence-carrying root, so the top fact's
/// upstream closure is `depth` hops deep. Returns `(db, top_ref)`.
#[cfg(feature = "semantic-reasoning")]
fn build_fixture(depth: usize, policy: TrustPolicy) -> (AletheiaDB, LineageRef) {
    let db = AletheiaDB::new().expect("db init");
    db.set_trust_policy(policy).expect("policy");

    let seed = db
        .create_node_with_lineage(
            "Doc",
            PropertyMapBuilder::new()
                .insert("confidence", 0.9_f64)
                .build(),
            &[],
        )
        .expect("seed");
    let mut top = db.node_lineage_ref(seed).expect("seed ref");
    for level in 0..depth {
        let root = db
            .create_node_with_lineage(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("lvl", level as i64)
                    .build(),
                &[],
            )
            .expect("root");
        let root_ref = db.node_lineage_ref(root).expect("root ref");
        let merged = db
            .create_node_with_lineage("Merge", PropertyMapBuilder::new().build(), &[top, root_ref])
            .expect("merge");
        top = db.node_lineage_ref(merged).expect("merge ref");
    }
    (db, top)
}

/// `computed_confidence`: the lazy scalar walk over the fixture closure.
#[cfg(feature = "semantic-reasoning")]
fn bench_computed_confidence(c: &mut Criterion) {
    let mut group = c.benchmark_group("trust_computed_confidence");
    for &depth in &[8_usize, 32] {
        let (db, top) = build_fixture(
            depth,
            TrustPolicy::weakest_link(MissingConfidencePolicy::Zero),
        );
        group.bench_function(format!("weakest_link_depth_{depth}"), |b| {
            b.iter(|| black_box(db.computed_confidence(black_box(top))));
        });
        let (db, top) = build_fixture(depth, TrustPolicy::noisy_or(MissingConfidencePolicy::Zero));
        group.bench_function(format!("noisy_or_depth_{depth}"), |b| {
            b.iter(|| black_box(db.computed_confidence(black_box(top))));
        });
    }
    group.finish();
}

/// `trust_breakdown`: the explainable tree build (shared-memo, O(n)).
#[cfg(feature = "semantic-reasoning")]
fn bench_trust_breakdown(c: &mut Criterion) {
    let mut group = c.benchmark_group("trust_breakdown");
    let options = TrustOptions::new();
    for &depth in &[8_usize, 32] {
        let (db, top) = build_fixture(
            depth,
            TrustPolicy::weakest_link(MissingConfidencePolicy::Zero),
        );
        group.bench_function(format!("depth_{depth}"), |b| {
            b.iter(|| black_box(db.trust_breakdown(black_box(top), &options)));
        });
    }
    group.finish();
}

#[cfg(feature = "semantic-reasoning")]
criterion_group!(benches, bench_computed_confidence, bench_trust_breakdown);
#[cfg(feature = "semantic-reasoning")]
criterion_main!(benches);

#[cfg(not(feature = "semantic-reasoning"))]
fn main() {}
