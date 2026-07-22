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
//! variance — it encodes a timing expectation the test author cannot control.
//! Instead this test asserts the *shape* of the outcome, which holds regardless
//! of how fast or slow the hardware is on a given run:
//!
//! - well-behaved reads ALL succeed and return the expected data (never
//!   starved, blocked, or corrupted by the concurrent pathological load) — the
//!   actual neighbor-protection property under test;
//! - the pathological stream produces a `ResourceExhausted` termination on
//!   every iteration, on each dimension it targets;
//! - the whole test finishes inside a generous wall-clock bound, so a
//!   regression that lets a pathological query run unbounded (the guard
//!   silently stops enforcing) fails by blowing the bound rather than hanging
//!   CI forever.
//!
//! # Why a high-fanout single hop, not a deep traversal
//!
//! The reliable, hardware-independent way to make a query "pathological" is to
//! make it *emit many rows*: a single hop out of a mega-hub node with several
//! thousand out-edges deterministically yields several thousand rows. The
//! result-row and memory dimensions then fire deterministically (they are
//! count/size based, not timing based), and the wall-clock dimension fires
//! reliably too — materializing thousands of rows in an unoptimized `cargo
//! test` debug build comfortably exceeds a 1 ms budget. (A deep
//! `Exact(depth)` traversal, by contrast, is *shortest-path node-distinct*, so
//! in a dense small-diameter graph almost no node has a shortest distance equal
//! to a large depth — it would yield ~0 rows and trip nothing.)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aletheiadb::core::error::{Error, QueryError};
use aletheiadb::{AletheiaDB, NodeId, PropertyMapBuilder};

/// Out-edges on the mega-hub node the pathological workers hammer. A single hop
/// yields this many rows — large enough that draining them in a debug build
/// exceeds the 1 ms wall-clock budget, and far above the 5-row cap.
const MEGA_FAN_OUT: usize = 2500;
/// Out-edges on the small hub the well-behaved workers read. Cheap and
/// unrelated to the mega hub, so its cost never depends on the pathological
/// load.
const CHEAP_FAN_OUT: usize = 5;

const PATHOLOGICAL_WORKERS: usize = 4;
const PATHOLOGICAL_ITERS_PER_WORKER: usize = 3;
const WELL_BEHAVED_WORKERS: usize = 4;
const WELL_BEHAVED_ITERS_PER_WORKER: usize = 50;

/// A tight per-call wall-clock budget. Draining `MEGA_FAN_OUT` rows in a debug
/// build reliably exceeds this, and the guard polls the deadline on every row
/// for the first several thousand rows, so the timeout fires promptly.
const TIGHT_TIMEOUT: Duration = Duration::from_millis(1);
/// A tiny per-call row cap; the mega hop emits far more.
const TIGHT_ROW_CAP: usize = 5;
/// A tiny per-call memory budget (bytes); the mega hop's rows exceed it almost
/// immediately.
const TIGHT_MEMORY_BUDGET: usize = 256;

/// The dimension each pathological worker targets, chosen by worker index so
/// all three enforced dimensions are exercised concurrently.
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

/// Build the shared database: a mega-hub `Person` node with `MEGA_FAN_OUT`
/// `KNOWS` out-edges for the pathological workers, plus a small hub with
/// `CHEAP_FAN_OUT` out-edges for the well-behaved workers. Returns
/// `(mega_hub, cheap_hub)`.
fn seed(db: &AletheiaDB) -> (NodeId, NodeId) {
    let mega_hub = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "MegaHub").build(),
        )
        .expect("create mega hub");
    for i in 0..MEGA_FAN_OUT {
        let leaf = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("idx", i as i64).build(),
            )
            .expect("create mega leaf");
        db.create_edge(mega_hub, leaf, "KNOWS", PropertyMapBuilder::new().build())
            .expect("create mega edge");
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

    (mega_hub, cheap_hub)
}

#[test]
fn pathological_queries_are_bounded_while_well_behaved_reads_keep_succeeding() {
    let overall_start = Instant::now();

    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let (mega_hub, cheap_hub) = seed(&db);

    let exhausted_terminations = Arc::new(AtomicUsize::new(0));
    let well_behaved_failures = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();

    // ---- Pathological workers ----
    //
    // Each hammers the mega hub with a single hop that would emit MEGA_FAN_OUT
    // rows, under a tight per-call limit on one of the three dimensions. Every
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
                let builder = db.query().start(mega_hub).traverse("KNOWS");
                let builder = match dimension {
                    Dimension::Timeout => builder.with_timeout(TIGHT_TIMEOUT),
                    Dimension::Rows => builder.with_max_rows(TIGHT_ROW_CAP),
                    Dimension::Memory => builder.with_memory_budget(TIGHT_MEMORY_BUDGET),
                };

                let results = builder
                    .execute(&db)
                    .expect("execute() must succeed lazily — the guard only fires on drain");

                let mut saw_exhausted = false;
                for row in results {
                    if let Err(e) = row {
                        assert_eq!(
                            resource_exhausted_dimension(&e),
                            Some(dimension.token()),
                            "pathological worker {worker_idx} got an unexpected error: {e:?}"
                        );
                        saw_exhausted = true;
                        exhausted_terminations.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
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
    // shared lock, starved executor) would show up here as failures or wrong
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

    // (a) every well-behaved read succeeded with the correct result.
    assert_eq!(
        well_behaved_failures.load(Ordering::Relaxed),
        0,
        "well-behaved reads must never fail or return wrong data while the pathological \
         stream runs concurrently"
    );

    // (b) every pathological iteration terminated via ResourceExhausted, and
    // the shared engine-lane counters observed the terminations across all
    // three dimensions.
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

    // (c) the whole test completes well inside a generous bound. Not a latency
    // assertion — a guard regression that lets the pathological hop run
    // unbounded would blow this bound and fail the test (rather than hang CI).
    let elapsed = overall_start.elapsed();
    assert!(
        elapsed < Duration::from_secs(30),
        "soak test took {elapsed:?}, exceeding the 30s regression bound — a pathological \
         query may be running unbounded instead of being cut off by its limit"
    );
}
