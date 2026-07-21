//! Integration tests for the DBOS Phase 3a workflow journal.
//!
//! These cover the §8 acceptance rows that apply to Phase 3a: exactly-once step
//! recording, crash-before-record re-execution, memoized replay, backdated
//! valid-time ordering, and fail-loud orchestration divergence.
#![cfg(feature = "durable-execution")]

use std::cell::Cell;

use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::{
    AletheiaDB, CreateRunSpec, StepExecError, StepRecordSpec, StepStatus, WorkflowError,
    WorkflowJournalExt, WorkflowRun, WorkflowStatus,
};

fn new_db() -> AletheiaDB {
    AletheiaDB::new().expect("in-memory db")
}

fn bootstrap_run(db: &AletheiaDB, workflow_id: &str) -> WorkflowRun {
    let journal = db.workflow_journal();
    journal.bootstrap().expect("bootstrap constraints");
    journal
        .create_run(CreateRunSpec::new(workflow_id, "test-workflow"))
        .expect("create run")
}

/// Count the number of Step nodes carrying a given idem_key (should always be 1
/// once recorded, thanks to the UNIQUE constraint).
fn count_steps_for_key(db: &AletheiaDB, workflow_id: &str, step_number: i64) -> usize {
    let key = format!("{workflow_id}:{step_number}");
    let value = aletheiadb::PropertyValue::from(key.as_str());
    db.find_nodes_by_property("Step", "idem_key", &value).len()
}

/// §8 #1 — recording the same step twice is idempotent: the second attempt hits
/// the UNIQUE(idem_key) violation internally, re-reads, and returns the
/// **winner's** memoized output — NOT the loser's freshly-supplied output.
///
/// To actually prove the winner's value is returned (rather than a regression
/// that echoes the caller's own input), the second record supplies a *different*
/// output `B` under the same name; the adopted result must still be the winner's
/// `A`, and exactly one Step node (holding `A`) must exist.
#[test]
fn dup_step_record_is_idempotent() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-dup");
    let journal = db.workflow_journal();

    let first = journal
        .record_step(
            &run,
            StepRecordSpec::completed(1, "charge", b"receipt-A".to_vec()),
        )
        .expect("first record");
    assert!(!first.deduplicated, "first write should not be a dedup");
    assert_eq!(first.record.output(), Some(&b"receipt-A"[..]));

    // Second record: SAME name, DIFFERENT output B. Must adopt the winner A.
    let second = journal
        .record_step(
            &run,
            StepRecordSpec::completed(1, "charge", b"receipt-B".to_vec()),
        )
        .expect("second record adopts winner");
    assert!(second.deduplicated, "second write should adopt the winner");
    assert_eq!(
        second.record.output(),
        Some(&b"receipt-A"[..]),
        "must return the winner A, not the loser's B"
    );

    // Exactly one Step node for this idem_key.
    assert_eq!(count_steps_for_key(&db, "wf-dup", 1), 1);

    // The stored node holds the winner A, not B.
    let stored = journal
        .get_step("wf-dup", 1)
        .expect("get step")
        .expect("step exists");
    assert_eq!(stored.output(), Some(&b"receipt-A"[..]));

    // Both calls returned the same (winner) output.
    assert_eq!(first.record.output(), second.record.output());
}

/// §8 #2 — a crash between executing and recording re-executes the step on the
/// next run, and exactly one Step ends up recorded.
#[test]
fn crash_before_record_reexecutes_once() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-crash");
    let journal = db.workflow_journal();

    let counter = Cell::new(0u32);
    let exec = || {
        counter.set(counter.get() + 1);
        Ok::<Vec<u8>, StepExecError>(b"work".to_vec())
    };

    // Simulate a crash: the work runs but the process dies BEFORE record_step.
    // We model this by invoking the executor directly and discarding the result.
    let _ = exec().expect("pre-crash execution");
    assert_eq!(counter.get(), 1);
    // Nothing was recorded yet.
    assert_eq!(count_steps_for_key(&db, "wf-crash", 1), 0);

    // On restart, get_or_record_step finds no Step and must re-execute exactly
    // once, then record it.
    let value = journal
        .get_or_record_step(&run, 1, "do-work", exec)
        .expect("re-execute and record");
    assert_eq!(
        counter.get(),
        2,
        "executor must run exactly once on re-drive"
    );
    assert!(!value.from_memo, "value came from re-execution, not memo");
    assert_eq!(value.output, b"work".to_vec());

    // Exactly one Step recorded after re-drive.
    assert_eq!(count_steps_for_key(&db, "wf-crash", 1), 1);
}

/// §8 #6 — replay returns the memoized output and never recomputes.
#[test]
fn replay_returns_memoized_not_recomputed() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-replay");
    let journal = db.workflow_journal();

    journal
        .record_step(&run, StepRecordSpec::completed(1, "compute", b"X".to_vec()))
        .expect("record X");

    let invoked = Cell::new(false);
    let exec = || {
        invoked.set(true);
        // If this ever runs, return a different value so a bug would be visible.
        Ok::<Vec<u8>, StepExecError>(b"Y".to_vec())
    };

    let value = journal
        .get_or_record_step(&run, 1, "compute", exec)
        .expect("memoized replay");
    assert!(!invoked.get(), "executor must NOT be invoked on memo hit");
    assert!(value.from_memo, "value must be from memo");
    assert_eq!(value.output, b"X".to_vec(), "must return memoized X, not Y");
}

/// §8 #8 — a backdated valid_time does not reorder replay: list_steps is sorted
/// by step_number, never by node-id (insertion order) or valid_from.
///
/// This is arranged so BOTH node-id order and valid_from order DISAGREE with
/// step_number order: the higher-numbered step 2 is recorded **first** (so it
/// gets the lower node id) and with an **earlier** valid_time; the lower-numbered
/// step 1 is recorded **last** (higher node id) at "now". A regression that
/// removed the `sort_by_key(step_number)` (or sorted by id / valid_from) would
/// yield `[2, 1]` and FAIL this test.
#[test]
fn backdated_valid_time_preserves_step_order() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-order");
    let journal = db.workflow_journal();

    let now: Timestamp = time::now();
    let earlier = Timestamp::new(now.wallclock() - 3_600_000_000, 0).expect("earlier ts");

    // Record step 2 FIRST (lower node id) with an EARLIER valid_time.
    journal
        .record_step(
            &run,
            StepRecordSpec::completed(2, "second", b"out-2".to_vec()).with_valid_time(earlier),
        )
        .expect("record step 2");

    // Record step 1 LAST (higher node id) at "now".
    journal
        .record_step(
            &run,
            StepRecordSpec::completed(1, "first", b"out-1".to_vec()).with_valid_time(now),
        )
        .expect("record step 1");

    let steps = journal.list_steps("wf-order").expect("list steps");
    let numbers: Vec<i64> = steps.iter().map(WorkflowRunStepNum::num).collect();
    assert_eq!(
        numbers,
        vec![1, 2],
        "must be ordered by step_number, not node-id (insertion) or valid_from"
    );

    assert_eq!(steps[0].output(), Some(&b"out-1"[..]));
    assert_eq!(steps[1].output(), Some(&b"out-2"[..]));
}

// Local helper trait so the test above can map to step numbers concisely.
trait WorkflowRunStepNum {
    fn num(&self) -> i64;
}
impl WorkflowRunStepNum for aletheiadb::StepRecord {
    fn num(&self) -> i64 {
        self.step_number()
    }
}

/// §8 #12 — orchestration divergence fails loud on a step-name mismatch and
/// never returns the wrong step's output.
#[test]
fn orchestration_divergence_fails_loud_on_name_mismatch() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-diverge");
    let journal = db.workflow_journal();

    journal
        .record_step(
            &run,
            StepRecordSpec::completed(3, "charge", b"charged".to_vec()),
        )
        .expect("record charge");

    // get_or_record_step with a diverging expected name must fail loud and NOT
    // invoke the executor (memo-hit path).
    let invoked = Cell::new(false);
    let exec = || {
        invoked.set(true);
        Ok::<Vec<u8>, StepExecError>(b"refunded".to_vec())
    };
    let err = journal
        .get_or_record_step(&run, 3, "refund", exec)
        .expect_err("must diverge");
    assert!(
        !invoked.get(),
        "executor must not run on a divergent memo hit"
    );
    match err {
        WorkflowError::OrchestrationDivergence {
            ref expected,
            ref found,
            step_number,
            ..
        } => {
            assert_eq!(step_number, 3);
            assert_eq!(expected, "refund");
            assert_eq!(found, "charge");
        }
        other => panic!("expected OrchestrationDivergence, got {other:?}"),
    }

    // record_step with a diverging name must also fail loud.
    let err2 = journal
        .record_step(
            &run,
            StepRecordSpec::completed(3, "refund", b"refunded".to_vec()),
        )
        .expect_err("record must diverge");
    assert!(matches!(
        err2,
        WorkflowError::OrchestrationDivergence { .. }
    ));

    // The stored step is unchanged: still "charge" with its original output.
    let stored = journal
        .get_step("wf-diverge", 3)
        .expect("get step")
        .expect("step exists");
    assert_eq!(stored.name(), "charge");
    assert_eq!(stored.status(), StepStatus::Completed);
    assert_eq!(stored.output(), Some(&b"charged"[..]));
}

/// A step recorded as `failed` propagates its error on replay — a memo-hit
/// `get_or_record_step` (and a `get_step`) surface `StepFailed` with the message,
/// never an empty output, and never invoke the executor.
#[test]
fn failed_step_replay_propagates_error() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-failed");
    let journal = db.workflow_journal();

    journal
        .record_step(&run, StepRecordSpec::failed(1, "risky", "boom"))
        .expect("record failed step");

    // get_step surfaces the failed record verbatim.
    let stored = journal
        .get_step("wf-failed", 1)
        .expect("get step")
        .expect("step exists");
    assert_eq!(stored.status(), StepStatus::Failed);
    assert_eq!(stored.error(), Some("boom"));
    assert_eq!(stored.output(), None);

    // Memo-hit get_or_record_step must surface StepFailed (not empty success)
    // and must NOT invoke exec.
    let invoked = Cell::new(false);
    let exec = || {
        invoked.set(true);
        Ok::<Vec<u8>, StepExecError>(b"should-not-run".to_vec())
    };
    let err = journal
        .get_or_record_step(&run, 1, "risky", exec)
        .expect_err("failed step must propagate");
    assert!(!invoked.get(), "exec must not run on a failed memo hit");
    match err {
        WorkflowError::StepFailed {
            ref message,
            step_number,
            ..
        } => {
            assert_eq!(step_number, 1);
            assert_eq!(message, "boom");
        }
        other => panic!("expected StepFailed, got {other:?}"),
    }
}

/// An `exec` closure that returns `Err` records **no** Step, so a later drive
/// re-executes; a subsequent succeeding drive records exactly once and returns
/// its output. The executor runs twice total; exactly one Step ends up recorded.
#[test]
fn exec_failure_is_not_recorded_and_reexecutes() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-execfail");
    let journal = db.workflow_journal();

    let counter = Cell::new(0u32);

    // First drive: exec fails → no Step recorded, StepExecutionFailed surfaced.
    let failing = || {
        counter.set(counter.get() + 1);
        Err::<Vec<u8>, StepExecError>(StepExecError::new("transient"))
    };
    let err = journal
        .get_or_record_step(&run, 1, "work", failing)
        .expect_err("exec failure surfaces");
    assert!(matches!(err, WorkflowError::StepExecutionFailed { .. }));
    assert_eq!(counter.get(), 1, "exec ran once on first drive");
    assert_eq!(
        count_steps_for_key(&db, "wf-execfail", 1),
        0,
        "a failed exec records NO Step"
    );

    // Second drive: exec succeeds → records once, returns its output.
    let succeeding = || {
        counter.set(counter.get() + 1);
        Ok::<Vec<u8>, StepExecError>(b"done".to_vec())
    };
    let value = journal
        .get_or_record_step(&run, 1, "work", succeeding)
        .expect("second drive records");
    assert_eq!(counter.get(), 2, "exec ran exactly twice total");
    assert!(!value.from_memo, "second drive is a fresh write");
    assert_eq!(value.output, b"done".to_vec());
    assert_eq!(
        count_steps_for_key(&db, "wf-execfail", 1),
        1,
        "exactly one Step recorded"
    );
}

/// Creating two runs with the same `workflow_id` rejects the second with
/// `RunAlreadyExists`; `get_run` round-trips the first run's fields.
#[test]
fn create_run_duplicate_is_rejected() {
    let db = new_db();
    let journal = db.workflow_journal();
    journal.bootstrap().expect("bootstrap");

    let created_before = time::now().wallclock();
    let run = journal
        .create_run(
            CreateRunSpec::new("wf-once", "checkout")
                .owner("agent-1")
                .input(b"payload".to_vec()),
        )
        .expect("first create");

    // Duplicate workflow_id is rejected.
    let err = journal
        .create_run(CreateRunSpec::new("wf-once", "checkout"))
        .expect_err("duplicate must be rejected");
    match err {
        WorkflowError::RunAlreadyExists { ref workflow_id } => {
            assert_eq!(workflow_id, "wf-once");
        }
        other => panic!("expected RunAlreadyExists, got {other:?}"),
    }

    // get_run round-trips the fields of the first run.
    let fetched = journal
        .get_run("wf-once")
        .expect("get_run ok")
        .expect("run exists");
    assert_eq!(fetched.workflow_id(), "wf-once");
    assert_eq!(fetched.name(), "checkout");
    assert_eq!(fetched.status(), WorkflowStatus::Pending);
    assert_eq!(fetched.owner(), Some("agent-1"));
    assert_eq!(fetched.input(), Some(&b"payload"[..]));
    assert_eq!(fetched.node_id(), run.node_id());
    let created = fetched.created_at().expect("created_at set");
    assert!(
        created >= created_before,
        "created_at must be >= the pre-create wallclock"
    );
}

/// `get_run` for an unknown workflow_id returns `Ok(None)`.
#[test]
fn get_run_miss_returns_none() {
    let db = new_db();
    let journal = db.workflow_journal();
    journal.bootstrap().expect("bootstrap");
    let miss = journal.get_run("does-not-exist").expect("get_run ok");
    assert!(miss.is_none(), "unknown run must be None");
}

/// Regression for the adopt-winner retry (FIX 1): two threads concurrently
/// `record_step` the SAME (workflow_id, step_number, name) with possibly
/// different outputs. Both must return `Ok` (no `Malformed`, no panic), exactly
/// one Step node must exist, and both returned outputs must equal the single
/// stored winner's output. The reserved-before-applied window is handled by the
/// bounded adopt-retry, so neither loser sees a transient `None`.
#[test]
fn concurrent_dup_record_adopts_without_error() {
    use std::sync::{Arc, Barrier};

    // A few iterations to make the commit race likely at least once.
    for iter in 0..24 {
        let db = Arc::new(new_db());
        let workflow_id = format!("wf-concurrent-{iter}");
        let run = bootstrap_run(&db, &workflow_id);
        let barrier = Arc::new(Barrier::new(2));

        let mut handles = Vec::new();
        for t in 0..2u8 {
            let db = Arc::clone(&db);
            let run = run.clone();
            let barrier = Arc::clone(&barrier);
            // Possibly-different outputs so adopting the loser's value would be
            // observable.
            let output = vec![b'A' + t];
            handles.push(std::thread::spawn(move || {
                let journal = db.workflow_journal();
                barrier.wait();
                journal.record_step(&run, StepRecordSpec::completed(1, "charge", output))
            }));
        }

        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread must not panic"))
            .collect();

        // Both calls must succeed — no Malformed, no error.
        for r in &results {
            assert!(
                r.is_ok(),
                "concurrent record must not error, got {:?}",
                r.as_ref().err()
            );
        }

        // Exactly one Step node exists for the idem_key.
        assert_eq!(
            count_steps_for_key(&db, &workflow_id, 1),
            1,
            "exactly one Step must exist despite the race"
        );

        // Both returned outputs equal the single stored winner's output.
        let stored = db
            .workflow_journal()
            .get_step(&workflow_id, 1)
            .expect("get step")
            .expect("step exists");
        let winner = stored.output().map(<[u8]>::to_vec);
        for r in &results {
            let outcome = r.as_ref().expect("ok result");
            assert_eq!(
                outcome.record.output().map(<[u8]>::to_vec),
                winner,
                "both callers must observe the winner's output"
            );
        }
    }
}

/// bootstrap is idempotent: calling it twice does not error.
#[test]
fn bootstrap_is_idempotent() {
    let db = new_db();
    let journal = db.workflow_journal();
    journal.bootstrap().expect("first bootstrap");
    journal
        .bootstrap()
        .expect("second bootstrap must not error");

    let constraints = db.list_unique_constraints();
    assert!(
        constraints
            .iter()
            .any(|(l, p)| l == "WorkflowRun" && p == "workflow_id")
    );
    assert!(
        constraints
            .iter()
            .any(|(l, p)| l == "Step" && p == "idem_key")
    );
}
