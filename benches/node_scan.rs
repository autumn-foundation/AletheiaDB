//! Full node-scan benchmarks (Issue #3422).
//!
//! There was previously no node-scan benchmark, so full-scan latency
//! regressions -- notably the O(max_id) worst case introduced by PR #3418's
//! streaming rewrite -- were not caught by `just bench`. This adds:
//!
//! - `dense`: a full scan over a dense graph (baseline; sweep is optimal here).
//! - `sparse/sweep` vs `sparse/paged`: a sparse, deletion-heavy graph (large
//!   `max_id`, few live nodes) that exposes the O(max_id) worst case and
//!   demonstrates the chunked-iteration recovery to O(live).
//!
//! Alongside the wall-clock timings, each run prints the `work_units` proxy
//! (candidate ids examined) for sweep vs paged, which is the count-based proxy
//! the issue calls for: sweep == `max_id`, paged ~= live.

use std::hint::black_box;
use std::sync::Arc;

use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::id::NodeId;
use aletheiadb::query::executor::{NodeScanIterator, ResultIterator, ScanStrategy};
use aletheiadb::storage::current::CurrentStorage;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

const PAGE_SIZE: usize = 4096;

/// Dense graph: ids `0..n`, all live.
fn dense_graph(n: u64) -> Arc<CurrentStorage> {
    let current = Arc::new(CurrentStorage::new());
    for i in 0..n {
        current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", i as i64).build(),
            )
            .expect("create_node");
    }
    current
}

/// Sparse graph: create `total` nodes, then delete all but every
/// `keep_stride`-th node, leaving a large `max_id` with few live nodes.
fn sparse_graph(total: u64, keep_stride: u64) -> Arc<CurrentStorage> {
    let current = dense_graph(total);
    for i in 0..total {
        if !i.is_multiple_of(keep_stride) {
            let _ = current.delete_node(NodeId::new(i).expect("id"));
        }
    }
    current
}

/// Drain a scan fully, returning (rows, work_units).
fn drain(mut iter: NodeScanIterator) -> (u64, u64) {
    let mut rows = 0u64;
    while let Some(r) = iter.next() {
        black_box(r.expect("row"));
        rows += 1;
    }
    (rows, iter.work_units())
}

fn bench_dense_full_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_scan/dense");
    for n in [1_000u64, 10_000, 50_000] {
        let current = dense_graph(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &current, |b, current| {
            b.iter(|| {
                let iter = NodeScanIterator::new(None, Arc::clone(current));
                black_box(drain(iter))
            });
        });
    }
    group.finish();
}

fn bench_sparse_full_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_scan/sparse");

    // ~1M ids ever allocated, keep every 10_000th -> ~100 live nodes: the
    // once-huge-now-tiny profile the issue targets.
    let total = 1_000_000u64;
    let stride = 10_000u64;
    let current = sparse_graph(total, stride);

    let live = current.node_count() as u64;
    let max_id = current.get_max_node_id();

    // One-off count proxy: sweep examines max_id ids; paged examines ~live.
    let (sweep_rows, sweep_work) = drain(NodeScanIterator::with_strategy(
        None,
        Arc::clone(&current),
        ScanStrategy::ForceSweep,
        PAGE_SIZE,
    ));
    let (paged_rows, paged_work) = drain(NodeScanIterator::with_strategy(
        None,
        Arc::clone(&current),
        ScanStrategy::ForcePaged,
        PAGE_SIZE,
    ));
    eprintln!(
        "[node_scan/sparse] live={live} max_id={max_id} | \
         sweep: rows={sweep_rows} work_units={sweep_work} | \
         paged: rows={paged_rows} work_units={paged_work} | \
         work reduction ~{}x",
        sweep_work / paged_work.max(1),
    );
    assert_eq!(
        sweep_rows, paged_rows,
        "sweep and paged must yield the same rows"
    );

    group.bench_function("sweep", |b| {
        b.iter(|| {
            let iter = NodeScanIterator::with_strategy(
                None,
                Arc::clone(&current),
                ScanStrategy::ForceSweep,
                PAGE_SIZE,
            );
            black_box(drain(iter))
        });
    });
    group.bench_function("paged", |b| {
        b.iter(|| {
            let iter = NodeScanIterator::with_strategy(
                None,
                Arc::clone(&current),
                ScanStrategy::ForcePaged,
                PAGE_SIZE,
            );
            black_box(drain(iter))
        });
    });
    group.bench_function("auto", |b| {
        b.iter(|| {
            let iter = NodeScanIterator::new(None, Arc::clone(&current));
            black_box(drain(iter))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_dense_full_scan, bench_sparse_full_scan);
criterion_main!(benches);
