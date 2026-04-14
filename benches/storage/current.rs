use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::storage::current::CurrentStorage;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn create_large_graph(storage: &CurrentStorage, count: usize) {
    for i in 0..count {
        storage
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("Person_{}", i))
                    .insert("age", i as i64)
                    .build(),
            )
            .unwrap();
    }
}

fn bench_get_all_node_ids(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_current");

    // Setup storage with 10k nodes
    let storage = CurrentStorage::new();
    create_large_graph(&storage, 10_000);

    group.bench_function("get_all_node_ids_10k", |b| {
        b.iter(|| black_box(storage.get_all_node_ids()))
    });

    group.finish();
}

criterion_group!(benches, bench_get_all_node_ids);
criterion_main!(benches);
