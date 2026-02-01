//! Benchmarks for edge iteration performance.
//!
//! Measures the overhead of cloning Edge structs during iteration.
//! After optimization (returning DashMap guards), iteration is ~46% faster
//! when edge data is accessed without taking ownership.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::graph::Edge;
use gallifreydb::core::id::{EdgeId, NodeId, VersionId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::index::current::CurrentIndexes;
use gallifreydb::storage::current::CurrentStorage;

fn bench_outgoing_traversal(c: &mut Criterion) {
    let storage = CurrentStorage::new();
    let props = PropertyMapBuilder::new().build();

    // Create source node
    let source = storage.create_node("Source", props.clone()).unwrap();

    // Create 1000 edges
    for _ in 0..1000 {
        let target = storage.create_node("Target", props.clone()).unwrap();
        storage
            .create_edge(source, target, "KNOWS", props.clone())
            .unwrap();
    }

    storage.compact_adjacency(); // Ensure optimized structure

    let mut group = c.benchmark_group("traversal");

    group.bench_function("vec_allocation_lookup", |b| {
        b.iter(|| {
            let edges = storage.get_outgoing_edges(source);
            let mut sum = 0;
            for edge_id in edges {
                // This simulates what SemanticPathfinder was doing:
                // 1. Get Vec<EdgeId>
                // 2. Lookup target for each edge (DashMap access)
                let target = storage.get_edge_target(edge_id).unwrap();
                sum += target.as_u64();
            }
            black_box(sum)
        })
    });

    group.bench_function("zero_copy_iter", |b| {
        b.iter(|| {
            let mut sum = 0;
            // This is the new optimized path:
            // 1. Iterator (no Vec)
            // 2. Direct access to target (no DashMap lookup)
            for entry in storage.get_outgoing_entries_iter(source) {
                sum += entry.target.as_u64();
            }
            black_box(sum)
        })
    });

    group.finish();
}

fn bench_iter_edges(c: &mut Criterion) {
    let indexes = CurrentIndexes::new();
    let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let props = PropertyMapBuilder::new().build();
    let version = VersionId::new(1).unwrap();

    // Insert 10,000 edges
    for i in 0..10000 {
        let edge = Edge::new(
            EdgeId::new(i).unwrap(),
            knows,
            NodeId::new(0).unwrap(),
            NodeId::new(i + 1).unwrap(),
            props.clone(),
            version,
        );
        indexes.insert_edge(edge);
    }

    c.bench_function("iter_edges", |b| {
        b.iter(|| {
            // Simulate reading access (e.g. summing IDs) to ensure we use the data
            let mut sum = 0;
            for edge in indexes.iter_edges() {
                sum += edge.id.as_u64();
            }
            black_box(sum)
        })
    });
}

criterion_group!(benches, bench_iter_edges, bench_outgoing_traversal);
criterion_main!(benches);
