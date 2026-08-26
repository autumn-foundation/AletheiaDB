//! Measures how AletheiaDB's read and write paths scale with concurrent threads.
//!
//! Run with:
//! ```text
//! cargo run --release --example concurrency_scaling
//! ```
//!
//! Four workloads are swept across thread counts:
//!
//! - `write/group_commit` — full transaction commits under `GroupCommit`
//! - `write/async`        — full transaction commits under `Async`
//! - `read/snapshot`      — `read_transaction()` + `get_node` (takes a snapshot timestamp)
//! - `read/current`       — `db.get_node()` (current-state fast path, no snapshot)
//!
//! The interesting number is the scaling factor: throughput at N threads divided
//! by throughput at 1 thread. A path that serializes on a global lock stays flat
//! (or degrades) as N rises; a path that genuinely parallelizes climbs with N.

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::NodeId;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use aletheiadb::api::{ReadOps, WriteOps};

const THREAD_COUNTS: &[usize] = &[1, 2, 4, 8];
/// GroupCommit runs ~10ms/commit, so 100 ops/thread is already ~1s at 1 thread.
const GROUP_COMMIT_OPS_PER_THREAD: usize = 100;
/// Async commits are ~5µs, so this needs to be large enough that thread
/// spawn/join overhead does not dominate the measured window.
const ASYNC_OPS_PER_THREAD: usize = 100_000;
/// Reads are ~20-60ns, so the op count has to be large or we are timing
/// `thread::join`, not the database.
const READ_OPS_PER_THREAD: usize = 2_000_000;
const SEED_NODES: usize = 1_000;

fn make_db(mode: DurabilityMode) -> (tempfile::TempDir, Arc<AletheiaDB>) {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let config = WalConfigBuilder::new()
        .wal_dir(temp_dir.path().to_path_buf())
        .segment_size(64 * 1024 * 1024)
        .expect("segment size")
        .segments_to_retain(3)
        .expect("segments to retain")
        .durability_mode(mode)
        .build();
    let db = AletheiaDB::with_wal_config(config).expect("db");
    (temp_dir, Arc::new(db))
}

/// Run `body` on `threads` threads and return aggregate ops/sec.
///
/// A `Barrier` (which parks rather than spins) releases all workers at once, so
/// on an oversubscribed box the synchronization does not steal cores from the
/// work being measured. The timer starts after the barrier and stops after
/// every worker has finished its loop.
fn measure<F>(threads: usize, ops_per_thread: usize, body: F) -> f64
where
    F: Fn(usize, usize) + Send + Sync + 'static,
{
    let body = Arc::new(body);
    let start_barrier = Arc::new(Barrier::new(threads + 1));
    let done_barrier = Arc::new(Barrier::new(threads + 1));

    let mut handles = Vec::with_capacity(threads);
    for t in 0..threads {
        let body = Arc::clone(&body);
        let start_barrier = Arc::clone(&start_barrier);
        let done_barrier = Arc::clone(&done_barrier);
        handles.push(thread::spawn(move || {
            start_barrier.wait();
            for i in 0..ops_per_thread {
                body(t, i);
            }
            done_barrier.wait();
        }));
    }

    start_barrier.wait();
    let start = Instant::now();
    done_barrier.wait();
    let elapsed = start.elapsed().as_secs_f64();

    for h in handles {
        h.join().expect("thread");
    }
    (threads * ops_per_thread) as f64 / elapsed
}

fn report(label: &str, results: &[(usize, f64)]) {
    let base = results.first().map(|(_, v)| *v).unwrap_or(1.0);
    println!("\n{label}");
    println!(
        "  {:>7}  {:>14}  {:>10}  {:>12}",
        "threads", "ops/sec", "scaling", "µs/op"
    );
    for (threads, ops) in results {
        println!(
            "  {:>7}  {:>14.0}  {:>9.2}x  {:>12.2}",
            threads,
            ops,
            ops / base,
            1_000_000.0 * (*threads as f64) / ops
        );
    }
}

fn bench_writes(label: &str, mode: DurabilityMode, ops_per_thread: usize) {
    let mut results = Vec::new();
    for &threads in THREAD_COUNTS {
        let (_guard, db) = make_db(mode);
        let db_for_body = Arc::clone(&db);
        let ops = measure(threads, ops_per_thread, move |t, i| {
            db_for_body
                .write(|tx| {
                    tx.create_node(
                        "Scaling",
                        PropertyMapBuilder::new()
                            .insert("thread", t as i64)
                            .insert("i", i as i64)
                            .build(),
                    )
                })
                .expect("commit");
        });
        results.push((threads, ops));
        drop(db);
    }
    report(label, &results);
}

fn bench_reads() {
    let (_guard, db) = make_db(DurabilityMode::Async {
        flush_interval_ms: 100,
    });
    let mut ids = Vec::with_capacity(SEED_NODES);
    for i in 0..SEED_NODES {
        let id = db
            .create_node(
                "Seed",
                PropertyMapBuilder::new().insert("i", i as i64).build(),
            )
            .expect("seed");
        ids.push(id);
    }
    let ids = Arc::new(ids);

    let mut snapshot_results = Vec::new();
    for &threads in THREAD_COUNTS {
        let db = Arc::clone(&db);
        let ids = Arc::clone(&ids);
        let ops = measure(threads, READ_OPS_PER_THREAD, move |t, i| {
            let id: NodeId = ids[(t * 7919 + i) % ids.len()];
            let tx = db.read_transaction().expect("read tx");
            let _ = tx.get_node(id).expect("get");
        });
        snapshot_results.push((threads, ops));
    }
    report(
        "read/snapshot  (read_transaction + get_node)",
        &snapshot_results,
    );

    let mut current_results = Vec::new();
    for &threads in THREAD_COUNTS {
        let db = Arc::clone(&db);
        let ids = Arc::clone(&ids);
        let ops = measure(threads, READ_OPS_PER_THREAD, move |t, i| {
            let id: NodeId = ids[(t * 7919 + i) % ids.len()];
            let _ = db.get_node(id).expect("get");
        });
        current_results.push((threads, ops));
    }
    report(
        "read/current   (db.get_node -> clones Node + bumps PropertyMap Arc)",
        &current_results,
    );

    // Same access pattern, but zero-copy: no Node clone, no PropertyMap Arc
    // increment. Isolates "shared atomic refcount" from "shared lock".
    let mut borrow_results = Vec::new();
    for &threads in THREAD_COUNTS {
        let db = Arc::clone(&db);
        let ids = Arc::clone(&ids);
        let ops = measure(threads, READ_OPS_PER_THREAD, move |t, i| {
            let id: NodeId = ids[(t * 7919 + i) % ids.len()];
            let _ = db.with_node(id, |n| n.id).expect("with_node");
        });
        borrow_results.push((threads, ops));
    }
    report(
        "read/borrow    (db.with_node, zero-copy, shared hot set)",
        &borrow_results,
    );

    // Zero-copy AND disjoint per-thread id ranges: no shared cache lines at all.
    // This is the ceiling the read path can reach.
    let mut disjoint_results = Vec::new();
    for &threads in THREAD_COUNTS {
        let db = Arc::clone(&db);
        let ids = Arc::clone(&ids);
        let ops = measure(threads, READ_OPS_PER_THREAD, move |t, i| {
            let stride = ids.len() / 16;
            let id: NodeId = ids[(t * stride + i) % ids.len()];
            let _ = db.with_node(id, |n| n.id).expect("with_node");
        });
        disjoint_results.push((threads, ops));
    }
    report(
        "read/disjoint  (db.with_node, zero-copy, disjoint id ranges)",
        &disjoint_results,
    );
}

fn main() {
    println!(
        "AletheiaDB concurrency scaling — {} cores available",
        thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );

    bench_writes(
        "write/group_commit  (GroupCommit { max_delay_ms: 10, max_batch_size: 200 })",
        DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 200,
        },
        GROUP_COMMIT_OPS_PER_THREAD,
    );
    bench_writes(
        "write/async         (Async { flush_interval_ms: 1 })",
        DurabilityMode::Async {
            flush_interval_ms: 1,
        },
        ASYNC_OPS_PER_THREAD,
    );
    bench_reads();
}
