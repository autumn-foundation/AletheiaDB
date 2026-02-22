use aletheiadb::index::current::CurrentIndexes;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_import_csr_clones(c: &mut Criterion) {
    let mut group = c.benchmark_group("import_csr_clones");

    // Test with significant size to make clone cost visible
    // 1M edges = 8MB per vector.
    let num_edges = 1_000_000;

    // Pre-allocate vectors
    let mut outgoing_node_ids = Vec::with_capacity(num_edges);
    let mut outgoing_offsets = Vec::with_capacity(num_edges);
    let mut outgoing_edge_ids = Vec::with_capacity(num_edges);

    for i in 0..num_edges {
        outgoing_node_ids.push(i as u64);
        outgoing_offsets.push(i as u64);
        outgoing_edge_ids.push(i as u64);
    }

    let incoming_node_ids = outgoing_node_ids.clone();
    let incoming_offsets = outgoing_offsets.clone();
    let incoming_edge_ids = outgoing_edge_ids.clone();

    group.bench_function("import_1m_empty_map", |b| {
        b.iter_with_setup(
            || {
                (
                    CurrentIndexes::new(),
                    outgoing_node_ids.clone(),
                    outgoing_offsets.clone(),
                    outgoing_edge_ids.clone(),
                    incoming_node_ids.clone(),
                    incoming_offsets.clone(),
                    incoming_edge_ids.clone(),
                )
            },
            |(indexes, out_n, out_o, out_e, in_n, in_o, in_e)| {
                indexes.import_csr(
                    black_box(out_n),
                    black_box(out_o),
                    black_box(out_e),
                    black_box(in_n),
                    black_box(in_o),
                    black_box(in_e),
                );
            },
        )
    });

    group.finish();
}

criterion_group!(benches, bench_import_csr_clones);
criterion_main!(benches);
