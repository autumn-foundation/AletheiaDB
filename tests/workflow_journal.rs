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
    WorkflowJournalExt, WorkflowRun,
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
/// the UNIQUE(idem_key) violation internally, re-reads, and returns the same
/// memoized output. Exactly one Step node exists for the idem_key.
#[test]
fn dup_step_record_is_idempotent() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-dup");
    let journal = db.workflow_journal();

    let first = journal
        .record_step(
            &run,
            StepRecordSpec::completed(1, "charge", b"receipt-1".to_vec()),
        )
        .expect("first record");
    assert!(!first.deduplicated, "first write should not be a dedup");
    assert_eq!(first.record.output(), Some(&b"receipt-1"[..]));

    let second = journal
        .record_step(
            &run,
            StepRecordSpec::completed(1, "charge", b"receipt-1".to_vec()),
        )
        .expect("second record adopts winner");
    assert!(second.deduplicated, "second write should adopt the winner");
    assert_eq!(second.record.output(), Some(&b"receipt-1"[..]));

    // Exactly one Step node for this idem_key.
    assert_eq!(count_steps_for_key(&db, "wf-dup", 1), 1);
    // Both calls returned the same output.
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
/// by step_number, and memoized outputs map to the correct step.
#[test]
fn backdated_valid_time_preserves_step_order() {
    let db = new_db();
    let run = bootstrap_run(&db, "wf-order");
    let journal = db.workflow_journal();

    // Record step 1 at "now".
    let now: Timestamp = time::now();
    journal
        .record_step(
            &run,
            StepRecordSpec::completed(1, "first", b"out-1".to_vec()).with_valid_time(now),
        )
        .expect("record step 1");

    // Record step 2 with a valid_time EARLIER than step 1's.
    let earlier = Timestamp::new(now.wallclock() - 3_600_000_000, 0).expect("earlier ts");
    journal
        .record_step(
            &run,
            StepRecordSpec::completed(2, "second", b"out-2".to_vec()).with_valid_time(earlier),
        )
        .expect("record step 2");

    let steps = journal.list_steps("wf-order").expect("list steps");
    let numbers: Vec<i64> = steps.iter().map(WorkflowRunStepNum::num).collect();
    assert_eq!(
        numbers,
        vec![1, 2],
        "must be ordered by step_number, not valid_from"
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
