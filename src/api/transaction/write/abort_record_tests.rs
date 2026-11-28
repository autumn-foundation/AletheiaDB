//! Commit-path abort record tests (Issue #3799).
//!
//! Each test forces one failure after the WAL append. The reopen tests delete
//! the index snapshot first, so recovery must replay the WAL from its start.
//! The test then reads the WAL to prove that the frame and its abort record
//! are on disk.

use crate::AletheiaDB;
use crate::api::transaction::WriteOps;
use crate::config::{AletheiaDBConfig, WalConfigBuilder, durable_config_for_data_dir};
use crate::core::error::{Error, Result};
use crate::core::property::PropertyMap;
use crate::storage::wal::group_commit::LockSite;
use crate::storage::wal::segment_reader::read_entries_from_dir_with_options;
use crate::storage::wal::{DurabilityMode, LSN, WalEntry, WalOperation};
use std::path::Path;

fn open(dir: &Path) -> AletheiaDB {
    AletheiaDB::with_unified_config(durable_config_for_data_dir(dir)).expect("open database")
}

fn open_sync(dir: &Path) -> AletheiaDB {
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .build();
    AletheiaDB::with_unified_config(config).expect("open database")
}

fn coordinator(db: &AletheiaDB) -> &crate::storage::wal::group_commit::GroupCommitCoordinator {
    db.wal
        .group_commit_coordinator()
        .expect("the durable config uses GroupCommit")
}

fn create(db: &AletheiaDB, label: &str) -> Result<crate::core::NodeId> {
    db.create_node(label, PropertyMap::new())
}

/// Commit one transaction with two creates.
fn create_two(db: &AletheiaDB) -> Result<()> {
    db.write(|tx| {
        tx.create_node("Aborted", PropertyMap::new())?;
        tx.create_node("Aborted", PropertyMap::new())?;
        Ok::<_, Error>(())
    })
}

/// All WAL entries on disk. Call it only after the database is dropped.
fn wal_entries(dir: &Path) -> Vec<WalEntry> {
    read_entries_from_dir_with_options(&dir.join("wal"), LSN::initial(), None, true)
        .expect("read WAL")
}

/// The begin LSNs that the on-disk `AbortTx` records name. Each named band
/// must also be on disk, so the test proves the drop, not a missing frame.
fn aborted_bands_on_disk(dir: &Path) -> Vec<u64> {
    let entries = wal_entries(dir);
    let begins: Vec<u64> = entries
        .iter()
        .filter(|e| matches!(e.operation, WalOperation::BeginTx { .. }))
        .map(|e| e.lsn.0)
        .collect();
    let aborted: Vec<u64> = entries
        .iter()
        .filter_map(|e| match e.operation {
            WalOperation::AbortTx { begin_lsn, .. } => Some(begin_lsn.0),
            _ => None,
        })
        .collect();
    for lsn in &aborted {
        assert!(
            begins.contains(lsn),
            "the aborted band at {lsn} is not on disk"
        );
    }
    aborted
}

/// Reopen with a full WAL replay and return the node count.
fn count_after_full_replay(dir: &Path, open_fn: fn(&Path) -> AletheiaDB) -> usize {
    let _ = std::fs::remove_dir_all(dir.join("indexes"));
    open_fn(dir).node_count()
}

/// Site A: registration fails inside the append step. Nothing was applied,
/// so there is no fence, and a later commit works.
#[test]
fn register_failure_is_aborted_without_fence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());

    coordinator(&db).fail_next_acquisition_at_for_test(LockSite::RegisterTransaction);
    let err = create(&db, "Aborted").expect_err("the injected register failure must fail");
    let msg = err.to_string();
    assert!(msg.contains("abort record is durable"), "{msg}");
    assert!(!msg.contains("UNKNOWN"), "{msg}");
    assert_eq!(db.node_count(), 0, "site A applies nothing");

    db.persist_indexes()
        .expect("nothing was applied, so persistence is not fenced");
    create(&db, "Keep").expect("a later commit succeeds");

    drop(db);
    assert_eq!(aborted_bands_on_disk(dir.path()).len(), 1);
    assert_eq!(
        count_after_full_replay(dir.path(), open),
        1,
        "only `Keep` may recover"
    );
}

/// Site B: apply fails after one of two ops. The frame is aborted, and the
/// partly applied live state fences snapshots and later commits.
#[test]
fn partial_apply_failure_is_aborted_and_fenced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());
    create(&db, "Keep").expect("a healthy commit succeeds");

    super::commit_test_hooks::fail_apply_after_ops_on_this_thread(1);
    let err = create_two(&db).expect_err("the injected apply failure must fail");
    assert!(
        matches!(err, Error::Storage(_)),
        "the apply error keeps its type: {err}"
    );
    assert_eq!(db.node_count(), 2, "the first op is applied in memory");

    for refused in [
        db.persist_indexes().expect_err("persist must be fenced"),
        db.backup(&dir.path().join("fenced.albk"))
            .expect_err("backup must be fenced"),
        create(&db, "Later").expect_err("a later commit must be fenced"),
    ] {
        assert!(matches!(refused, Error::FailedPrecondition(_)), "{refused}");
    }

    drop(db);
    assert_eq!(aborted_bands_on_disk(dir.path()).len(), 1);
    assert_eq!(
        count_after_full_replay(dir.path(), open),
        1,
        "only `Keep` may recover"
    );
}

/// Site B in `Synchronous` mode: the abort record is flushed inline.
#[test]
fn sync_mode_partial_apply_failure_is_aborted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open_sync(dir.path());

    super::commit_test_hooks::fail_apply_after_ops_on_this_thread(1);
    create_two(&db).expect_err("the injected apply failure must fail");

    drop(db);
    assert_eq!(aborted_bands_on_disk(dir.path()).len(), 1);
    assert_eq!(count_after_full_replay(dir.path(), open_sync), 0);
}

/// Site C: `wait_for_flush` fails after the writes are visible to other
/// transactions. The commit path must not abort them: a later transaction can
/// depend on them. The error keeps the UNKNOWN disclosure.
#[test]
fn wait_for_flush_failure_is_not_aborted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());

    coordinator(&db).fail_next_acquisition_at_for_test(LockSite::WaitForFlush);
    let err = create(&db, "Visible").expect_err("the injected wait failure must fail");
    let msg = err.to_string();
    assert!(msg.contains("durability status is UNKNOWN"), "{msg}");
    assert!(msg.contains("already visible"), "{msg}");

    db.persist_indexes().expect("site C sets no fence");
    create(&db, "Later").expect("a later commit succeeds");

    drop(db);
    assert!(
        aborted_bands_on_disk(dir.path()).is_empty(),
        "no abort at site C"
    );
    assert_eq!(count_after_full_replay(dir.path(), open), 2);
}

/// When the abort record cannot be confirmed durable, the error keeps the
/// durability-UNKNOWN disclosure, and the coordinator stays usable.
#[test]
fn unconfirmed_abort_record_reports_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());

    // Fail the commit's registration AND the abort record's registration.
    coordinator(&db).fail_next_acquisitions_at_for_test(LockSite::RegisterTransaction, 2);
    let err = create(&db, "Aborted").expect_err("the injected register failure must fail");
    let msg = err.to_string();
    assert!(msg.contains("durability status is UNKNOWN"), "{msg}");
    assert!(
        msg.contains("abort record is not confirmed durable"),
        "{msg}"
    );

    create(&db, "Later").expect("the injection disarms after two failures");
}

/// An empty transaction has no frame, so an apply failure writes no abort
/// record and sets no fence.
#[test]
fn empty_transaction_writes_no_abort_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = open(dir.path());

    super::commit_test_hooks::fail_apply_after_ops_on_this_thread(0);
    let _ = db.write(|_tx| Ok::<_, Error>(()));
    super::commit_test_hooks::clear_apply_failure();
    db.persist_indexes().expect("no fence without a frame");

    drop(db);
    assert!(aborted_bands_on_disk(dir.path()).is_empty());
}
