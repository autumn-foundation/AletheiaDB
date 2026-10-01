//! Read-path cost of the replica apply gate (Issue #3788).
//!
//! The gate makes a replica's batch apply atomically visible to concurrent
//! current-state reads. It is *disarmed* on a primary (one predictable branch)
//! and *armed* on a replica, where reads run a seqlock: two loads of a
//! read-mostly sequence word, no atomic RMW, so reader fan-out still scales.
//!
//! These benchmarks quantify both sides of that claim:
//!
//! * `primary/*` — the gate must be invisible on a primary. Compare against
//!   `current_state`'s node-lookup / single-hop numbers; the target
//!   (<1µs single-hop, <100ns node lookup) is unchanged.
//! * `replica_idle/*` — the steady-state cost on a replica with the gate armed
//!   but no apply in flight. This is the number that matters for read
//!   scale-out, since a replica spends the overwhelming majority of its time
//!   between batches.
//! * `replica_idle_concurrent/*` — the same read from several threads at once,
//!   which is where a naive `RwLock` implementation would show its cache-line
//!   ping-pong and this one should not.
//!
//! Run with: `cargo bench --bench replication_apply_gate`

use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use aletheiadb::core::error::Result;
use aletheiadb::core::id::NodeId;
use aletheiadb::{
    AletheiaDB, FetchOutcome, PropertyMapBuilder, ReplicationOptions, ReplicationSource,
};
use criterion::{Criterion, criterion_group, criterion_main};

const NODES: usize = 10_000;
const OUT_DEGREE: usize = 4;

/// A source that never has anything to send: the replica arms its gate and then
/// sits idle, isolating the gate's steady-state read cost from apply work.
struct IdleSource;

impl ReplicationSource for IdleSource {
    fn fetch_entries(&mut self, _from_lsn: u64, _max_entries: usize) -> Result<FetchOutcome> {
        Ok(FetchOutcome::Entries {
            entries: Vec::new(),
            primary_flushed_lsn: 0,
            primary_wallclock_micros: 0,
        })
    }

    fn fetch_snapshot(&mut self, _dest: &Path) -> Result<u64> {
        unimplemented!("the idle source is never bootstrapped from")
    }
}

/// A populated in-memory database plus the ids to read back.
fn build_graph() -> (AletheiaDB, Vec<NodeId>) {
    let db = AletheiaDB::new().expect("create db");

    let ids: Vec<NodeId> = (0..NODES)
        .map(|i| {
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("id", i as i64).build(),
            )
            .expect("create node")
        })
        .collect();

    for i in 0..NODES {
        for j in 0..OUT_DEGREE {
            let target = (i + j + 1) % NODES;
            db.create_edge(
                ids[i],
                ids[target],
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .expect("create edge");
        }
    }

    (db, ids)
}

/// The same graph, but with replication started so the apply gate is armed.
/// The idle source means no batch ever publishes.
fn build_replica_graph() -> (AletheiaDB, Vec<NodeId>) {
    let (db, ids) = build_graph();
    db.start_replication(
        Box::new(IdleSource),
        ReplicationOptions {
            // Long poll: the applier thread stays out of the way.
            poll_interval: Duration::from_secs(3600),
            ..ReplicationOptions::default()
        },
    )
    .expect("start replication");
    (db, ids)
}

fn bench_single_threaded(c: &mut Criterion, prefix: &str, db: &AletheiaDB, ids: &[NodeId]) {
    let mut group = c.benchmark_group(prefix);

    group.bench_function("get_node", |b| {
        let mut i = 0usize;
        b.iter(|| {
            i = (i + 1) % ids.len();
            black_box(db.get_node(black_box(ids[i])).expect("node"))
        });
    });

    group.bench_function("single_hop", |b| {
        let mut i = 0usize;
        b.iter(|| {
            i = (i + 1) % ids.len();
            black_box(db.get_outgoing_edges(black_box(ids[i])))
        });
    });

    group.finish();
}

fn bench_concurrent(c: &mut Criterion, prefix: &str, db: Arc<AletheiaDB>, ids: Arc<Vec<NodeId>>) {
    let mut group = c.benchmark_group(prefix);

    for threads in [2usize, 4, 8] {
        group.bench_function(format!("get_node/{threads}_threads"), |b| {
            b.iter_custom(|iters| {
                let per_thread = iters.div_ceil(threads as u64);
                let barrier = Arc::new(Barrier::new(threads + 1));
                let stop = Arc::new(AtomicBool::new(false));

                let handles: Vec<_> = (0..threads)
                    .map(|t| {
                        let (db, ids, barrier, stop) = (
                            Arc::clone(&db),
                            Arc::clone(&ids),
                            Arc::clone(&barrier),
                            Arc::clone(&stop),
                        );
                        thread::spawn(move || {
                            barrier.wait();
                            let mut i = t * 97;
                            for _ in 0..per_thread {
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                i = (i + 1) % ids.len();
                                black_box(db.get_node(ids[i]).expect("node"));
                            }
                        })
                    })
                    .collect();

                barrier.wait();
                let start = std::time::Instant::now();
                for h in handles {
                    h.join().expect("reader thread");
                }
                start.elapsed()
            });
        });
    }

    group.finish();
}

fn apply_gate_benchmarks(c: &mut Criterion) {
    let (primary, primary_ids) = build_graph();
    bench_single_threaded(c, "apply_gate/primary", &primary, &primary_ids);

    let (replica, replica_ids) = build_replica_graph();
    bench_single_threaded(c, "apply_gate/replica_idle", &replica, &replica_ids);

    let primary = Arc::new(primary);
    let primary_ids = Arc::new(primary_ids);
    bench_concurrent(
        c,
        "apply_gate/primary_concurrent",
        Arc::clone(&primary),
        Arc::clone(&primary_ids),
    );

    let replica = Arc::new(replica);
    let replica_ids = Arc::new(replica_ids);
    bench_concurrent(
        c,
        "apply_gate/replica_idle_concurrent",
        Arc::clone(&replica),
        Arc::clone(&replica_ids),
    );
}

criterion_group!(benches, apply_gate_benchmarks);
criterion_main!(benches);
