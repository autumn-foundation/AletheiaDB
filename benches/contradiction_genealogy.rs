//! Contradiction-genealogy performance gate (Issue #3352).
//!
//! Success metric: reconstructing a contradiction genealogy over an entity with
//! ≤ 1K recorded versions completes well under the 50 ms p99 target (the
//! temporal-reconstruction budget). The engine is O(versions²) per entity in the
//! conflicting-pair scan but runs over a single `historical.read()` history
//! fetch — no extra storage round-trip per pair.
//!
//! Gated behind `semantic-temporal`; run with:
//! `cargo bench --no-default-features --features semantic-temporal --bench contradiction_genealogy`.

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::id::EntityId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::experimental::temporal::contradiction_genealogy::{
    ContradictionTarget, GenealogyOptions,
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Create one node and rewrite its `ceo` property `versions` times, alternating
/// same-valid-period corrections (retroactive) and forward, never-retracted
/// value changes (contemporaneous), so the history is dense with overlapping
/// competing claims — the worst case for the pair scan.
fn build_history(db: &AletheiaDB, versions: usize) -> EntityId {
    let base = time::now().wallclock();
    let vt_early = Timestamp::new(base - 1_000_000_000, 0).unwrap();
    let id = db
        .create_node_with_options(
            "Company",
            PropertyMapBuilder::new().insert("ceo", "P0").build(),
            WriteRequestOptions::new().with_valid_from(vt_early),
        )
        .unwrap();
    for i in 1..versions {
        // Even i: retroactive (reuse vt_early). Odd i: contemporaneous (advance,
        // but never below the creation valid_from).
        let vf = if i % 2 == 0 {
            vt_early
        } else {
            Timestamp::new(base - 1_000_000_000 + i as i64 * 1_000, 0).unwrap()
        };
        db.update_node_with_options(
            id,
            PropertyMapBuilder::new()
                .insert("ceo", format!("P{i}"))
                .build(),
            WriteRequestOptions::new().with_valid_from(vf),
        )
        .unwrap();
    }
    EntityId::Node(id)
}

fn bench_genealogy_1k_versions(c: &mut Criterion) {
    let db = AletheiaDB::new().unwrap();
    let entity = build_history(&db, 1000);
    let opts = GenealogyOptions::default();

    c.bench_function("contradiction_genealogy_1k_versions", |b| {
        b.iter(|| {
            let g = db
                .contradiction_genealogy(
                    ContradictionTarget::EntityProperty {
                        entity: black_box(entity),
                        property: "ceo".to_string(),
                    },
                    &opts,
                )
                .unwrap();
            black_box(g.claims.len());
        });
    });
}

criterion_group!(benches, bench_genealogy_1k_versions);
criterion_main!(benches);
