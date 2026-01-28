use criterion::{criterion_group, criterion_main, Criterion, black_box};
use gallifreydb::index::current::CurrentIndexes;
use gallifreydb::core::graph::Edge;
use gallifreydb::core::id::{EdgeId, NodeId, VersionId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::PropertyMapBuilder;

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
            NodeId::new(i+1).unwrap(),
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

criterion_group!(benches, bench_iter_edges);
criterion_main!(benches);
