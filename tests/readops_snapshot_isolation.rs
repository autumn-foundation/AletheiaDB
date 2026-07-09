//! Snapshot-isolation tests for the `ReadOps` edge-listing error path
//! (Issue #359 review follow-up).
//!
//! The edge-listing methods (`get_outgoing_edges`, `get_incoming_edges`,
//! `get_outgoing_edges_with_label`) gate on node existence *as seen by the
//! transaction's snapshot*, not by current storage. These tests pin that
//! contract under concurrency:
//!
//! - a node committed AFTER a read transaction's snapshot is not visible to
//!   it, so edge listing returns `Err(NodeNotFound)` even though the node
//!   exists in current storage;
//! - a node visible AT snapshot time that a later transaction deletes is
//!   still visible to the old read transaction (historical fallback), so
//!   edge listing returns `Ok`.

use aletheiadb::api::transaction::{ReadOps, WriteOps};
use aletheiadb::{AletheiaDB, Error, PropertyMapBuilder, StorageError};

#[test]
fn read_tx_edge_listing_errors_for_node_committed_after_snapshot() {
    let db = AletheiaDB::new().unwrap();

    // Snapshot taken BEFORE the node exists.
    let read_tx = db.read_transaction().unwrap();

    // A concurrent transaction commits a new node after the snapshot.
    let node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // The node exists in current storage but is not visible at the old
    // snapshot: the existence gate must report NodeNotFound.
    let result = read_tx.get_outgoing_edges(node);
    assert!(
        matches!(
            result,
            Err(Error::Storage(StorageError::NodeNotFound(id))) if id == node
        ),
        "node committed after the snapshot must not be visible, got {result:?}"
    );
    assert!(read_tx.get_incoming_edges(node).is_err());
    assert!(
        read_tx
            .get_outgoing_edges_with_label(node, "KNOWS")
            .is_err()
    );

    // A fresh transaction (snapshot after the commit) sees it.
    let fresh_tx = db.read_transaction().unwrap();
    assert!(
        fresh_tx.get_outgoing_edges(node).unwrap().is_empty(),
        "a fresh snapshot must see the committed node (Ok(empty))"
    );
}

#[test]
fn read_tx_edge_listing_ok_for_node_deleted_after_snapshot() {
    let db = AletheiaDB::new().unwrap();

    let node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Snapshot taken while the node is alive.
    let read_tx = db.read_transaction().unwrap();
    assert!(read_tx.get_node(node).is_ok());

    // A later transaction commits a delete.
    db.write(|tx| tx.delete_node(node)).unwrap();

    // The old snapshot still sees the node (historical fallback), so edge
    // listing is Ok — not NodeNotFound.
    let outgoing = read_tx.get_outgoing_edges(node);
    assert!(
        outgoing.is_ok(),
        "node visible at snapshot time (deleted later) must stay visible \
         via the historical fallback, got {outgoing:?}"
    );
    assert!(outgoing.unwrap().is_empty());
    assert!(read_tx.get_incoming_edges(node).is_ok());
    assert!(read_tx.get_outgoing_edges_with_label(node, "KNOWS").is_ok());

    // A fresh transaction (snapshot after the delete) reports NodeNotFound.
    let fresh_tx = db.read_transaction().unwrap();
    assert!(
        matches!(
            fresh_tx.get_outgoing_edges(node),
            Err(Error::Storage(StorageError::NodeNotFound(id))) if id == node
        ),
        "a fresh snapshot must not see the deleted node"
    );
}
