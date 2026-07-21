//! Workflow-journal micro-benchmarks (DBOS Phase 3a; design sketch
//! `docs/plans/2026-07-19-dbos-design-sketch.md`, Issue #3577).
//!
//! This is the **§7 OQ9 baseline harness** for the exactly-once step-recording
//! primitive: it measures `record_step` throughput and documents the journal's
//! per-step **write amplification** — each recorded step commits exactly
//! **1 `Step` node + 1 `HAS_STEP` edge** in one atomic write transaction.
//!
//! Because §7's OQ9 leaves the concrete throughput/latency target set **open**,
//! this harness intentionally asserts no gate — the numbers it produces are the
//! baseline against which future targets will be set (**targets TBD**).
//!
//! The whole module is gated behind the `durable-execution` feature (see the
//! `[[bench]]` entry in `Cargo.toml`), so it never affects a default build.

#[cfg(feature = "durable-execution")]
use aletheiadb::{AletheiaDB, CreateRunSpec, StepRecordSpec, WorkflowJournalExt, WorkflowRun};
#[cfg(feature = "durable-execution")]
use criterion::{Criterion, criterion_group, criterion_main};
#[cfg(feature = "durable-execution")]
use std::hint::black_box;

/// Build a fresh, bootstrapped in-memory database and a single run to record
/// steps against.
#[cfg(feature = "durable-execution")]
fn fresh_run(workflow_id: &str) -> (AletheiaDB, WorkflowRun) {
    let db = AletheiaDB::new().expect("in-memory db");
    let journal = db.workflow_journal();
    journal.bootstrap().expect("bootstrap constraints");
    let run = journal
        .create_run(CreateRunSpec::new(workflow_id, "bench-workflow"))
        .expect("create run");
    (db, run)
}

/// Throughput of `record_step`: each iteration commits one `Step` node + one
/// `HAS_STEP` edge (write amplification = 1 node + 1 edge per step). A monotonic
/// step number keeps every write a distinct, non-colliding `idem_key`.
#[cfg(feature = "durable-execution")]
fn bench_record_step(c: &mut Criterion) {
    let (db, run) = fresh_run("wf-bench-record");
    let journal = db.workflow_journal();
    let mut step_number: i64 = 0;

    c.bench_function("workflow_journal/record_step", |b| {
        b.iter(|| {
            step_number += 1;
            let outcome = journal
                .record_step(
                    &run,
                    StepRecordSpec::completed(step_number, "charge", b"receipt".to_vec()),
                )
                .expect("record step");
            black_box(outcome);
        });
    });
}

/// Cost of the adopt-winner (idempotent replay) path: re-recording an
/// already-recorded step hits the `UNIQUE(idem_key)` violation, re-reads, and
/// adopts the winner. This is the hot path for crash-replay memoization.
#[cfg(feature = "durable-execution")]
fn bench_record_step_dedup(c: &mut Criterion) {
    let (db, run) = fresh_run("wf-bench-dedup");
    let journal = db.workflow_journal();
    // Record step 1 once; every benched call re-records it and adopts the winner.
    journal
        .record_step(
            &run,
            StepRecordSpec::completed(1, "charge", b"receipt".to_vec()),
        )
        .expect("seed record");

    c.bench_function("workflow_journal/record_step_dedup", |b| {
        b.iter(|| {
            let outcome = journal
                .record_step(
                    &run,
                    StepRecordSpec::completed(1, "charge", b"receipt".to_vec()),
                )
                .expect("adopt winner");
            debug_assert!(outcome.deduplicated);
            black_box(outcome);
        });
    });
}

#[cfg(feature = "durable-execution")]
criterion_group!(benches, bench_record_step, bench_record_step_dedup);
#[cfg(feature = "durable-execution")]
criterion_main!(benches);

// When the feature is disabled the bench target still needs a `main`.
#[cfg(not(feature = "durable-execution"))]
fn main() {
    eprintln!("workflow_journal bench requires --features durable-execution");
}
