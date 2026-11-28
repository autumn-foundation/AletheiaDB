//! Commit-path abort record tests (Issue #3799).
//!
//! Each test forces one failure after the WAL append, then reopens the
//! database from the same directory. The failed write must not come back.

use crate::AletheiaDB;
use crate::config::durable_config_for_data_dir;
use crate::core::error::Error;
use crate::core::property::PropertyMap;
use crate::storage::wal::group_commit::LockSite;
use std::path::Path;

fn open(dir: &Path) -> AletheiaDB {
    AletheiaDB::with_unified_config(durable_config_for_data_dir(dir)).expect("open database")
}

fn coordinator(db: &AletheiaDB) -> &crate::storage::wal::group_commit::GroupCommitCoordinator {
    db.wal
        .group_commit_coordinator()
        .expect("the durable config uses GroupCommit")
}

fn create(db: &AletheiaDB, label: &str) -> crate::core::error::Result<crate::core::NodeId> {
    db.create_node(label, PropertyMap::new())
}

/// Reopen `dir` and return the node count.
fn count_after_reopen(dir: &Path) -> usize {
    open(dir).node_count()
}

/// Site C: `wait_for_flush` fails after apply. The abort record is durable,
/// so recovery drops the frame, and the error says so.
#[test]
fn wait_for_flush_failure_is_aborted_and_not_replayed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());
    create(&db, "Keep").expect("a healthy commit succeeds");

    coordinator(&db).fail_next_acquisition_at_for_test(LockSite::WaitForFlush);
    let err = create(&db, "Aborted").expect_err("the injected wait failure must fail the commit");
    let msg = err.to_string();
    assert!(msg.contains("abort record is durable"), "{msg}");
    assert!(!msg.contains("UNKNOWN"), "{msg}");

    drop(db);
    assert_eq!(count_after_reopen(dir.path()), 1, "only `Keep` may recover");
}

/// Site C also fences index persistence and backup: the live state holds a
/// write that the WAL disowns, so no snapshot may capture it.
#[test]
fn wait_for_flush_failure_fences_persistence_and_backup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());
    coordinator(&db).fail_next_acquisition_at_for_test(LockSite::WaitForFlush);
    create(&db, "Aborted").expect_err("the injected wait failure must fail the commit");

    let persist = db.persist_indexes().expect_err("persist must be fenced");
    assert!(matches!(persist, Error::FailedPrecondition(_)), "{persist}");
    let backup = db
        .backup(&dir.path().join("fenced.albk"))
        .expect_err("backup must be fenced");
    assert!(matches!(backup, Error::FailedPrecondition(_)), "{backup}");
}

/// Site A: registration fails inside the append step. Nothing was applied,
/// so there is no fence, and a later commit still works.
#[test]
fn register_failure_is_aborted_without_fence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());

    coordinator(&db).fail_next_acquisition_at_for_test(LockSite::RegisterTransaction);
    let err = create(&db, "Aborted").expect_err("the injected register failure must fail");
    let msg = err.to_string();
    assert!(msg.contains("abort record is durable"), "{msg}");

    db.persist_indexes()
        .expect("nothing was applied, so persistence is not fenced");
    create(&db, "Keep").expect("a later commit succeeds");

    drop(db);
    assert_eq!(count_after_reopen(dir.path()), 1, "only `Keep` may recover");
}

/// Site B: apply fails after the append. The frame is aborted and the
/// partly-applied live state is fenced.
#[test]
fn apply_failure_is_aborted_and_fenced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());

    super::commit_test_hooks::fail_next_apply_on_this_thread();
    create(&db, "Aborted").expect_err("the injected apply failure must fail");
    let persist = db.persist_indexes().expect_err("persist must be fenced");
    assert!(matches!(persist, Error::FailedPrecondition(_)), "{persist}");

    drop(db);
    assert_eq!(count_after_reopen(dir.path()), 0);
}

/// When the abort record cannot be confirmed durable, the error keeps the
/// durability-UNKNOWN disclosure.
#[test]
fn unconfirmed_abort_record_reports_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());

    // Fail the commit's registration AND the abort record's registration.
    coordinator(&db).fail_next_acquisitions_at_for_test(LockSite::RegisterTransaction, 2);
    let err = create(&db, "Aborted").expect_err("the injected register failure must fail");
    let msg = err.to_string();
    assert!(msg.contains("durability status UNKNOWN"), "{msg}");
    assert!(
        msg.contains("abort record is not confirmed durable"),
        "{msg}"
    );
}
