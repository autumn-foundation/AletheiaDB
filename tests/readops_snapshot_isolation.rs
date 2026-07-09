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

/// Regression for the macOS-only trunk CI failure: a read snapshot and a
/// subsequent delete commit that land in the *same wallclock tick* must not
/// break snapshot isolation.
///
/// Root cause: `snapshot_timestamp_for_read` peeked the HLC frontier to compute
/// a snapshot `S` but never reserved it. When the delete commit happened in the
/// same tick, its `send()` recomputed the identical stamp `S`, closing the
/// superseded version's transaction-time interval at exactly `[C1, S)`. The
/// half-open upper bound (`TimeRange::contains` uses `< end`) then excluded the
/// snapshot `S`, so the historical fallback returned `NodeNotFound` — a genuine
/// snapshot-isolation violation reachable in production whenever a read snapshot
/// and a later commit share a wallclock tick.
///
/// A frozen [`SimulatedClock`] makes the single-tick collision deterministic
/// (on a normally-advancing clock it is a rare race). The clock is injected
/// *before* the database is constructed so the startup frontier is seeded at the
/// frozen instant and every subsequent timestamp differs only in its logical
/// counter, guaranteeing the collision.
#[cfg(feature = "simulation")]
#[test]
fn read_tx_edge_listing_ok_for_node_deleted_in_same_clock_tick() {
    use aletheiadb::simulation::SimulatedClock;

    // Freeze the clock so the read snapshot and the delete commit share a tick.
    let clock = SimulatedClock::new(1_700_000_000_000_000);
    let _guard = clock.inject();

    let db = AletheiaDB::new().unwrap();

    let node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Snapshot taken while the node is alive (same frozen tick).
    let read_tx = db.read_transaction().unwrap();
    assert!(read_tx.get_node(node).is_ok());

    // A later transaction commits a delete — in the SAME wallclock tick.
    db.write(|tx| tx.delete_node(node)).unwrap();

    // The old snapshot must still see the node via the historical fallback.
    // Before the fix the snapshot stamp collided with the delete's commit stamp,
    // so the superseded version's tx-interval excluded the snapshot →
    // NodeNotFound. Reserving the snapshot tick sorts the delete strictly after
    // the read, restoring visibility.
    let outgoing = read_tx.get_outgoing_edges(node);
    assert!(
        outgoing.is_ok(),
        "node visible at snapshot time must stay visible even when the delete \
         commits in the same wallclock tick, got {outgoing:?}"
    );
    assert!(outgoing.unwrap().is_empty());
    assert!(read_tx.get_incoming_edges(node).is_ok());
    assert!(read_tx.get_outgoing_edges_with_label(node, "KNOWS").is_ok());
}
