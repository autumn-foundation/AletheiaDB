//! Concurrency soak test for per-query resource limits (Issue #3368, AC8 —
//! "neighbor protection"): a pathological query stream is reliably terminated
//! at its configured limits while a well-behaved query stream, sharing the same
//! database concurrently, keeps succeeding.
//!
//! # Why the assertions are robustness-oriented, not percentile-based
//!
//! CI runners have wildly variable, non-deterministic CPU scheduling (shared
//! cores, noisy neighbors, thermal throttling). A test that asserts "the
//! well-behaved read completes in under N milliseconds" or "the pathological
//! query is cut off within X% of its deadline" is inherently flaky under that
//! variance. Instead this test asserts the *shape* of the outcome, which holds
//! regardless of hardware speed:
//!
//! - well-behaved reads ALL succeed and return the expected data (never
//!   starved, blocked, or corrupted by the concurrent pathological load) — the
//!   actual neighbor-protection property under test;
//! - **every** pathological iteration terminates with `ResourceExhausted` on the
//!   dimension it targets. This is the primary, fully deterministic
//!   guard-works signal: a broken guard would let the query drain to completion,
//!   flipping this assertion — regardless of graph size or timing.
//!
//! # The wall-clock bound covers only the concurrent worker phase, never seeding
//!
//! The bound in assertion (c) times **only** the concurrent worker phase, with
//! the (single-threaded, WAL-durable) seed excluded — an earlier version timed
//! the whole test and flaked on a slow CI runner where seeding several thousand
//! GroupCommit writes alone dominated the budget. The worker-phase bound is a
//! generous anti-hang net (a truly stuck guard blows it), not a latency
//! assertion; correctness is carried by assertion (b), which is deterministic.
//!
//! # Why a high-fanout single hop, not a deep traversal
//!
//! The reliable, hardware-independent way to make a query "pathological" is to
//! make it *emit many rows*: a single hop out of a hub node with many out-edges
//! deterministically yields many rows, so the result-row and memory dimensions
//! (count/size based, not timing based) fire deterministically. The wall-clock
//! dimension is made deterministic a different way — see the timeout worker.
//! (A deep `Exact(depth)` traversal, by contrast, is shortest-path
//! node-distinct, so in a dense small-diameter graph almost no node has a
//! shortest distance equal to a large depth — it would yield ~0 rows and trip
//! nothing.)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aletheiadb::core::error::{Error, QueryError};
use aletheiadb::{AletheiaDB, NodeId, PropertyMapBuilder};

/// Out-edges on the hub node the pathological workers hammer. A single hop
/// yields this many rows — far above the 5-row cap and the tiny memory budget,
/// so those two dimensions fire deterministically. Kept modest so seeding stays
/// fast on a contended CI runner (seeding, not the guarded queries, dominates
/// this test's wall-clock cost).
const HUB_FAN_OUT: usize = 200;
/// Out-edges on the small hub the well-behaved workers read. Cheap and
/// unrelated to the pathological hub, so its cost never depends on the
/// pathological load.
const CHEAP_FAN_OUT: usize = 5;

const PATHOLOGICAL_WORKERS: usize = 4;
const PATHOLOGICAL_ITERS_PER_WORKER: usize = 3;
const WELL_BEHAVED_WORKERS: usize = 4;
const WELL_BEHAVED_ITERS_PER_WORKER: usize = 50;

/// A tight per-call wall-clock budget for the timeout worker.
const TIGHT_TIMEOUT: Duration = Duration::from_millis(1);
/// A tiny per-call row cap; the hub hop emits far more.
const TIGHT_ROW_CAP: usize = 5;
/// A tiny per-call memory budget (bytes); the hub hop's rows exceed it almost
/// immediately.
const TIGHT_MEMORY_BUDGET: usize = 256;
/// Generous anti-hang bound on the concurrent worker phase (seeding excluded).
/// With a working guard the phase completes in well under a second; this only
/// fails if a query hangs unbounded.
const WORKER_PHASE_BOUND: Duration = Duration::from_secs(60);

/// The dimension each pathological worker targets, chosen by worker index so all
/// three enforced dimensions are exercised concurrently.
#[derive(Clone, Copy)]
enum Dimension {
    Timeout,
    Rows,
    Memory,
}

impl Dimension {
    fn token(self) -> &'static str {
        match self {
            Dimension::Timeout => "wall_clock_timeout",
            Dimension::Rows => "result_rows",
            Dimension::Memory => "memory_bytes",
        }
    }
}

/// Extract the `ResourceExhausted` dimension token from an error, if it is one.
fn resource_exhausted_dimension(err: &Error) -> Option<&'static str> {
    match err {
        Error::Query(QueryError::ResourceExhausted { dimension, .. }) => Some(dimension),
        _ => None,
    }
}

/// Build the shared database: a hub `Person` node with `HUB_FAN_OUT` `KNOWS`
/// out-edges for the pathological workers, plus a small hub with `CHEAP_FAN_OUT`
/// out-edges for the well-behaved workers. Returns `(hub, cheap_hub)`.
fn seed(db: &AletheiaDB) -> (NodeId, NodeId) {
    let hub = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Hub").build(),
        )
        .expect("create hub");
    for i in 0..HUB_FAN_OUT {
        let leaf = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("idx", i as i64).build(),
            )
            .expect("create hub leaf");
        db.create_edge(hub, leaf, "KNOWS", PropertyMapBuilder::new().build())
            .expect("create hub edge");
    }

    let cheap_hub = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "CheapHub").build(),
        )
        .expect("create cheap hub");
    for i in 0..CHEAP_FAN_OUT {
        let leaf = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("CheapLeaf{i}"))
                    .build(),
            )
            .expect("create cheap leaf");
        db.create_edge(cheap_hub, leaf, "KNOWS", PropertyMapBuilder::new().build())
            .expect("create cheap edge");
    }

    (hub, cheap_hub)
}

#[test]
fn pathological_queries_are_bounded_while_well_behaved_reads_keep_succeeding() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let (hub, cheap_hub) = seed(&db);

    let exhausted_terminations = Arc::new(AtomicUsize::new(0));
    let well_behaved_failures = Arc::new(AtomicUsize::new(0));

    // Time only the concurrent worker phase — seeding above is single-threaded,
    // WAL-durable, and environment-dominated, so it must not count against the
    // guard's regression bound.
    let worker_phase_start = Instant::now();

    let mut handles = Vec::new();

    // ---- Pathological workers ----
    //
    // Each hammers the hub with a single hop that would emit HUB_FAN_OUT rows,
    // under a tight per-call limit on one of the three dimensions. Every
    // iteration is expected to terminate with `ResourceExhausted` on that
    // dimension — never succeed, never hang.
    for worker_idx in 0..PATHOLOGICAL_WORKERS {
        let db = Arc::clone(&db);
        let exhausted_terminations = Arc::clone(&exhausted_terminations);
        let dimension = match worker_idx % 3 {
            0 => Dimension::Timeout,
            1 => Dimension::Rows,
            _ => Dimension::Memory,
        };
        handles.push(std::thread::spawn(move || {
            for _ in 0..PATHOLOGICAL_ITERS_PER_WORKER {
                let saw_exhausted = run_pathological_once(&db, hub, dimension, worker_idx);
                if saw_exhausted {
                    exhausted_terminations.fetch_add(1, Ordering::Relaxed);
                }
                assert!(
                    saw_exhausted,
                    "pathological worker {worker_idx} (dimension={}) drained fully without \
                     hitting its limit — the guard did not fire",
                    dimension.token()
                );
            }
        }));
    }

    // ---- Well-behaved workers ----
    //
    // Cheap single-hop reads on an isolated hub, unaffected by any limit
    // (defaults are generous). Every one must succeed with the correct result
    // set — the actual neighbor-protection signal: a broken guard (poisoned
    // shared lock, starved executor) would surface here as failures or wrong
    // data, not merely slowness.
    for worker_idx in 0..WELL_BEHAVED_WORKERS {
        let db = Arc::clone(&db);
        let well_behaved_failures = Arc::clone(&well_behaved_failures);
        handles.push(std::thread::spawn(move || {
            for iter in 0..WELL_BEHAVED_ITERS_PER_WORKER {
                let outcome = db
                    .query()
                    .start(cheap_hub)
                    .traverse("KNOWS")
                    .execute(&db)
                    .and_then(|results| results.collect_all());
                match outcome {
                    Ok(rows) if rows.len() == CHEAP_FAN_OUT => {}
                    Ok(rows) => {
                        well_behaved_failures.fetch_add(1, Ordering::Relaxed);
                        eprintln!(
                            "well-behaved worker {worker_idx} iter {iter}: expected \
                             {CHEAP_FAN_OUT} rows, got {}",
                            rows.len()
                        );
                    }
                    Err(e) => {
                        well_behaved_failures.fetch_add(1, Ordering::Relaxed);
                        eprintln!("well-behaved worker {worker_idx} iter {iter}: {e:?}");
                    }
                }
            }
        }));
    }

    for handle in handles {
        handle.join().expect("worker thread panicked");
    }
    let worker_phase = worker_phase_start.elapsed();

    // (a) every well-behaved read succeeded with the correct result.
    assert_eq!(
        well_behaved_failures.load(Ordering::Relaxed),
        0,
        "well-behaved reads must never fail or return wrong data while the pathological \
         stream runs concurrently"
    );

    // (b) every pathological iteration terminated via ResourceExhausted, and the
    // shared engine-lane counters observed terminations on all three dimensions.
    assert_eq!(
        exhausted_terminations.load(Ordering::Relaxed),
        PATHOLOGICAL_WORKERS * PATHOLOGICAL_ITERS_PER_WORKER,
        "every pathological iteration must terminate via ResourceExhausted"
    );
    let counters = db.query_limit_counters();
    assert!(
        counters.wall_clock_timeout > 0,
        "the wall-clock-timeout dimension must have fired: {counters:?}"
    );
    assert!(
        counters.result_rows > 0,
        "the result-rows dimension must have fired: {counters:?}"
    );
    assert!(
        counters.memory_bytes > 0,
        "the memory-bytes dimension must have fired: {counters:?}"
    );

    // (c) the concurrent worker phase (seeding excluded) completes well inside a
    // generous anti-hang bound. A guard regression that let the pathological hop
    // run unbounded would blow this instead of hanging CI.
    assert!(
        worker_phase < WORKER_PHASE_BOUND,
        "worker phase took {worker_phase:?}, exceeding the {WORKER_PHASE_BOUND:?} anti-hang \
         bound — a pathological query may be running unbounded instead of being cut off"
    );
}

/// Run one pathological query and report whether it terminated with the
/// expected `ResourceExhausted` dimension.
///
/// Row and memory caps fire deterministically from the hub's fan-out. The
/// wall-clock case is made deterministic without depending on machine speed:
/// `with_timeout` fixes the deadline at `execute()` time and the stream is lazy,
/// so sleeping past the (1 ms) deadline before draining makes the guard's
/// pre-pull check at row 0 fire on any hardware.
fn run_pathological_once(
    db: &AletheiaDB,
    hub: NodeId,
    dimension: Dimension,
    worker_idx: usize,
) -> bool {
    let builder = db.query().start(hub).traverse("KNOWS");
    let results = match dimension {
        Dimension::Timeout => builder.with_timeout(TIGHT_TIMEOUT),
        Dimension::Rows => builder.with_max_rows(TIGHT_ROW_CAP),
        Dimension::Memory => builder.with_memory_budget(TIGHT_MEMORY_BUDGET),
    }
    .execute(db)
    .expect("execute() must succeed lazily — the guard only fires on drain");

    if matches!(dimension, Dimension::Timeout) {
        // Elapse the lazily-fixed deadline before draining a single row.
        std::thread::sleep(Duration::from_millis(15));
    }

    for row in results {
        if let Err(e) = row {
            assert_eq!(
                resource_exhausted_dimension(&e),
                Some(dimension.token()),
                "pathological worker {worker_idx} got an unexpected error: {e:?}"
            );
            return true;
        }
    }
    false
}
