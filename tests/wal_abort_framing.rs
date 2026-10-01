#![cfg(test)]

//! Crash-recovery tests for WAL **abort** framing (Issue #3413, remaining gap).
//!
//! The commit-framing already on trunk (`[BeginTx, ..ops.., CommitTx]`) fixed the
//! torn-batch prefix-replay and per-entry timestamp-bisection defects. It did NOT
//! fix the case these tests pin: a transaction whose **complete, valid** frame is
//! durably fsync'd and is **then rejected** at the commit guard (a lost CAS/lease
//! claim, a stale fence, or an orphaning delete/dangling edge). Because the WAL
//! write precedes the guard check and recovery replays frames without re-running
//! preconditions, the rejected write is silently **reapplied on crash recovery** —
//! resurrecting exactly the zombie the guard refused live.
//!
//! Each test drives **real** WAL replay: it writes through a durable on-disk
//! `AletheiaDB::open`, drops the handle, **deletes the index-persistence
//! snapshot** so the reopen must replay the WAL from LSN 1 (rather than loading
//! current state from the snapshot the drop-time persist wrote at a later LSN),
//! then reopens. A rejected write reappearing after that deletion is proof the
//! phantom WAL frame replayed.

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::error::{Error, TransactionError};
use aletheiadb::core::property::PropertyValue;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use std::path::Path;
use std::time::Duration;

/// Delete the index-persistence snapshot so the next `open` must replay the WAL
/// from the beginning instead of loading current state from the snapshot. The
/// `expect` (not `.ok()`) guards against the test silently regressing into being
/// satisfied by a snapshot instead of genuine WAL decode.
fn force_full_replay(data_dir: &Path) {
    std::fs::remove_dir_all(data_dir.join("indexes")).expect("index snapshot dir must exist");
}

/// R1–R3 (CAS / lease / **fence** family): a single-threaded **fenced claim**
/// whose `new_fence` does not beat the stored fence passes the claim gate
/// (version matches) but fails the fence gate under the commit-serialization
/// guard. Fenced claims are excluded from the pre-WAL fast-path
/// (`conflict::detect_conflicts`), so today the rejected claim's full-replace
/// `UpdateNode` is fsync'd as a durable `[BeginTx, UpdateNode, CommitTx]` frame
/// BEFORE `FenceTooLow` is raised. After a forced WAL replay the rejected fence
/// (and lease owner) must NOT resurface.
#[test]
fn rejected_fenced_claim_leaves_no_phantom_frame_after_replay() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let id = {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");

        // A run held at fence = 10.
        let id = db
            .create_node(
                "WorkflowRun",
                PropertyMapBuilder::new()
                    .insert("wf", "W1")
                    .insert("fence", 10_i64)
                    .build(),
            )
            .expect("create run");
        let v = db.get_node(id).expect("get run").current_version;

        // Fenced claim with new_fence = 5 (< stored 10). The version matches, so
        // the claim gate passes; the fence gate (5 > 10 == false) rejects it with
        // FenceTooLow — but only AFTER the full-replace UpdateNode frame is durable.
        let err = db
            .claim_with_lease_fenced(
                id,
                v,
                "lease_owner",
                "lease_until",
                "fence",
                PropertyValue::from("Z"),
                Duration::from_secs(3600),
                5_i64,
                PropertyMapBuilder::new().insert("wf", "W1").build(),
            )
            .expect_err("a fence that does not beat the stored fence must be rejected");
        assert!(
            matches!(
                err,
                Error::Transaction(TransactionError::FenceTooLow { .. })
            ),
            "expected FenceTooLow, got {err:?}"
        );

        // Live view is untouched: the guard rejected before applying anything.
        let live = db.get_node(id).expect("get run after rejected claim");
        assert_eq!(
            live.get_property("fence").and_then(|p| p.as_int()),
            Some(10),
            "rejected claim must not change live state"
        );
        assert!(
            live.get_property("lease_owner").is_none(),
            "rejected claim must not stamp an owner live"
        );

        id
    };

    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).expect("reopen durable db");
    let node = db2.get_node(id).expect("get run after replay");

    // The phantom frame must NOT have replayed.
    assert_eq!(
        node.get_property("fence").and_then(|p| p.as_int()),
        Some(10),
        "rejected fenced claim (fence=5) must NOT resurface after WAL replay"
    );
    assert!(
        node.get_property("lease_owner").is_none(),
        "rejected claim's lease_owner must NOT resurface after WAL replay"
    );
}

/// R4 (delete-orphan write-skew): a `delete_node` that a concurrent transaction's
/// newly-committed edge turns into an orphaning delete is rejected under the
/// commit guard (`detect_delete_orphan_write_skew`, Issue #3416) — but only AFTER
/// its tombstone frame is durably fsync'd. After a forced WAL replay the
/// rejected delete must NOT resurface (the node and edge must survive).
#[test]
fn rejected_orphaning_delete_leaves_no_phantom_frame_after_replay() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let (n2, edge_id) = {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");

        let n1 = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "1").build())
            .expect("n1");
        let n2 = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "2").build())
            .expect("n2");

        // Open the deleter FIRST so its snapshot precedes the edge creation.
        let mut deleter = db.write_transaction().expect("open deleter tx");
        deleter.delete_node(n2).expect("buffer delete of n2");

        // A concurrent tx creates and commits an edge n1->n2 (durable + applied).
        let edge_id = {
            let mut creator = db.write_transaction().expect("open creator tx");
            let e = creator
                .create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
                .expect("buffer edge");
            creator.commit().expect("edge commit");
            e
        };

        // The deleter now commits: the write-skew re-check sees the edge committed
        // after its snapshot (referencing n2, not closed by this tx) and aborts —
        // AFTER the delete tombstone frame is durably appended+fsync'd.
        let err = deleter
            .commit()
            .expect_err("deleting n2 would orphan the concurrently-created edge");
        assert!(
            matches!(
                err,
                Error::Transaction(TransactionError::ValidationFailed { .. })
            ),
            "expected ValidationFailed write-skew abort, got {err:?}"
        );

        (n2, edge_id)
    };

    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).expect("reopen durable db");

    // The rejected delete must NOT have replayed: n2 alive, edge intact.
    assert!(
        db2.get_node(aletheiadb::core::NodeId::new(n2.as_u64()).unwrap())
            .is_ok(),
        "n2 must survive; the rejected orphaning delete must not replay"
    );
    assert!(
        db2.get_edge(aletheiadb::core::EdgeId::new(edge_id.as_u64()).unwrap())
            .is_ok(),
        "the edge must survive with a live endpoint"
    );
}

/// Positive control: a *successfully committed* fenced claim (fence strictly
/// greater than stored) must survive a forced WAL replay unchanged — guards
/// against the fix over-discarding legitimately committed frames.
#[test]
fn committed_fenced_claim_survives_replay() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let id = {
        let db = AletheiaDB::open(&data_dir).expect("open durable db");
        let id = db
            .create_node(
                "WorkflowRun",
                PropertyMapBuilder::new().insert("fence", 3_i64).build(),
            )
            .expect("create run");
        let v = db.get_node(id).expect("get run").current_version;

        db.claim_with_lease_fenced(
            id,
            v,
            "lease_owner",
            "lease_until",
            "fence",
            PropertyValue::from("Winner"),
            Duration::from_secs(3600),
            4_i64, // 4 > 3 -> admitted
            PropertyMapBuilder::new().build(),
        )
        .expect("fenced claim 4 > 3 must succeed");
        id
    };

    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).expect("reopen durable db");
    let node = db2.get_node(id).expect("get run after replay");
    assert_eq!(
        node.get_property("fence").and_then(|p| p.as_int()),
        Some(4),
        "committed fenced claim must survive replay"
    );
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|p| p.as_str().map(String::from)),
        Some("Winner".to_string()),
        "committed claim's owner must survive replay"
    );
}
