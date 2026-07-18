//! Belief-revision audit performance gate (Issue #3362).
//!
//! Success metric: a belief-revision audit over an entity with ≤100 recorded
//! versions completes in **< 10 ms** (the temporal-reconstruction target).
//!
//! The engine is O(versions) over a single `historical.read()` history fetch —
//! no extra storage round-trip per pair. This bench builds a ~100-version
//! history on one node and measures a full audit.
//!
//! Gated behind `semantic-temporal` (the cohort the feature lives in); run with:
//! `cargo bench --no-default-features --features semantic-temporal --bench belief_revision`.

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::id::EntityId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::experimental::temporal::belief_revision::RevisionOptions;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Create one node and rewrite it ~100 times, alternating corrections (same
/// valid_from) and world-changes (advancing valid_from) so every classification
/// branch is exercised.
fn build_history(db: &AletheiaDB, versions: usize) -> EntityId {
    let base = time::now().wallclock();
    let vt_early = Timestamp::new(base - 1_000_000_000, 0).unwrap();
    let id = db
        .create_node_with_options(
            "Doc",
            PropertyMapBuilder::new().insert("rev", 0_i64).build(),
            WriteRequestOptions::new().with_valid_from(vt_early),
        )
        .unwrap();
    for i in 1..versions {
        // Even i: correction (reuse vt_early). Odd i: world-change (advance).
        let vf = if i % 2 == 0 {
            vt_early
        } else {
            Timestamp::new(base - 1_000_000_000 + i as i64 * 1_000, 0).unwrap()
        };
        db.update_node_with_options(
            id,
            PropertyMapBuilder::new().insert("rev", i as i64).build(),
            WriteRequestOptions::new().with_valid_from(vf),
        )
        .unwrap();
    }
    EntityId::Node(id)
}

fn bench_audit_100_versions(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    let entity = build_history(&db, 100);
    let opts = RevisionOptions::default();

    c.bench_function("belief_revisions_100_versions", |b| {
        b.iter(|| {
            let log = db.belief_revisions(black_box(entity), &opts).unwrap();
            black_box(log.revisions().len());
        });
    });
}

criterion_group!(benches, bench_audit_100_versions);
criterion_main!(benches);
