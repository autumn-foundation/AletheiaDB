//! Benchmarks for AdjacencyIndex sparse vs dense scenarios.
//!
//! This benchmark demonstrates the performance improvements of the sparse
//! representation when dealing with large gaps in node IDs (e.g., after deletions).

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::{EdgeId, NodeId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::index::adjacency::AdjacencyIndex;

/// Build an index with dense node IDs (0, 1, 2, 3, ..., n)
fn build_dense_index(num_nodes: usize) -> AdjacencyIndex {
    let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let mut edges = Vec::new();

    for i in 0..num_nodes {
        let source = NodeId::new(i as u64).unwrap();
        let target = NodeId::new((i + 1) as u64).unwrap();
        let edge_id = EdgeId::new(i as u64).unwrap();
        edges.push((source, target, edge_id, knows));
    }

    AdjacencyIndex::build(edges)
}

/// Build an index with sparse node IDs (large gaps between IDs)
/// Simulates a database after many deletions
fn build_sparse_index(num_nodes: usize, gap_size: u64) -> AdjacencyIndex {
    let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let mut edges = Vec::new();

    for i in 0..num_nodes {
        let source_id = (i as u64) * gap_size;
        let target_id = source_id + 1;
        let source = NodeId::new(source_id).unwrap();
        let target = NodeId::new(target_id).unwrap();
        let edge_id = EdgeId::new(i as u64).unwrap();
        edges.push((source, target, edge_id, knows));
    }

    AdjacencyIndex::build(edges)
}

/// Benchmark building dense adjacency index
fn bench_build_dense(c: &mut Criterion) {
    let mut group = c.benchmark_group("adjacency_build_dense");

    for num_nodes in [100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(num_nodes as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(num_nodes),
            &num_nodes,
            |b, &num_nodes| {
                b.iter(|| {
                    let index = build_dense_index(num_nodes);
                    black_box(index);
                });
            },
        );
    }
    group.finish();
}

/// Benchmark building sparse adjacency index
fn bench_build_sparse(c: &mut Criterion) {
    let mut group = c.benchmark_group("adjacency_build_sparse");

    // Test with different gap sizes to simulate various deletion patterns
    for (num_nodes, gap_size) in [(1_000, 1_000), (1_000, 10_000), (1_000, 100_000)] {
        group.throughput(Throughput::Elements(num_nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("nodes", format!("{}nodes_gap{}", num_nodes, gap_size)),
            &(num_nodes, gap_size),
            |b, &(num_nodes, gap_size)| {
                b.iter(|| {
                    let index = build_sparse_index(num_nodes, gap_size);
                    black_box(index);
                });
            },
        );
    }
    group.finish();
}

/// Benchmark memory usage comparison
///
/// This indirectly measures memory efficiency by comparing build times
/// and structure size through the lens of overall index construction.
fn bench_memory_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("adjacency_memory");

    // Dense: 1000 nodes with IDs 0-999
    group.bench_function("dense_1000_nodes", |b| {
        b.iter(|| {
            let index = build_dense_index(1_000);
            // Force usage of index to prevent dead code elimination
            black_box(index.edge_count());
        });
    });

    // Sparse: 1000 nodes with max_id = 1_000_000 (gaps of 1000)
    // With optimization: internal structures should be proportional to actual nodes (1000)
    // Without optimization: would allocate for max_node_id (1_000_000)
    group.bench_function("sparse_1000_nodes_max1m", |b| {
        b.iter(|| {
            let index = build_sparse_index(1_000, 1_000);
            // Force usage of index to prevent dead code elimination
            black_box(index.edge_count());
        });
    });

    group.finish();
}

/// Benchmark lookup performance (get_adjacency)
fn bench_lookup_dense(c: &mut Criterion) {
    let index = build_dense_index(10_000);
    let node = NodeId::new(5_000).unwrap();

    c.bench_function("adjacency_lookup_dense", |b| {
        b.iter(|| {
            let adj = index.get_adjacency(black_box(node));
            black_box(adj);
        });
    });
}

/// Benchmark lookup performance on sparse index
fn bench_lookup_sparse(c: &mut Criterion) {
    let index = build_sparse_index(10_000, 1_000);
    // Node ID = 5_000 * 1_000 = 5_000_000
    let node = NodeId::new(5_000 * 1_000).unwrap();

    c.bench_function("adjacency_lookup_sparse", |b| {
        b.iter(|| {
            let adj = index.get_adjacency(black_box(node));
            black_box(adj);
        });
    });
}

/// Benchmark lookup for non-existent nodes
fn bench_lookup_missing(c: &mut Criterion) {
    let mut group = c.benchmark_group("adjacency_lookup_missing");

    // Dense index
    let dense_index = build_dense_index(10_000);
    let missing_node = NodeId::new(50_000).unwrap();
    group.bench_function("dense", |b| {
        b.iter(|| {
            let adj = dense_index.get_adjacency(black_box(missing_node));
            black_box(adj);
        });
    });

    // Sparse index with gaps
    let sparse_index = build_sparse_index(10_000, 1_000);
    // Try to find a node in the gap (should not exist)
    let gap_node = NodeId::new(5_000 * 1_000 + 500).unwrap();
    group.bench_function("sparse", |b| {
        b.iter(|| {
            let adj = sparse_index.get_adjacency(black_box(gap_node));
            black_box(adj);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_build_dense,
    bench_build_sparse,
    bench_memory_efficiency,
    bench_lookup_dense,
    bench_lookup_sparse,
    bench_lookup_missing
);
criterion_main!(benches);
