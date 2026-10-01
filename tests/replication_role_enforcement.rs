//! Integration tests for node-role enforcement on the Rust API surface
//! (Issue #3355, Slice A: roles + always-compiled read-only-replica
//! rejection).
//!
//! Covers:
//! 1. Default role is `Primary`; `enter_replica_mode`/`promote_to_primary`
//!    flip the role; writes succeed again after promotion.
//! 2. Every convenience write method funnels through `write_transaction()`
//!    and is rejected with `TransactionError::ReadOnlyReplica` on a replica.
//! 3. Reads are entirely unaffected on a replica.
//! 4. The commit-time recheck catches a promotion/demotion race: a
//!    transaction constructed while `Primary` is rejected at commit if the
//!    node is demoted before `commit()` runs.
//! 7. `DatabaseStats.replication.role` reflects the current role.
//!
//! Run with: `cargo test --test replication_role_enforcement`

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::error::{Error, TransactionError};
use aletheiadb::{AletheiaDB, GLOBAL_INTERNER, NodeRole, PropertyMapBuilder};

fn props() -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new().insert("name", "Alice").build()
}

fn assert_read_only_replica(err: &Error) {
    match err {
        Error::Transaction(TransactionError::ReadOnlyReplica) => {}
        other => panic!("expected TransactionError::ReadOnlyReplica, got: {other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains("read-only replica"),
        "error message should mention the read-only replica, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 1. Role transitions
// ---------------------------------------------------------------------------

#[test]
fn default_role_is_primary() {
    let db = AletheiaDB::new().expect("create db");
    assert_eq!(db.node_role(), NodeRole::Primary);
    assert!(!db.is_replica());
}

#[test]
fn enter_replica_mode_flips_role() {
    let db = AletheiaDB::new().expect("create db");
    db.enter_replica_mode();
    assert_eq!(db.node_role(), NodeRole::Replica);
    assert!(db.is_replica());
}

#[test]
fn promote_to_primary_flips_back_and_writes_succeed_again() {
    let db = AletheiaDB::new().expect("create db");
    db.enter_replica_mode();
    assert!(db.create_node("Person", props()).is_err());

    db.promote_to_primary().expect("promote");
    assert_eq!(db.node_role(), NodeRole::Primary);

    let node_id = db
        .create_node("Person", props())
        .expect("write succeeds again after promotion");
    assert!(db.get_node(node_id).is_ok());
}

#[test]
fn promote_to_primary_on_a_primary_is_idempotent_ok() {
    let db = AletheiaDB::new().expect("create db");
    assert_eq!(db.node_role(), NodeRole::Primary);
    db.promote_to_primary().expect("promoting a primary is Ok");
    assert_eq!(db.node_role(), NodeRole::Primary);
}

// ---------------------------------------------------------------------------
// 2. Every write surface rejects on a replica
// ---------------------------------------------------------------------------

#[test]
fn write_transaction_rejected_on_replica() {
    let db = AletheiaDB::new().expect("create db");
    db.enter_replica_mode();

    let err = db
        .write_transaction()
        .err()
        .expect("write_transaction must fail");
    assert_read_only_replica(&err);
}

#[test]
fn write_transaction_with_options_rejected_on_replica() {
    use aletheiadb::WriteOptions;

    let db = AletheiaDB::new().expect("create db");
    db.enter_replica_mode();

    let err = db
        .write_transaction_with_options(WriteOptions::new())
        .err()
        .expect("write_transaction_with_options must fail");
    assert_read_only_replica(&err);
}

#[test]
fn create_node_rejected_on_replica() {
    let db = AletheiaDB::new().expect("create db");
    db.enter_replica_mode();

    let err = db.create_node("Person", props()).expect_err("must fail");
    assert_read_only_replica(&err);
}

#[test]
fn create_edge_rejected_on_replica() {
    // Create endpoints while still primary, then demote.
    let db = AletheiaDB::new().expect("create db");
    let a = db.create_node("Person", props()).expect("create a");
    let b = db.create_node("Person", props()).expect("create b");

    db.enter_replica_mode();

    let err = db
        .create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .expect_err("must fail");
    assert_read_only_replica(&err);
}

#[test]
fn update_node_rejected_on_replica() {
    let db = AletheiaDB::new().expect("create db");
    let node_id = db.create_node("Person", props()).expect("create");

    db.enter_replica_mode();

    let err = db
        .update_node_with_valid_time(
            node_id,
            PropertyMapBuilder::new().insert("name", "Bob").build(),
            None,
        )
        .expect_err("must fail");
    assert_read_only_replica(&err);
}

#[test]
fn delete_node_rejected_on_replica() {
    use aletheiadb::api::transaction::WriteRequestOptions;

    let db = AletheiaDB::new().expect("create db");
    let node_id = db.create_node("Person", props()).expect("create");

    db.enter_replica_mode();

    let err = db
        .delete_node_with_options(node_id, WriteRequestOptions::default())
        .expect_err("must fail");
    assert_read_only_replica(&err);
}

// ---------------------------------------------------------------------------
// 3. Reads are unaffected on a replica
// ---------------------------------------------------------------------------

#[test]
fn reads_still_work_on_replica() {
    let db = AletheiaDB::new().expect("create db");
    let alice = db.create_node("Person", props()).expect("create alice");
    let bob = db.create_node("Person", props()).expect("create bob");
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("create edge");

    db.enter_replica_mode();
    assert!(db.is_replica());

    // Plain get.
    let node = db.get_node(alice).expect("get_node must work on a replica");
    let label = GLOBAL_INTERNER
        .resolve_with(node.label, |s| s.to_string())
        .expect("label resolves");
    assert_eq!(label, "Person");

    // History.
    let history = db
        .get_node_history(alice)
        .expect("get_node_history must work on a replica");
    assert!(!history.versions.is_empty());

    // Traversal / query builder.
    let results = db
        .query()
        .start(alice)
        .traverse("KNOWS")
        .execute(&db)
        .expect("traversal must work on a replica")
        .collect_all()
        .expect("collecting traversal results must work on a replica");
    assert_eq!(results.len(), 1);
}

// ---------------------------------------------------------------------------
// 4. Commit-time promotion/demotion-race guard
// ---------------------------------------------------------------------------

#[test]
fn commit_rejected_when_demoted_after_construction() {
    let db = AletheiaDB::new().expect("create db");

    // Construct the transaction while still Primary...
    let mut tx = db
        .write_transaction()
        .expect("construction succeeds while primary");
    tx.create_node("Person", props()).expect("buffer a write");

    // ...then the node is demoted before commit runs.
    db.enter_replica_mode();

    let err = tx.commit().expect_err("commit must be rejected");
    assert_read_only_replica(&err);
}

// ---------------------------------------------------------------------------
// 7. Stats skeleton
// ---------------------------------------------------------------------------

#[test]
fn stats_reports_replication_role() {
    let db = AletheiaDB::new().expect("create db");
    assert_eq!(db.stats().replication.role, "primary");
    assert!(db.stats().replication.replica.is_none());

    db.enter_replica_mode();
    assert_eq!(db.stats().replication.role, "replica");
    assert!(db.stats().replication.replica.is_none());
}
