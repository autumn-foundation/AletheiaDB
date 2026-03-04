use aletheiadb::core::id::NodeId;
use aletheiadb::core::vector::SparseVec;
use aletheiadb::index::vector::sparse::{ScoringMethod, SparseIndexConfig, SparseVectorIndex};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_sparse_search(c: &mut Criterion) {
    let index = SparseVectorIndex::new(SparseIndexConfig {
        dimensions: 10_000,
        scoring: ScoringMethod::Cosine,
        ..Default::default()
    })
    .unwrap();

    // Add some dummy vectors
    for i in 0..100 {
        let doc = SparseVec::new(vec![i, i + 1, i + 2], vec![1.0, 2.0, 3.0], 10_000).unwrap();
        index.add(NodeId::new(i as u64).unwrap(), &doc).unwrap();
    }

    let query = SparseVec::new(vec![42, 43], vec![1.0, 2.0], 10_000).unwrap();

    c.bench_function("sparse_search", |b| {
        b.iter(|| index.search(black_box(&query), black_box(10)).unwrap())
    });
}

criterion_group!(benches, bench_sparse_search);
criterion_main!(benches);
