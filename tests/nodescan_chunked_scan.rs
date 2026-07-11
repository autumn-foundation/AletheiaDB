//! Perf-correctness tests for `NodeScanIterator` chunked scanning (Issue #3422).
//!
//! PR #3418 made a full node scan stream ids over `[0, max_id)`: O(1) resident
//! memory but **O(max_id) time**. Node ids are monotonic and never reused, so
//! after a bulk delete (or a sparse post-compaction id space) `max_id` is the
//! high-water mark of ids ever allocated, not the live count -- a scan then
//! spends almost all its time skipping dead ids.
//!
//! Issue #3422 recovers **O(live) time** by paging the live keys of
//! `node_headers` in bounded chunks instead of sweeping the dense id range.
//! These tests assert both the *correctness* of the result set under heavy
//! tombstone accumulation and the *perf-correctness* property that scan work
//! tracks the live node count, not `max_id`. The work-proportionality assertion
//! FAILS on the pre-#3422 sweep-only implementation (where a full scan always
//! examines `max_id` candidates).

use std::collections::BTreeSet;
use std::sync::Arc;

use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::id::NodeId;
use aletheiadb::query::executor::{NodeScanIterator, ResultIterator, ScanStrategy};
use aletheiadb::storage::current::CurrentStorage;

/// Build a graph with `total` nodes (ids `0..total`), then delete every node
/// except the ones whose id is in `keep`, producing a sparse id space with a
/// large `max_id` (== `total`) but only `keep.len()` live nodes.
fn sparse_graph(total: u64, keep: &[u64]) -> Arc<CurrentStorage> {
    let current = Arc::new(CurrentStorage::new());
    for i in 0..total {
        current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", i as i64).build(),
            )
            .expect("create_node");
    }
    let keep_set: BTreeSet<u64> = keep.iter().copied().collect();
    for i in 0..total {
        if !keep_set.contains(&i) {
            current
                .delete_node(NodeId::new(i).expect("valid id"))
                .expect("delete_node");
        }
    }
    current
}

/// Drain a scan fully, returning the set of yielded node ids.
fn drain_ids(mut iter: NodeScanIterator) -> BTreeSet<u64> {
    let mut ids = BTreeSet::new();
    while let Some(res) = iter.next() {
        let row = res.expect("scan row");
        let node = row.entity.as_node().expect("node entity");
        ids.insert(node.id.as_u64());
    }
    ids
}

/// The headline perf-correctness property: over a large-`max_id`, few-live
/// graph, a full scan visits work proportional to the LIVE count, not `max_id`.
///
/// On the pre-#3422 sweep-only iterator this fails: `work_units` == `max_id`
/// (10_000), which is ~2500x the live count.
#[test]
fn full_scan_work_is_live_proportional_not_maxid() {
    const TOTAL: u64 = 10_000;
    let keep = [3u64, 41, 4242, 9999];
    let current = sparse_graph(TOTAL, &keep);

    assert_eq!(current.node_count(), keep.len(), "live count sanity");
    assert_eq!(current.get_max_node_id(), TOTAL, "max_id sanity");

    let mut iter = NodeScanIterator::new(None, Arc::clone(&current));
    let mut yielded = 0u64;
    while let Some(res) = iter.next() {
        res.expect("scan row");
        yielded += 1;
    }

    assert_eq!(
        yielded,
        keep.len() as u64,
        "must yield exactly the live nodes"
    );

    // Live-proportional: allow generous slack (paging touches ~live headers
    // plus per-page overhead) but far below the O(max_id) sweep cost.
    let live = keep.len() as u64;
    assert!(
        iter.work_units() <= live * 8 + 64,
        "full scan examined {} candidates for {} live nodes (max_id={}); \
         expected O(live), not O(max_id). Chunked iteration regressed.",
        iter.work_units(),
        live,
        TOTAL,
    );
}

/// Auto strategy must pick paging for a sparse graph, so its work is
/// live-proportional (directly contrasts with the forced sweep below).
#[test]
fn auto_selects_paged_for_sparse_forced_sweep_stays_maxid() {
    const TOTAL: u64 = 8_000;
    let keep = [1u64, 2, 3, 7, 4000, 7999];
    let current = sparse_graph(TOTAL, &keep);

    let paged = NodeScanIterator::new(None, Arc::clone(&current));
    let paged_work = {
        let mut it = paged;
        while let Some(r) = it.next() {
            r.expect("row");
        }
        it.work_units()
    };

    let sweep =
        NodeScanIterator::with_strategy(None, Arc::clone(&current), ScanStrategy::ForceSweep, 4096);
    let sweep_work = {
        let mut it = sweep;
        while let Some(r) = it.next() {
            r.expect("row");
        }
        it.work_units()
    };

    assert_eq!(
        sweep_work, TOTAL,
        "forced sweep examines every id in [0, max_id)"
    );
    assert!(
        paged_work < sweep_work / 10,
        "auto/paged work ({paged_work}) should be far below sweep work ({sweep_work})",
    );
}

/// Dense graphs must keep the sweep (no regression): auto work == max_id.
#[test]
fn auto_selects_sweep_for_dense() {
    let current = Arc::new(CurrentStorage::new());
    for i in 0..2_000u64 {
        current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", i as i64).build(),
            )
            .expect("create_node");
    }
    let max_id = current.get_max_node_id();

    let mut iter = NodeScanIterator::new(None, Arc::clone(&current));
    let mut yielded = 0u64;
    while let Some(r) = iter.next() {
        r.expect("row");
        yielded += 1;
    }
    assert_eq!(yielded, 2_000, "all dense nodes yielded");
    assert_eq!(
        iter.work_units(),
        max_id,
        "dense graph should sweep, examining every id",
    );
}

/// Paged and sweep strategies must return the identical result set under heavy
/// tombstone accumulation (correctness of the chunked path).
#[test]
fn paged_and_sweep_return_identical_results() {
    const TOTAL: u64 = 5_000;
    let keep = [0u64, 5, 6, 7, 100, 2500, 2501, 4999];
    let current = sparse_graph(TOTAL, &keep);

    let paged = drain_ids(NodeScanIterator::with_strategy(
        None,
        Arc::clone(&current),
        ScanStrategy::ForcePaged,
        4096,
    ));
    let sweep = drain_ids(NodeScanIterator::with_strategy(
        None,
        Arc::clone(&current),
        ScanStrategy::ForceSweep,
        4096,
    ));

    let expected: BTreeSet<u64> = keep.iter().copied().collect();
    assert_eq!(paged, expected, "paged result set");
    assert_eq!(sweep, expected, "sweep result set");
    assert_eq!(paged, sweep, "paged and sweep must agree");
}

/// Multi-page keyset paging (tiny page size) must be duplicate-free and
/// gap-free: every live node exactly once, ascending, across many pages.
#[test]
fn tiny_page_size_covers_all_live_nodes_without_dup_or_gap() {
    const TOTAL: u64 = 200;
    // Keep 13 live nodes, forcing ceil(13/2) = 7 pages at page_size = 2.
    let keep = [1u64, 3, 9, 10, 11, 44, 45, 46, 99, 100, 150, 198, 199];
    let current = sparse_graph(TOTAL, &keep);

    let mut iter = NodeScanIterator::with_strategy(
        None,
        Arc::clone(&current),
        ScanStrategy::ForcePaged,
        2, // tiny page size -> many pages
    );

    let mut seen: Vec<u64> = Vec::new();
    while let Some(r) = iter.next() {
        let row = r.expect("row");
        seen.push(row.entity.as_node().expect("node").id.as_u64());
    }

    // No duplicates.
    let unique: BTreeSet<u64> = seen.iter().copied().collect();
    assert_eq!(unique.len(), seen.len(), "no duplicate ids across pages");
    // Exactly the live set.
    let expected: BTreeSet<u64> = keep.iter().copied().collect();
    assert_eq!(unique, expected, "every live node exactly once");
}

/// The label fast-path must be preserved in the paged strategy: only nodes with
/// the requested label are yielded, and an unknown label yields nothing.
#[test]
fn paged_label_filter_and_unknown_label() {
    let current = Arc::new(CurrentStorage::new());
    let mut person_ids = Vec::new();
    for i in 0..300u64 {
        let label = if i % 3 == 0 { "Person" } else { "Widget" };
        let id = current
            .create_node(
                label,
                PropertyMapBuilder::new().insert("n", i as i64).build(),
            )
            .expect("create_node");
        if label == "Person" {
            person_ids.push(id.as_u64());
        }
    }
    // Delete a chunk of Widgets to sparsify the id space.
    for i in 1..250u64 {
        if i % 3 != 0 {
            let _ = current.delete_node(NodeId::new(i).expect("id"));
        }
    }
    let live_persons: BTreeSet<u64> = person_ids.into_iter().collect();

    // Label filter under forced paging returns exactly the Person nodes.
    let paged = drain_ids(NodeScanIterator::with_strategy(
        Some("Person".to_string()),
        Arc::clone(&current),
        ScanStrategy::ForcePaged,
        4096,
    ));
    assert_eq!(paged, live_persons, "paged label filter matches Persons");

    // Same via sweep for cross-check.
    let sweep = drain_ids(NodeScanIterator::with_strategy(
        Some("Person".to_string()),
        Arc::clone(&current),
        ScanStrategy::ForceSweep,
        4096,
    ));
    assert_eq!(sweep, live_persons, "sweep label filter matches Persons");

    // Unknown label yields nothing on both strategies.
    let unknown_paged = drain_ids(NodeScanIterator::with_strategy(
        Some("NeverInterned".to_string()),
        Arc::clone(&current),
        ScanStrategy::ForcePaged,
        4096,
    ));
    assert!(unknown_paged.is_empty(), "unknown label -> empty (paged)");
    let unknown_auto = drain_ids(NodeScanIterator::new(
        Some("NeverInterned".to_string()),
        Arc::clone(&current),
    ));
    assert!(unknown_auto.is_empty(), "unknown label -> empty (auto)");
}

/// Empty graph: both strategies yield nothing and do no work.
#[test]
fn empty_graph_yields_nothing() {
    let current = Arc::new(CurrentStorage::new());
    let auto = drain_ids(NodeScanIterator::new(None, Arc::clone(&current)));
    assert!(auto.is_empty());
    let paged = drain_ids(NodeScanIterator::with_strategy(
        None,
        Arc::clone(&current),
        ScanStrategy::ForcePaged,
        4096,
    ));
    assert!(paged.is_empty());
}
