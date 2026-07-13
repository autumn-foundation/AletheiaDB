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
//! for sweep vs paged. This proxy counts **candidate ids examined**: for the
//! sweep it is `max_id` (every id in `[0, max_id)` is probed); for the paged
//! strategy it is the honest `live * pages` per-page re-enumeration cost, NOT
//! merely the live ids materialized. The single-page sparse case (1 page) has
//! `paged_work == live`; the multi-page case (`live > K`) exposes the
//! `* pages` factor, which is why it is included -- a single-page-only proxy
//! would understate paged cost and read as an artificial "~live" win.

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

/// Print the one-off honest work-unit proxy for a sparse graph and register the
/// sweep/paged/auto wall-clock benches under `group`.
///
/// `pages_hint` is the expected number of paged re-enumerations
/// (`ceil(live / K)`); the multi-page case (`pages_hint > 1`) is what makes the
/// honest `paged_work == live * pages` factor visible -- a single-page case
/// alone would read as `paged_work == live` and hide the per-page re-scan cost.
fn sparse_case(c: &mut Criterion, name: &str, total: u64, stride: u64) {
    let mut group = c.benchmark_group(format!("node_scan/sparse/{name}"));
    let current = sparse_graph(total, stride);

    let live = current.node_count() as u64;
    let max_id = current.get_max_node_id();
    let pages = live.div_ceil(PAGE_SIZE as u64).max(1);

    // One-off count proxy: sweep examines max_id ids; paged examines the honest
    // per-page enumeration cost live * pages (NOT just the live ids retained).
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
        "[node_scan/sparse/{name}] live={live} max_id={max_id} pages~={pages} | \
         sweep: rows={sweep_rows} work_units={sweep_work} | \
         paged: rows={paged_rows} work_units={paged_work} (== live*pages = {}) | \
         work reduction ~{}x",
        live.saturating_mul(pages),
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

fn bench_sparse_full_scan(c: &mut Criterion) {
    // Single-page: ~1M ids ever allocated, keep every 10_000th -> ~100 live
    // nodes (1 page). The once-huge-now-tiny profile the issue targets.
    sparse_case(c, "single_page", 1_000_000, 10_000);

    // Multi-page: ~1M ids ever allocated, keep every 100th -> ~10_000 live
    // nodes, which at K=4096 spans ceil(10000/4096) = 3 pages. This stresses the
    // honest work proxy: paged_work == live * 3, not live.
    sparse_case(c, "multi_page", 1_000_000, 100);
}

criterion_group!(benches, bench_dense_full_scan, bench_sparse_full_scan);
criterion_main!(benches);
