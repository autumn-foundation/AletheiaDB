//! Deterministic two-phase crash-recovery workload for instruction-count
//! profiling of `AletheiaDB::open()`'s cold-start path (checkpoint load +
//! WAL replay), driven entirely through the public `AletheiaDB` API — the
//! "Recovery Flow: Startup → Load Checkpoint → Replay WAL → Restore Indexes
//! → Ready" documented in `CLAUDE.md`, with an explicit `<5s for 10K
//! nodes/50K edges` target for the "medium" scenario.
//!
//! This is two runs of one plain binary, not a criterion benchmark or a
//! single self-contained process — see `examples/bolt_workload.rs` for why
//! criterion's iteration loop doesn't fit under valgrind, and this harness
//! has an additional reason to split into two OS processes: `AletheiaDB`'s
//! `Drop` synchronously performs a final full index checkpoint before
//! returning (`src/db/mod.rs`, `src/storage/index_persistence/worker.rs`),
//! so a single-process write→drop→reopen cycle (as `tests/open_durable.rs`
//! exercises for correctness) always reopens against an up-to-date
//! checkpoint with an empty WAL tail — replay does near-zero work. To
//! produce a WAL tail that reopening must actually replay, `setup` mode
//! checkpoints a base dataset via `persist_indexes()`, writes *more* data
//! after that checkpoint, then calls `std::process::exit(0)` — which skips
//! `Drop` entirely, simulating a crash immediately after the last
//! acknowledged write rather than a clean shutdown. `recover` mode then
//! measures a fresh `AletheiaDB::open()` against that on-disk state in a
//! separate process invocation, so only the checkpoint-load + WAL-replay
//! cost is in the profiled process.
//!
//! ```text
//! cargo build --profile bench --example bolt_recovery_workload
//!
//! rm -rf /tmp/bolt_recovery_workload_data
//! BOLT_MODE=setup BOLT_CKPT_NODES=3000 BOLT_TAIL_NODES=800 BOLT_OUT_DEGREE=4 \
//!     target/release/examples/bolt_recovery_workload
//!
//! BOLT_MODE=recover \
//!     valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \
//!     target/release/examples/bolt_recovery_workload
//! callgrind_annotate /tmp/cg.out | less
//! ```
//!
//! Env vars: `BOLT_MODE` (required: `setup` or `recover`), `BOLT_DATA_DIR`
//! (default: a fixed path under the OS temp dir, so `setup` and `recover`
//! agree without passing it explicitly), `BOLT_CKPT_NODES` (default 3000,
//! `setup` only), `BOLT_TAIL_NODES` (default 800, `setup` only — together
//! with its `BOLT_OUT_DEGREE` edges this must stay under the default
//! 5000-mutation `GraphPersistencePolicy` auto-checkpoint threshold, or the
//! background persistence thread's periodic poll can checkpoint the tail out
//! from under this harness before `setup` exits, leaving nothing for
//! `recover` to replay), `BOLT_OUT_DEGREE` (default 4), `BOLT_SETUP_THREADS`
//! (default 32, `setup` only — a lone sequential caller pays GroupCommit's
//! full ~10ms batch-window latency per write; concurrent callers fill each
//! batch instead, see `make_nodes_parallel`'s doc comment).

use aletheiadb::prelude::*;
use std::path::PathBuf;
use std::time::Instant;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn data_dir() -> PathBuf {
    std::env::var("BOLT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("bolt_recovery_workload_data"))
}

/// `setup` writes through `AletheiaDB::open()`'s durable, `GroupCommit`
/// (`max_delay_ms: 10`) WAL — a single sequential caller pays the full
/// ~10ms batch-window latency per commit (~100 ops/sec), so this fans writes
/// out across `threads` concurrent callers to actually fill each commit
/// batch, matching how GroupCommit reaches its documented ~100K+/sec
/// throughput. Only `setup`'s wall time benefits; `recover`'s single-
/// threaded WAL replay (the thing under profile) is unaffected by this.
fn make_nodes_parallel(db: &AletheiaDB, count: usize, prefix: &str, threads: usize) -> Vec<NodeId> {
    let chunk = count.div_ceil(threads.max(1));
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let start = (t * chunk).min(count);
                let end = (start + chunk).min(count);
                scope.spawn(move || -> Vec<NodeId> {
                    (start..end)
                        .map(|i| {
                            let props = PropertyMapBuilder::new()
                                .insert("name", format!("{prefix}{i}"))
                                .insert("age", (i % 80) as i64)
                                .build();
                            db.create_node("Person", props).expect("create_node")
                        })
                        .collect()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    })
}

fn make_edges_parallel(
    db: &AletheiaDB,
    sources: &[NodeId],
    all_targets: &[NodeId],
    out_degree: usize,
    threads: usize,
) {
    let chunk = sources.len().div_ceil(threads.max(1));
    std::thread::scope(|scope| {
        for t in 0..threads {
            let start = (t * chunk).min(sources.len());
            let end = (start + chunk).min(sources.len());
            if start == end {
                continue;
            }
            scope.spawn(move || {
                for (i, &source) in sources[start..end].iter().enumerate() {
                    let i = start + i;
                    for j in 0..out_degree {
                        let target = all_targets[(i + j + 1) % all_targets.len()];
                        let props = PropertyMapBuilder::new()
                            .insert("weight", (i + j) as i64)
                            .build();
                        db.create_edge(source, target, "KNOWS", props)
                            .expect("create_edge");
                    }
                }
            });
        }
    });
}

fn run_setup() {
    let dir = data_dir();
    let ckpt_nodes = env_usize("BOLT_CKPT_NODES", 3_000);
    let tail_nodes = env_usize("BOLT_TAIL_NODES", 800);
    let out_degree = env_usize("BOLT_OUT_DEGREE", 4);
    let threads = env_usize("BOLT_SETUP_THREADS", 32);

    let _ = std::fs::remove_dir_all(&dir);

    let db = AletheiaDB::open(&dir).expect("open durable db");

    // --- Checkpoint baseline ---
    let base_ids = make_nodes_parallel(&db, ckpt_nodes, "Base", threads);
    make_edges_parallel(&db, &base_ids, &base_ids, out_degree, threads);
    db.persist_indexes()
        .expect("persist_indexes (checkpoint baseline)");

    // --- Post-checkpoint WAL tail: this is what `recover` mode must replay ---
    let tail_ids = make_nodes_parallel(&db, tail_nodes, "Tail", threads);
    let mut all_ids = base_ids;
    all_ids.extend(tail_ids.iter().copied());
    make_edges_parallel(&db, &tail_ids, &all_ids, out_degree, threads);

    eprintln!(
        "setup complete: checkpoint_nodes={ckpt_nodes} tail_nodes={tail_nodes} out_degree={out_degree} dir={}",
        dir.display()
    );

    // Deliberately skip `Drop` (which would perform a final checkpoint,
    // erasing the WAL tail above) to simulate a crash right after the last
    // acknowledged (already fsynced under GroupCommit) write.
    std::process::exit(0);
}

fn run_recover() {
    let dir = data_dir();

    let t0 = Instant::now();
    let db = AletheiaDB::open(&dir).expect("reopen: load checkpoint + replay WAL tail");
    let elapsed = t0.elapsed();

    // A handful of reads through the public API, both to sanity-check that
    // replay actually reconstructed the tail and to keep the profiled
    // process from being pure dead code after `open()` returns.
    let mut sink: u64 = db.node_count() as u64;
    for i in 0..50 {
        if let Ok(node_id) = NodeId::new(i as u64) {
            if let Ok(node) = db.get_node(node_id) {
                sink = sink.wrapping_add(node.properties.len() as u64);
            }
            sink = sink.wrapping_add(db.get_outgoing_edges(node_id).len() as u64);
        }
    }

    eprintln!("recovery wall time: {elapsed:?}");
    println!("sink={sink} node_count={}", db.node_count());

    // Exclude the closing `Drop` (final checkpoint write) from the profiled
    // window; recovery is what this harness measures.
    std::process::exit(0);
}

fn main() {
    match std::env::var("BOLT_MODE").as_deref() {
        Ok("setup") => run_setup(),
        Ok("recover") => run_recover(),
        other => panic!(
            "BOLT_MODE must be set to \"setup\" or \"recover\" (got {other:?}); see this file's \
             module doc comment for the two-phase reproduction steps"
        ),
    }
}
