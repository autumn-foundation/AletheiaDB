//! Benchmarks for Incremental CSR Adjacency Index
//!
//! This benchmark suite validates performance targets:
//! - Insert: O(1) with no rebuild cliff
//! - Read (no delta): ~5-10ns (same as current CSR)
//! - Read (with delta): ~20-30ns (+15ns merge overhead)
//! - Compaction: O(E log E), <10ms for 10K edges

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::{EdgeId, NodeId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::index::adjacency::{AdjacencyEntry, AdjacencyIndex};
use gallifreydb::index::incremental_adjacency::IncrementalAdjacencyIndex;
use std::sync::Arc;

// ============================================================================
// Benchmark: Insert Latency (Target: O(1), no cliff)
// ============================================================================

fn bench_insert_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_insert_latency");

    for num_existing_edges in [0, 1_000, 10_000, 100_000] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}existing", num_existing_edges)),
            &num_existing_edges,
            |b, &num_existing| {
                let index = setup_index_with_edges(num_existing);
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");

                let mut counter = 0u64;
                b.iter(|| {
                    let source =
                        NodeId::new((num_existing as u64) + counter + 1).expect("valid node id");
                    let target =
                        NodeId::new((num_existing as u64) + counter + 2).expect("valid node id");
                    let edge_id =
                        EdgeId::new((num_existing as u64) + counter + 1).expect("valid edge id");
                    let entry = AdjacencyEntry::new(target, edge_id, knows);

                    index.insert(black_box(source), black_box(entry));
                    counter += 1;
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Read Latency - No Delta (Target: ~5-10ns)
// ============================================================================

fn bench_read_no_delta(c: &mut Criterion) {
    // Benchmark fast path: read from frozen only
    let mut group = c.benchmark_group("incremental_read_no_delta");

    for num_edges in [100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}edges", num_edges)),
            &num_edges,
            |b, &num_edges| {
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
                let frozen_edges: Vec<_> = (0..num_edges)
                    .map(|i| {
                        (
                            NodeId::new(i as u64).expect("valid node id"),
                            NodeId::new((i + 1) as u64).expect("valid node id"),
                            EdgeId::new(i as u64).expect("valid edge id"),
                            knows,
                        )
                    })
                    .collect();
                let frozen = AdjacencyIndex::build(frozen_edges);
                let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

                let node = NodeId::new((num_edges / 2) as u64).expect("valid node id");

                b.iter(|| {
                    let guard = index.get_adjacency(black_box(node));
                    black_box(guard)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Read Latency - With Delta (Target: ~20-30ns)
// ============================================================================

fn bench_read_with_delta(c: &mut Criterion) {
    // Benchmark merge path: read from frozen + delta
    let mut group = c.benchmark_group("incremental_read_with_delta");

    for delta_ratio in [0.01, 0.05, 0.10] {
        let num_frozen = 10_000usize;
        let num_delta = (num_frozen as f64 * delta_ratio) as usize;

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}pct_delta", (delta_ratio * 100.0) as usize)),
            &(num_frozen, num_delta),
            |b, &(num_frozen, num_delta)| {
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");

                // Create frozen
                let frozen_edges: Vec<_> = (0..num_frozen)
                    .map(|i| {
                        (
                            NodeId::new(i as u64).expect("valid node id"),
                            NodeId::new((i + 1) as u64).expect("valid node id"),
                            EdgeId::new(i as u64).expect("valid edge id"),
                            knows,
                        )
                    })
                    .collect();
                let frozen = AdjacencyIndex::build(frozen_edges);
                let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

                // Add delta
                for i in num_frozen..(num_frozen + num_delta) {
                    index.insert(
                        NodeId::new(i as u64).expect("valid node id"),
                        AdjacencyEntry::new(
                            NodeId::new((i + 1) as u64).expect("valid node id"),
                            EdgeId::new(i as u64).expect("valid edge id"),
                            knows,
                        ),
                    );
                }

                // Query node with delta
                let node = NodeId::new(num_frozen as u64).expect("valid node id");

                b.iter(|| {
                    let guard = index.get_adjacency(black_box(node));
                    black_box(guard)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Compaction Throughput (Target: <10ms for 10K edges)
// ============================================================================

fn bench_compaction_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_compaction");

    for (num_frozen, num_delta) in [(1_000, 100), (10_000, 1_000), (100_000, 10_000)] {
        group.throughput(Throughput::Elements((num_frozen + num_delta) as u64));
        group.bench_with_input(
            BenchmarkId::new("edges", format!("{}+{}", num_frozen, num_delta)),
            &(num_frozen, num_delta),
            |b, &(num_frozen, num_delta)| {
                b.iter_batched(
                    || {
                        // Setup: create index with frozen + delta
                        let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
                        let frozen_edges: Vec<_> = (0..num_frozen)
                            .map(|i| {
                                (
                                    NodeId::new(i as u64).expect("valid node id"),
                                    NodeId::new((i + 1) as u64).expect("valid node id"),
                                    EdgeId::new(i as u64).expect("valid edge id"),
                                    knows,
                                )
                            })
                            .collect();
                        let frozen = AdjacencyIndex::build(frozen_edges);
                        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

                        for i in num_frozen..(num_frozen + num_delta) {
                            index.insert(
                                NodeId::new(i as u64).expect("valid node id"),
                                AdjacencyEntry::new(
                                    NodeId::new((i + 1) as u64).expect("valid node id"),
                                    EdgeId::new(i as u64).expect("valid edge id"),
                                    knows,
                                ),
                            );
                        }

                        index
                    },
                    |index| {
                        // Benchmark compaction
                        index.compact();
                        black_box(index)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Concurrent Read-Write
// ============================================================================

fn bench_concurrent_read_write(c: &mut Criterion) {
    use std::thread;

    let mut group = c.benchmark_group("incremental_concurrent");

    for num_threads in [2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
                let frozen_edges: Vec<_> = (0..10_000)
                    .map(|i| {
                        (
                            NodeId::new(i).expect("valid node id"),
                            NodeId::new(i + 1).expect("valid node id"),
                            EdgeId::new(i).expect("valid edge id"),
                            knows,
                        )
                    })
                    .collect();
                let frozen = AdjacencyIndex::build(frozen_edges);
                let index = Arc::new(IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen)));

                b.iter(|| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|thread_id| {
                            let index_clone = Arc::clone(&index);
                            let knows_inner = knows;
                            thread::spawn(move || {
                                for i in 0..100 {
                                    // Mix reads and writes
                                    if i % 2 == 0 {
                                        // Write
                                        let source = NodeId::new((thread_id * 1000 + i) as u64)
                                            .expect("valid node id");
                                        let target = NodeId::new((thread_id * 1000 + i + 1) as u64)
                                            .expect("valid node id");
                                        index_clone.insert(
                                            source,
                                            AdjacencyEntry::new(
                                                target,
                                                EdgeId::new((thread_id * 1000 + i) as u64)
                                                    .expect("valid edge id"),
                                                knows_inner,
                                            ),
                                        );
                                    } else {
                                        // Read
                                        let node = NodeId::new((i % 10_000) as u64)
                                            .expect("valid node id");
                                        let guard = index_clone.get_adjacency(node);
                                        black_box(guard.iter().count());
                                    }
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().expect("thread join");
                    }
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: FrozenAdjacencyView Hot Path (Target: ~10ns)
// ============================================================================

fn bench_frozen_view(c: &mut Criterion) {
    let mut group = c.benchmark_group("frozen_view_hot_path");

    for num_edges in [1_000, 10_000, 100_000] {
        group.throughput(Throughput::Elements(1));

        // Benchmark FrozenAdjacencyView (direct slice access)
        group.bench_with_input(
            BenchmarkId::new("frozen_view", format!("{}edges", num_edges)),
            &num_edges,
            |b, &num_edges| {
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
                let frozen_edges: Vec<_> = (0..num_edges)
                    .map(|i| {
                        (
                            NodeId::new(i as u64).expect("valid node id"),
                            NodeId::new((i + 1) as u64).expect("valid node id"),
                            EdgeId::new(i as u64).expect("valid edge id"),
                            knows,
                        )
                    })
                    .collect();
                let frozen = AdjacencyIndex::build(frozen_edges);
                let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

                // Get frozen view (should be available since no delta/tombstones)
                let view = index.frozen_view().expect("frozen view available");
                let node = NodeId::new((num_edges / 2) as u64).expect("valid node id");

                b.iter(|| {
                    let slice = view.get_adjacency(black_box(node));
                    black_box(slice)
                });
            },
        );

        // Compare with get_adjacency (MergedAdjacencyGuard)
        group.bench_with_input(
            BenchmarkId::new("merged_guard", format!("{}edges", num_edges)),
            &num_edges,
            |b, &num_edges| {
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
                let frozen_edges: Vec<_> = (0..num_edges)
                    .map(|i| {
                        (
                            NodeId::new(i as u64).expect("valid node id"),
                            NodeId::new((i + 1) as u64).expect("valid node id"),
                            EdgeId::new(i as u64).expect("valid edge id"),
                            knows,
                        )
                    })
                    .collect();
                let frozen = AdjacencyIndex::build(frozen_edges);
                let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

                let node = NodeId::new((num_edges / 2) as u64).expect("valid node id");

                b.iter(|| {
                    let guard = index.get_adjacency(black_box(node));
                    black_box(guard)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Comparison with Current CSR (rebuild cliff)
// ============================================================================

fn bench_comparison_current_csr(c: &mut Criterion) {
    // Benchmark to compare incremental CSR vs full rebuild
    let mut group = c.benchmark_group("comparison_csr");

    // Test incremental CSR insert + query (no rebuild cliff)
    group.bench_function("incremental_csr_insert_query", |b| {
        b.iter_batched(
            || {
                // Setup: IncrementalAdjacencyIndex
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
                let frozen_edges: Vec<_> = (0..10_000)
                    .map(|i| {
                        (
                            NodeId::new(i).expect("valid node id"),
                            NodeId::new(i + 1).expect("valid node id"),
                            EdgeId::new(i).expect("valid edge id"),
                            knows,
                        )
                    })
                    .collect();
                let frozen = AdjacencyIndex::build(frozen_edges);
                let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

                (index, knows)
            },
            |(index, knows)| {
                // Benchmark: insert 100 edges then query
                for i in 10_000..10_100 {
                    index.insert(
                        NodeId::new(i).expect("valid node id"),
                        AdjacencyEntry::new(
                            NodeId::new(i + 1).expect("valid node id"),
                            EdgeId::new(i).expect("valid edge id"),
                            knows,
                        ),
                    );
                }

                // Query - NO REBUILD, just merge
                let guard = index.get_adjacency(NodeId::new(5000).expect("valid node id"));
                let count = guard.iter().count();
                black_box(count)
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Test full rebuild scenario (simulates the rebuild cliff)
    group.bench_function("full_rebuild_insert_query", |b| {
        b.iter_batched(
            || {
                // Setup: Create base frozen index
                let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
                let edges: Vec<_> = (0..10_000)
                    .map(|i| {
                        (
                            NodeId::new(i).expect("valid node id"),
                            NodeId::new(i + 1).expect("valid node id"),
                            EdgeId::new(i).expect("valid edge id"),
                            knows,
                        )
                    })
                    .collect();
                (edges, knows)
            },
            |(mut edges, knows)| {
                // Benchmark: add 100 edges then full rebuild
                for i in 10_000..10_100 {
                    edges.push((
                        NodeId::new(i).expect("valid node id"),
                        NodeId::new(i + 1).expect("valid node id"),
                        EdgeId::new(i).expect("valid edge id"),
                        knows,
                    ));
                }

                // Full rebuild (the cliff!)
                let rebuilt = AdjacencyIndex::build(edges);
                let node = NodeId::new(5000).expect("valid node id");
                let count = rebuilt.get_adjacency(node).len();
                black_box(count)
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ============================================================================
// Helper Functions
// ============================================================================

fn setup_index_with_edges(num_edges: usize) -> IncrementalAdjacencyIndex {
    // Helper to create an index with specified number of frozen edges
    if num_edges == 0 {
        return IncrementalAdjacencyIndex::new();
    }

    let knows = GLOBAL_INTERNER.intern("KNOWS").expect("intern KNOWS");
    let frozen_edges: Vec<_> = (0..num_edges)
        .map(|i| {
            (
                NodeId::new(i as u64).expect("valid node id"),
                NodeId::new((i + 1) as u64).expect("valid node id"),
                EdgeId::new(i as u64).expect("valid edge id"),
                knows,
            )
        })
        .collect();
    let frozen = AdjacencyIndex::build(frozen_edges);
    IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen))
}

criterion_group!(
    benches,
    bench_insert_latency,
    bench_read_no_delta,
    bench_read_with_delta,
    bench_frozen_view,
    bench_compaction_throughput,
    bench_concurrent_read_write,
    bench_comparison_current_csr,
);
criterion_main!(benches);
