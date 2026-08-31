//! Background adjacency maintenance (Issue #3810).
//!
//! ADR-0026 designed a two-tier adjacency index (frozen CSR + delta buffer)
//! whose read fast path is only reachable when the delta buffer and the
//! tombstone set are *globally empty*. Phase 5 shipped a `CompactionScheduler`
//! that nothing outside the tests ever started, so every `get_outgoing_edges` /
//! `get_incoming_edges` call in a real database permanently took the merged
//! (delta) path.
//!
//! These tests pin the shipped behavior: a database built through the public
//! API drains its delta/tombstone layers on its own once writes go quiet, the
//! maintenance costs one process-wide worker (not two threads per database),
//! it is opt-out-able, and it never changes what a read returns.

use aletheiadb::AletheiaDBConfig;
use aletheiadb::index::adjacency_maintenance::{self, AdjacencyMaintenanceConfig};
use aletheiadb::prelude::*;
use serial_test::serial;
use std::time::{Duration, Instant};

/// Poll `cond` until it holds or `timeout` elapses. Returns whether it held.
///
/// Background maintenance is time-driven, so assertions about it are written as
/// bounded polls rather than fixed sleeps: a passing run finishes as soon as the
/// worker has done its job, and a failing run still terminates.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Timeout generous enough to absorb a loaded CI box; a healthy run converges
/// in ~2 maintenance ticks.
const CONVERGE: Duration = Duration::from_secs(20);

/// Build a small graph: `nodes` nodes, each with `out_degree` outgoing edges.
///
/// Deliberately far below `IncrementalConfig::max_delta_edges` (10,000) so the
/// absolute size threshold can never fire -- this is exactly the "threshold
/// bootstrap gap" case from Issue #3810 where `should_compact()`'s ratio branch
/// is also dead (frozen starts at 0).
fn build_graph(db: &AletheiaDB, nodes: usize, out_degree: usize) -> Vec<NodeId> {
    let ids: Vec<NodeId> = (0..nodes)
        .map(|i| {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("n{i}"))
                .build();
            db.create_node("Person", props).expect("create_node")
        })
        .collect();

    for i in 0..nodes {
        for j in 0..out_degree {
            let target = ids[(i + j + 1) % nodes];
            db.create_edge(ids[i], target, "KNOWS", PropertyMapBuilder::new().build())
                .expect("create_edge");
        }
    }
    ids
}

/// AC1/AC2/AC4: a database built through the public API compacts on its own
/// after a write burst, so subsequent reads reach the frozen CSR fast path.
#[test]
#[serial]
fn background_maintenance_drains_delta_after_write_burst() {
    let db = AletheiaDB::new().expect("create db");
    build_graph(&db, 200, 3);

    // Before maintenance runs, the delta layer holds the edges (this is the
    // state the issue found the database stuck in forever).
    let stats = db.adjacency_stats();
    assert!(
        stats.outgoing.delta_edges > 0 || stats.outgoing.frozen_edges > 0,
        "edges must be recorded somewhere: {stats:?}"
    );

    assert!(
        wait_until(CONVERGE, || db.adjacency_stats().is_fully_compacted()),
        "background maintenance never drained the delta buffer: {:?}",
        db.adjacency_stats()
    );

    let stats = db.adjacency_stats();
    assert_eq!(stats.outgoing.delta_edges, 0);
    assert_eq!(stats.incoming.delta_edges, 0);
    assert_eq!(stats.outgoing.frozen_edges, 600);
    assert_eq!(stats.incoming.frozen_edges, 600);
}

/// AC4: the drained graph is below every configured compaction threshold, so
/// the pre-existing `should_compact()` policy alone would never have fired.
/// This is the documented "threshold bootstrap gap" -- closed by the
/// write-quiescence trigger, not by the size thresholds.
#[test]
#[serial]
fn small_graph_below_size_thresholds_still_reaches_the_fast_path() {
    use aletheiadb::index::{IncrementalAdjacencyIndex, IncrementalConfig};

    let cfg = IncrementalConfig::default();
    // A stand-alone index in exactly the state our database is in after the
    // write burst below: fresh (frozen == 0) with a small delta.
    let probe = IncrementalAdjacencyIndex::new();
    for i in 1..=50u64 {
        probe.insert(
            NodeId::new(1).unwrap(),
            aletheiadb::index::AdjacencyEntry::new(
                NodeId::new(i + 1).unwrap(),
                EdgeId::new(i).unwrap(),
                aletheiadb::core::interning::InternedString::from_raw(1),
            ),
        );
    }
    assert!(
        !probe.should_compact(),
        "precondition: 50 delta edges on a fresh index is below max_delta_edges ({}) \
         and the ratio branch is dead while frozen == 0",
        cfg.max_delta_edges
    );

    // ... yet the shipping database still drains it.
    let db = AletheiaDB::new().expect("create db");
    build_graph(&db, 20, 2);
    assert!(
        wait_until(CONVERGE, || db.adjacency_stats().is_fully_compacted()),
        "small graph never reached the frozen fast path: {:?}",
        db.adjacency_stats()
    );
}

/// AC1: deletions (tombstones) are drained too -- a tombstone is just as
/// effective at disabling the fast path as a delta edge.
#[test]
#[serial]
fn background_maintenance_drains_tombstones_after_deletes() {
    let db = AletheiaDB::new().expect("create db");
    let ids = build_graph(&db, 30, 2);

    assert!(
        wait_until(CONVERGE, || db.adjacency_stats().is_fully_compacted()),
        "initial build never compacted"
    );

    // Delete a node's edges by cascading the node away.
    db.write(|tx| tx.delete_node_cascade(ids[0]))
        .expect("cascade delete");

    assert!(
        wait_until(CONVERGE, || db.adjacency_stats().is_fully_compacted()),
        "tombstones were never drained: {:?}",
        db.adjacency_stats()
    );
    assert_eq!(db.adjacency_stats().outgoing.tombstones, 0);
}

/// AC5: background compaction is invisible to readers -- the edges reachable
/// from a node are identical before and after a compaction, and remain correct
/// when reads, writes and compaction all interleave.
#[test]
#[serial]
fn adjacency_reads_are_unchanged_by_background_compaction() {
    let db = AletheiaDB::new().expect("create db");
    let ids = build_graph(&db, 40, 3);

    let before: Vec<_> = {
        let mut v = db.get_outgoing_edges(ids[0]);
        v.sort();
        v
    };
    assert_eq!(before.len(), 3);

    assert!(
        wait_until(CONVERGE, || db.adjacency_stats().is_fully_compacted()),
        "never compacted"
    );

    let after: Vec<_> = {
        let mut v = db.get_outgoing_edges(ids[0]);
        v.sort();
        v
    };
    assert_eq!(before, after, "compaction changed a read result");

    // Interleave a write with maintenance and re-check both directions.
    let extra = db
        .create_edge(ids[0], ids[7], "KNOWS", PropertyMapBuilder::new().build())
        .expect("create_edge");
    assert!(
        wait_until(CONVERGE, || db.adjacency_stats().is_fully_compacted()),
        "never re-compacted after a late write"
    );
    let outgoing = db.get_outgoing_edges(ids[0]);
    assert_eq!(outgoing.len(), 4);
    assert!(outgoing.contains(&extra));
    assert!(db.get_incoming_edges(ids[7]).contains(&extra));
}

/// AC6: maintenance is default-on but opt-out-able through the unified config.
#[test]
#[serial]
fn maintenance_can_be_disabled_via_config() {
    let config = AletheiaDBConfig::builder()
        .adjacency(AdjacencyMaintenanceConfig::disabled())
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("create db");
    build_graph(&db, 20, 2);

    // Give a would-be worker far longer than its tick interval to act.
    std::thread::sleep(Duration::from_millis(500));
    let stats = db.adjacency_stats();
    assert!(
        !stats.is_fully_compacted(),
        "maintenance ran despite being disabled: {stats:?}"
    );
    assert_eq!(stats.outgoing.frozen_edges, 0);

    // The explicit API still works when the background worker is off.
    db.compact_adjacency();
    assert!(db.adjacency_stats().is_fully_compacted());
}

/// AC3: maintenance costs ONE process-wide worker thread, not two threads per
/// database -- the objection that blocked wiring the per-index scheduler in.
///
/// Measured differentially: a database spawns threads for other subsystems too
/// (WAL flusher, ...), so the meaningful quantity is how many *extra* threads
/// the same databases cost with maintenance on versus off.
#[cfg(target_os = "linux")]
#[test]
#[serial]
fn maintenance_does_not_spawn_a_thread_per_database() {
    fn thread_count() -> usize {
        std::fs::read_dir("/proc/self/task")
            .expect("read /proc/self/task")
            .count()
    }

    const DBS: usize = 20;

    fn make(maintenance: AdjacencyMaintenanceConfig) -> Vec<AletheiaDB> {
        (0..DBS)
            .map(|_| {
                let config = AletheiaDBConfig::builder()
                    .adjacency(maintenance.clone())
                    .build();
                let db = AletheiaDB::with_unified_config(config).expect("create db");
                build_graph(&db, 5, 1);
                db
            })
            .collect()
    }

    // Warm up so the one-off shared worker is not attributed to the group below.
    let warmup = AletheiaDB::new().expect("create db");
    build_graph(&warmup, 5, 1);
    assert!(wait_until(CONVERGE, || warmup
        .adjacency_stats()
        .is_fully_compacted()));
    assert!(adjacency_maintenance::worker_is_running());

    let before_off = thread_count();
    let without = make(AdjacencyMaintenanceConfig::disabled());
    let cost_without = thread_count().saturating_sub(before_off);
    drop(without);

    let before_on = thread_count();
    let with = make(AdjacencyMaintenanceConfig::default());
    let cost_with = thread_count().saturating_sub(before_on);

    assert_eq!(with.len(), DBS);
    assert!(
        cost_with <= cost_without + 2,
        "{DBS} databases cost {cost_with} threads with background adjacency \
         maintenance and {cost_without} without it; maintenance must not spawn \
         a thread per database"
    );
}

/// AC3: a dropped database releases its maintenance registration -- no leak,
/// and no `shutdown_*` obligation on the caller.
#[test]
#[serial]
fn dropped_database_is_deregistered() {
    let baseline = adjacency_maintenance::registered_index_count();
    {
        let db = AletheiaDB::new().expect("create db");
        build_graph(&db, 10, 1);
        assert!(
            adjacency_maintenance::registered_index_count() >= baseline + 2,
            "a database registers its outgoing and incoming indexes"
        );
    }
    assert!(
        wait_until(CONVERGE, || adjacency_maintenance::registered_index_count()
            <= baseline),
        "dropped database left {} registrations behind (baseline {baseline})",
        adjacency_maintenance::registered_index_count()
    );
}

/// AC5: reads issued from other threads while the background worker compacts
/// never observe a missing or duplicated edge.
#[test]
#[serial]
fn concurrent_reads_during_maintenance_are_consistent() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let ids = build_graph(&db, 60, 4);
    let stop = Arc::new(AtomicBool::new(false));

    let readers: Vec<_> = (0..3)
        .map(|_| {
            let db = Arc::clone(&db);
            let stop = Arc::clone(&stop);
            let source = ids[0];
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let edges = db.get_outgoing_edges(source);
                    assert_eq!(
                        edges.len(),
                        4,
                        "a concurrent compaction exposed a torn adjacency list"
                    );
                    let mut sorted = edges.clone();
                    sorted.sort();
                    sorted.dedup();
                    assert_eq!(sorted.len(), edges.len(), "duplicate edge observed");
                }
            })
        })
        .collect();

    let compacted = wait_until(CONVERGE, || db.adjacency_stats().is_fully_compacted());
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().expect("reader thread panicked");
    }
    assert!(compacted, "never compacted: {:?}", db.adjacency_stats());
}

/// AC5 (regression): the torn read that starting compaction exposes.
///
/// Compaction moves edges from the delta buffer into the frozen CSR. Before
/// Issue #3810 it retired the delta entries *first* and published the rebuilt
/// CSR second, so a reader landing in between paired a pre-compaction CSR with
/// a post-retire delta and got an adjacency list **missing** those edges -- on
/// a freshly built graph, an empty one. Nothing shipped compaction concurrently
/// with reads, so the bug was dormant; background maintenance makes that
/// interleaving routine.
///
/// Hammering `compact_adjacency()` from one thread while another reads is a far
/// more reliable reproduction than waiting for the background worker.
#[test]
#[serial]
fn explicit_compaction_never_tears_a_concurrent_read() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let ids = build_graph(&db, 60, 4);
    let stop = Arc::new(AtomicBool::new(false));
    let torn = Arc::new(AtomicUsize::new(0));
    let observed_min = Arc::new(AtomicUsize::new(usize::MAX));

    let readers: Vec<_> = (0..2)
        .map(|_| {
            let db = Arc::clone(&db);
            let stop = Arc::clone(&stop);
            let torn = Arc::clone(&torn);
            let observed_min = Arc::clone(&observed_min);
            let source = ids[0];
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let edges = db.get_outgoing_edges(source);
                    let mut unique = edges.clone();
                    unique.sort();
                    unique.dedup();
                    if edges.len() != 4 || unique.len() != edges.len() {
                        torn.fetch_add(1, Ordering::Relaxed);
                        observed_min.fetch_min(edges.len(), Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for _ in 0..200 {
        db.compact_adjacency();
        // Re-dirty the delta so the next compaction has something to publish.
        db.create_edge(ids[1], ids[2], "KNOWS", PropertyMapBuilder::new().build())
            .expect("create_edge");
    }

    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().expect("reader panicked");
    }

    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "reads observed a torn adjacency list during compaction (smallest length seen: {})",
        observed_min.load(Ordering::Relaxed)
    );
}
