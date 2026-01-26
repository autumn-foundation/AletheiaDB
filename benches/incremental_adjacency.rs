//! Benchmarks for Incremental CSR Adjacency Index
//!
//! This benchmark suite validates performance targets:
//! - Insert: O(1) with no rebuild cliff
//! - Read (no delta): ~5-10ns (same as current CSR)
//! - Read (with delta): ~20-30ns (+15ns merge overhead)
//! - Compaction: O(E log E), <10ms for 10K edges

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use gallifreydb::core::id::{EdgeId, NodeId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
// Will uncomment as we implement:
// use gallifreydb::index::incremental_adjacency::{IncrementalAdjacencyIndex, IncrementalConfig};
// use gallifreydb::index::adjacency::AdjacencyIndex;
use std::sync::Arc;

// ============================================================================
// Benchmark: Insert Latency (Target: O(1), no cliff)
// ============================================================================

fn bench_insert_latency(_c: &mut Criterion) {
    // let mut group = c.benchmark_group("incremental_insert_latency");
    //
    // for num_existing_edges in [0, 1_000, 10_000, 100_000] {
    //     group.throughput(Throughput::Elements(1));
    //     group.bench_with_input(
    //         BenchmarkId::from_parameter(format!("{}existing", num_existing_edges)),
    //         &num_existing_edges,
    //         |b, &num_existing| {
    //             let index = setup_index_with_edges(num_existing);
    //             let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    //
    //             b.iter(|| {
    //                 let source = NodeId::new((num_existing + 1) as u64).unwrap();
    //                 let target = NodeId::new((num_existing + 2) as u64).unwrap();
    //                 let edge_id = EdgeId::new((num_existing + 1) as u64).unwrap();
    //                 let entry = AdjacencyEntry::new(target, edge_id, knows);
    //
    //                 index.insert(black_box(source), black_box(entry));
    //             });
    //         },
    //     );
    // }
    //
    // group.finish();
}

// ============================================================================
// Benchmark: Read Latency - No Delta (Target: ~5-10ns)
// ============================================================================

fn bench_read_no_delta(_c: &mut Criterion) {
    // // Benchmark fast path: read from frozen only
    // let mut group = c.benchmark_group("incremental_read_no_delta");
    //
    // for num_edges in [100, 1_000, 10_000] {
    //     group.throughput(Throughput::Elements(1));
    //     group.bench_with_input(
    //         BenchmarkId::from_parameter(format!("{}edges", num_edges)),
    //         &num_edges,
    //         |b, &num_edges| {
    //             let frozen_edges: Vec<_> = (0..num_edges)
    //                 .map(|i| {
    //                     (
    //                         NodeId::new(i as u64).unwrap(),
    //                         NodeId::new((i + 1) as u64).unwrap(),
    //                         EdgeId::new(i as u64).unwrap(),
    //                         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
    //                     )
    //                 })
    //                 .collect();
    //             let frozen = AdjacencyIndex::build(frozen_edges);
    //             let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));
    //
    //             let node = NodeId::new(num_edges / 2).unwrap();
    //
    //             b.iter(|| {
    //                 let guard = index.get_adjacency(black_box(node));
    //                 black_box(guard)
    //             });
    //         },
    //     );
    // }
    //
    // group.finish();
}

// ============================================================================
// Benchmark: Read Latency - With Delta (Target: ~20-30ns)
// ============================================================================

fn bench_read_with_delta(_c: &mut Criterion) {
    // // Benchmark merge path: read from frozen + delta
    // let mut group = c.benchmark_group("incremental_read_with_delta");
    //
    // for delta_ratio in [0.01, 0.05, 0.10] {
    //     let num_frozen = 10_000;
    //     let num_delta = (num_frozen as f64 * delta_ratio) as usize;
    //
    //     group.throughput(Throughput::Elements(1));
    //     group.bench_with_input(
    //         BenchmarkId::from_parameter(format!("{}pct_delta", (delta_ratio * 100.0) as usize)),
    //         &(num_frozen, num_delta),
    //         |b, &(num_frozen, num_delta)| {
    //             // Create frozen
    //             let frozen_edges: Vec<_> = (0..num_frozen)
    //                 .map(|i| {
    //                     (
    //                         NodeId::new(i as u64).unwrap(),
    //                         NodeId::new((i + 1) as u64).unwrap(),
    //                         EdgeId::new(i as u64).unwrap(),
    //                         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
    //                     )
    //                 })
    //                 .collect();
    //             let frozen = AdjacencyIndex::build(frozen_edges);
    //             let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));
    //
    //             // Add delta
    //             let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    //             for i in num_frozen..(num_frozen + num_delta) {
    //                 index.insert(
    //                     NodeId::new(i as u64).unwrap(),
    //                     AdjacencyEntry::new(
    //                         NodeId::new((i + 1) as u64).unwrap(),
    //                         EdgeId::new(i as u64).unwrap(),
    //                         knows,
    //                     ),
    //                 );
    //             }
    //
    //             // Query node with delta
    //             let node = NodeId::new(num_frozen as u64).unwrap();
    //
    //             b.iter(|| {
    //                 let guard = index.get_adjacency(black_box(node));
    //                 black_box(guard)
    //             });
    //         },
    //     );
    // }
    //
    // group.finish();
}

// ============================================================================
// Benchmark: Compaction Throughput (Target: <10ms for 10K edges)
// ============================================================================

fn bench_compaction_throughput(_c: &mut Criterion) {
    // let mut group = c.benchmark_group("incremental_compaction");
    //
    // for (num_frozen, num_delta) in [(1_000, 100), (10_000, 1_000), (100_000, 10_000)] {
    //     group.throughput(Throughput::Elements((num_frozen + num_delta) as u64));
    //     group.bench_with_input(
    //         BenchmarkId::new("edges", format!("{}+{}", num_frozen, num_delta)),
    //         &(num_frozen, num_delta),
    //         |b, &(num_frozen, num_delta)| {
    //             b.iter_batched(
    //                 || {
    //                     // Setup: create index with frozen + delta
    //                     let frozen_edges: Vec<_> = (0..num_frozen)
    //                         .map(|i| {
    //                             (
    //                                 NodeId::new(i as u64).unwrap(),
    //                                 NodeId::new((i + 1) as u64).unwrap(),
    //                                 EdgeId::new(i as u64).unwrap(),
    //                                 GLOBAL_INTERNER.intern("KNOWS").unwrap(),
    //                             )
    //                         })
    //                         .collect();
    //                     let frozen = AdjacencyIndex::build(frozen_edges);
    //                     let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));
    //
    //                     let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    //                     for i in num_frozen..(num_frozen + num_delta) {
    //                         index.insert(
    //                             NodeId::new(i as u64).unwrap(),
    //                             AdjacencyEntry::new(
    //                                 NodeId::new((i + 1) as u64).unwrap(),
    //                                 EdgeId::new(i as u64).unwrap(),
    //                                 knows,
    //                             ),
    //                         );
    //                     }
    //
    //                     index
    //                 },
    //                 |index| {
    //                     // Benchmark compaction
    //                     index.compact();
    //                     black_box(index)
    //                 },
    //                 criterion::BatchSize::SmallInput,
    //             );
    //         },
    //     );
    // }
    //
    // group.finish();
}

// ============================================================================
// Benchmark: Concurrent Read-Write
// ============================================================================

fn bench_concurrent_read_write(_c: &mut Criterion) {
    // use std::thread;
    //
    // let mut group = c.benchmark_group("incremental_concurrent");
    //
    // for num_threads in [2, 4, 8] {
    //     group.bench_with_input(
    //         BenchmarkId::new("threads", num_threads),
    //         &num_threads,
    //         |b, &num_threads| {
    //             let frozen_edges: Vec<_> = (0..10_000)
    //                 .map(|i| {
    //                     (
    //                         NodeId::new(i).unwrap(),
    //                         NodeId::new(i + 1).unwrap(),
    //                         EdgeId::new(i).unwrap(),
    //                         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
    //                     )
    //                 })
    //                 .collect();
    //             let frozen = AdjacencyIndex::build(frozen_edges);
    //             let index = Arc::new(IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen)));
    //
    //             b.iter(|| {
    //                 let handles: Vec<_> = (0..num_threads)
    //                     .map(|thread_id| {
    //                         let index_clone = Arc::clone(&index);
    //                         thread::spawn(move || {
    //                             let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    //                             for i in 0..100 {
    //                                 // Mix reads and writes
    //                                 if i % 2 == 0 {
    //                                     // Write
    //                                     let source = NodeId::new((thread_id * 1000 + i) as u64).unwrap();
    //                                     index_clone.insert(
    //                                         source,
    //                                         AdjacencyEntry::new(
    //                                             NodeId::new((source.as_u64() + 1)).unwrap(),
    //                                             EdgeId::new((thread_id * 1000 + i) as u64).unwrap(),
    //                                             knows,
    //                                         ),
    //                                     );
    //                                 } else {
    //                                     // Read
    //                                     let node = NodeId::new((i % 10_000) as u64).unwrap();
    //                                     let guard = index_clone.get_adjacency(node);
    //                                     black_box(guard.iter().count());
    //                                 }
    //                             }
    //                         })
    //                     })
    //                     .collect();
    //
    //                 for handle in handles {
    //                     handle.join().unwrap();
    //                 }
    //             });
    //         },
    //     );
    // }
    //
    // group.finish();
}

// ============================================================================
// Benchmark: Comparison with Current CSR
// ============================================================================

fn bench_comparison_current_csr(_c: &mut Criterion) {
    // // Benchmark to compare incremental CSR vs current CSR rebuild
    // let mut group = c.benchmark_group("comparison_csr");
    //
    // // Scenario: Insert 100 edges then query
    // group.bench_function("current_csr_insert_query", |b| {
    //     b.iter_batched(
    //         || {
    //             // Setup: CurrentIndexes with lazy rebuild
    //             let indexes = CurrentIndexes::new();
    //             let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    //
    //             // Pre-populate with 10K edges
    //             for i in 0..10_000 {
    //                 let edge = Edge::new(
    //                     EdgeId::new(i).unwrap(),
    //                     knows,
    //                     NodeId::new(i).unwrap(),
    //                     NodeId::new(i + 1).unwrap(),
    //                     PropertyMapBuilder::new().build(),
    //                     VersionId::new(1).unwrap(),
    //                 );
    //                 indexes.insert_edge(edge);
    //             }
    //             // Trigger rebuild
    //             indexes.rebuild_adjacency();
    //
    //             (indexes, knows)
    //         },
    //         |(indexes, knows)| {
    //             // Benchmark: insert 100 edges then query
    //             for i in 10_000..10_100 {
    //                 let edge = Edge::new(
    //                     EdgeId::new(i).unwrap(),
    //                     knows,
    //                     NodeId::new(i).unwrap(),
    //                     NodeId::new(i + 1).unwrap(),
    //                     PropertyMapBuilder::new().build(),
    //                     VersionId::new(1).unwrap(),
    //                 );
    //                 indexes.insert_edge(edge);
    //             }
    //
    //             // Query - THIS TRIGGERS REBUILD (the cliff!)
    //             let guard = indexes.get_outgoing(NodeId::new(5000).unwrap());
    //             black_box(guard)
    //         },
    //         criterion::BatchSize::SmallInput,
    //     );
    // });
    //
    // group.bench_function("incremental_csr_insert_query", |b| {
    //     b.iter_batched(
    //         || {
    //             // Setup: IncrementalAdjacencyIndex
    //             let frozen_edges: Vec<_> = (0..10_000)
    //                 .map(|i| {
    //                     (
    //                         NodeId::new(i).unwrap(),
    //                         NodeId::new(i + 1).unwrap(),
    //                         EdgeId::new(i).unwrap(),
    //                         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
    //                     )
    //                 })
    //                 .collect();
    //             let frozen = AdjacencyIndex::build(frozen_edges);
    //             let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));
    //
    //             (index, GLOBAL_INTERNER.intern("KNOWS").unwrap())
    //         },
    //         |(index, knows)| {
    //             // Benchmark: insert 100 edges then query
    //             for i in 10_000..10_100 {
    //                 index.insert(
    //                     NodeId::new(i).unwrap(),
    //                     AdjacencyEntry::new(
    //                         NodeId::new(i + 1).unwrap(),
    //                         EdgeId::new(i).unwrap(),
    //                         knows,
    //                     ),
    //                 );
    //             }
    //
    //             // Query - NO REBUILD, just merge
    //             let guard = index.get_adjacency(NodeId::new(5000).unwrap());
    //             black_box(guard)
    //         },
    //         criterion::BatchSize::SmallInput,
    //     );
    // });
    //
    // group.finish();
}

// ============================================================================
// Helper Functions
// ============================================================================

// fn setup_index_with_edges(num_edges: usize) -> IncrementalAdjacencyIndex {
//     // Helper to create an index with specified number of frozen edges
//     let frozen_edges: Vec<_> = (0..num_edges)
//         .map(|i| {
//             (
//                 NodeId::new(i as u64).unwrap(),
//                 NodeId::new((i + 1) as u64).unwrap(),
//                 EdgeId::new(i as u64).unwrap(),
//                 GLOBAL_INTERNER.intern("KNOWS").unwrap(),
//             )
//         })
//         .collect();
//     let frozen = AdjacencyIndex::build(frozen_edges);
//     IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen))
// }

criterion_group!(
    benches,
    bench_insert_latency,
    bench_read_no_delta,
    bench_read_with_delta,
    bench_compaction_throughput,
    bench_concurrent_read_write,
    bench_comparison_current_csr,
);
criterion_main!(benches);
