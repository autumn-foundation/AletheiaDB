//! Write-path overhead benchmark for property-type / required-key schema
//! constraints (Issue #3378).
//!
//! Measures single-node create latency in two configurations, back-to-back in
//! one process run so the ratio is immune to cross-machine variance:
//!
//! - `baseline`   — no schema constraints declared (schemaless).
//! - `ten_active` — ten typed/required constraints declared on the target label.
//!
//! AC: p99 single-write latency with 10 declared constraints must stay within
//! 10% of the schemaless baseline. The per-write check is a single DashMap
//! lookup plus a bounded scan of the declared constraints, so the overhead is
//! expected to be well inside that budget.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::EntityKind;
use aletheiadb::core::constraint::DeclaredType;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn conforming_props() -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new()
        .insert("k0", 0i64)
        .insert("k1", 1i64)
        .insert("k2", 2i64)
        .insert("k3", 3i64)
        .insert("k4", 4i64)
        .insert("k5", "s5")
        .insert("k6", "s6")
        .insert("k7", "s7")
        .insert("k8", true)
        .insert("k9", 9i64)
        .build()
}

fn declare_ten(db: &AletheiaDB) {
    db.schema_constraint(EntityKind::Node, "Person")
        .require_typed("k0", DeclaredType::Integer)
        .require_typed("k1", DeclaredType::Integer)
        .require_typed("k2", DeclaredType::Integer)
        .require_typed("k3", DeclaredType::Integer)
        .require_typed("k4", DeclaredType::Integer)
        .require_typed("k5", DeclaredType::String)
        .require_typed("k6", DeclaredType::String)
        .require_typed("k7", DeclaredType::String)
        .require_typed("k8", DeclaredType::Boolean)
        .require_typed("k9", DeclaredType::Integer)
        .enable()
        .unwrap();
}

fn bench_schema_constraints(c: &mut Criterion) {
    let mut group = c.benchmark_group("schema_constraint_write");

    group.bench_function("baseline_no_constraints", |b| {
        let db = AletheiaDB::new().unwrap();
        b.iter(|| {
            let id = db
                .create_node("Person", black_box(conforming_props()))
                .unwrap();
            black_box(id);
        });
    });

    group.bench_function("ten_declared_constraints", |b| {
        let db = AletheiaDB::new().unwrap();
        declare_ten(&db);
        b.iter(|| {
            let id = db
                .create_node("Person", black_box(conforming_props()))
                .unwrap();
            black_box(id);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_schema_constraints);
criterion_main!(benches);
