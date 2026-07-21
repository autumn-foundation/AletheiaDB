//! Secondary property-index benchmark: scan vs indexed equality lookup.
//!
//! Builds a 100K-node store and measures a **selective** equality lookup
//! (`find_nodes_by_property`) two ways:
//!
//! - `scan`: no index enabled — the pre-existing O(nodes-per-label) full scan.
//! - `indexed`: the same lookup with a secondary equality index enabled — an
//!   O(matches) `DashMap` probe.
//!
//! The point is the asymptotic gap: the scan cost grows with the label
//! population while the indexed probe does not, so the indexed path should be
//! orders of magnitude faster at 100K nodes.

use std::hint::black_box;
use std::sync::Arc;

use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::property::PropertyValue;
use aletheiadb::storage::current::CurrentStorage;
use criterion::{Criterion, criterion_group, criterion_main};

const N: usize = 100_000;

/// Build a store of `N` `Person` nodes, each with a unique `email` and a
/// low-cardinality `city`. Returns the store; the index is enabled by the caller
/// so the `scan` and `indexed` cases share identical data.
fn build_store() -> Arc<CurrentStorage> {
    let current = Arc::new(CurrentStorage::new());
    for i in 0..N {
        current
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("email", format!("user{i}@example.com"))
                    .insert("city", if i % 2 == 0 { "NYC" } else { "SF" })
                    .build(),
            )
            .expect("create_node");
    }
    current
}

fn bench_property_lookup(c: &mut Criterion) {
    // A selective lookup: exactly one of the 100K nodes matches.
    let needle = PropertyValue::from("user50000@example.com".to_string());

    let scan_store = build_store();

    let indexed_store = build_store();
    indexed_store
        .enable_property_index("Person", "email")
        .expect("enable index");

    let mut group = c.benchmark_group("property_lookup_selective_100k");

    group.bench_function("scan", |b| {
        b.iter(|| {
            black_box(scan_store.find_nodes_by_property(
                black_box("Person"),
                black_box("email"),
                black_box(&needle),
            ))
        });
    });

    group.bench_function("indexed", |b| {
        b.iter(|| {
            black_box(indexed_store.find_nodes_by_property(
                black_box("Person"),
                black_box("email"),
                black_box(&needle),
            ))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_property_lookup);
criterion_main!(benches);
