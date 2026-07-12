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

    // Pin the historical fallback directly: get_node must still resolve the
    // pre-delete version at the old snapshot even after the same-tick delete.
    assert!(
        read_tx.get_node(node).is_ok(),
        "get_node must resolve the pre-delete version via the historical \
         fallback after a same-tick delete, got {:?}",
        read_tx.get_node(node)
    );
}

/// Companion to the same-tick delete test for the *superseded-not-deleted* path:
/// a node **updated** in the same clock tick as the read snapshot must remain
/// visible to that snapshot as its pre-update version. An update leaves a new
/// (updated) version in current storage whose commit stamp equals the snapshot
/// under the pre-fix collision, so the visibility check falls through to the
/// historical fallback — which, before the reservation fix, closed the
/// pre-update version's tx-interval at `[C1, S)` and excluded `S`, yielding
/// `NodeNotFound`. Reserving the snapshot tick keeps the pre-update version
/// visible.
#[cfg(feature = "simulation")]
#[test]
fn read_tx_sees_pre_update_version_for_node_updated_in_same_clock_tick() {
    use aletheiadb::simulation::SimulatedClock;

    let clock = SimulatedClock::new(1_700_000_000_000_000);
    let _guard = clock.inject();

    let db = AletheiaDB::new().unwrap();

    let node = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("v", 1_i64).build(),
        )
        .unwrap();

    // Snapshot taken while the pre-update version is current (same frozen tick).
    let read_tx = db.read_transaction().unwrap();
    let before = read_tx.get_node(node).unwrap();
    assert_eq!(before.properties.get("v").and_then(|p| p.as_int()), Some(1));

    // A later transaction updates the node — in the SAME wallclock tick.
    db.write(|tx| tx.update_node(node, PropertyMapBuilder::new().insert("v", 2_i64).build()))
        .unwrap();

    // The old snapshot must still resolve the pre-update version (v == 1) via
    // the historical fallback, not error out with NodeNotFound.
    let after = read_tx.get_node(node);
    assert!(
        after.is_ok(),
        "node updated in the same wallclock tick must stay visible as its \
         pre-update version, got {after:?}"
    );
    assert_eq!(
        after.unwrap().properties.get("v").and_then(|p| p.as_int()),
        Some(1),
        "the old snapshot must observe the pre-update value, not the update"
    );
    // Edge-listing existence gates must likewise still see the node.
    assert!(read_tx.get_outgoing_edges(node).is_ok());
    assert!(read_tx.get_incoming_edges(node).is_ok());
    assert!(read_tx.get_outgoing_edges_with_label(node, "KNOWS").is_ok());
}

/// Edge analogue of the same-tick delete test: an **edge** deleted in the same
/// clock tick as the read snapshot must stay readable via `get_edge`'s
/// historical fallback (`get_edge_from_historical`), which uses the identical
/// half-open `[C1, end)` logic. Before the reservation fix the delete's commit
/// stamp collided with the snapshot `S`, closing the edge version at `[C1, S)`
/// and excluding `S` → `EdgeNotFound`.
#[cfg(feature = "simulation")]
#[test]
fn read_tx_edge_readable_for_edge_deleted_in_same_clock_tick() {
    use aletheiadb::simulation::SimulatedClock;

    let clock = SimulatedClock::new(1_700_000_000_000_000);
    let _guard = clock.inject();

    let db = AletheiaDB::new().unwrap();

    let a = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let b = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let edge = db
        .create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Snapshot taken while the edge is alive (same frozen tick).
    let read_tx = db.read_transaction().unwrap();
    assert!(read_tx.get_edge(edge).is_ok());

    // A later transaction deletes the edge — in the SAME wallclock tick.
    db.write(|tx| tx.delete_edge(edge)).unwrap();

    // The old snapshot must still resolve the edge via the historical fallback.
    let got = read_tx.get_edge(edge);
    assert!(
        got.is_ok(),
        "edge visible at snapshot time must stay readable even when the delete \
         commits in the same wallclock tick, got {got:?}"
    );

    // A fresh transaction (snapshot after the delete) must not see the edge.
    let fresh_tx = db.read_transaction().unwrap();
    assert!(
        matches!(
            fresh_tx.get_edge(edge),
            Err(Error::Storage(StorageError::EdgeNotFound(id))) if id == edge
        ),
        "a fresh snapshot must not see the deleted edge"
    );
}

/// Issue #3504 regression (delete): a node deleted in the SAME wallclock tick as
/// an existing read snapshot must stay visible to that snapshot on the *valid*
/// dimension. `close_previous_version_intervals` used to close the superseded
/// version's valid-time interval in place at the delete's `valid_from` whenever
/// `new.valid_from > prev.valid_from`. Because the version chain is a
/// transaction-time partition (the tx-time close already hides the version from
/// current-state reads), that in-place valid-close was never needed for
/// correctness -- but it retroactively excluded a snapshot whose valid
/// coordinate equalled the delete's `valid_from`, yielding `NodeNotFound` for a
/// node that was alive at snapshot time: a snapshot-isolation violation on the
/// valid dimension (the residual half of #3435 that #3437 did not cover).
///
/// Frozen `SimulatedClock`: the snapshot is taken at `T0+1` (if-branch of the
/// visibility check, `S=(T0+1, 0)`), and the delete commits at `T0+1` with its
/// default `valid_from = T0+1 > T0` (the node's creation valid time), so the
/// pre-fix code closed the node version to `[T0, T0+1)` and excluded `S`.
#[cfg(feature = "simulation")]
#[test]
fn read_tx_sees_node_when_delete_valid_time_collides_with_snapshot() {
    use aletheiadb::simulation::SimulatedClock;

    let mut clock = SimulatedClock::new(1_700_000_000_000_000);
    let _guard = clock.inject();

    let db = AletheiaDB::new().unwrap();

    // Created at the frozen instant T0 (valid_from == T0).
    let node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Advance one tick and take a snapshot S=(T0+1, 0) while the node is alive.
    clock.jump_to(1_700_000_000_000_001);
    let read_tx = db.read_transaction().unwrap();
    assert!(
        read_tx.get_node(node).is_ok(),
        "snapshot taken at T0+1 must see the node that was created at T0"
    );

    // Delete in the same tick (T0+1): its default valid_from (T0+1) is strictly
    // greater than the node's creation valid_from (T0), triggering the buggy
    // in-place valid-close of the superseded version.
    db.write(|tx| tx.delete_node(node)).unwrap();

    // The old snapshot must STILL see the pre-delete node on the valid
    // dimension. Before the #3504 fix these all returned NodeNotFound because
    // the superseded version's valid interval was retroactively closed at T0+1,
    // excluding the snapshot's valid coordinate.
    let outgoing = read_tx.get_outgoing_edges(node);
    assert!(
        outgoing.is_ok(),
        "node alive at snapshot time must stay visible even when a delete \
         commits with valid_from equal to the snapshot's valid coordinate, \
         got {outgoing:?}"
    );
    assert!(outgoing.unwrap().is_empty());
    assert!(
        read_tx.get_incoming_edges(node).is_ok(),
        "incoming-edge existence gate must still see the pre-delete node"
    );
    assert!(
        read_tx.get_node(node).is_ok(),
        "get_node must resolve the pre-delete version at the old snapshot, \
         got {:?}",
        read_tx.get_node(node)
    );

    // A fresh snapshot taken after the delete must NOT see the node.
    let fresh_tx = db.read_transaction().unwrap();
    assert!(
        matches!(
            fresh_tx.get_outgoing_edges(node),
            Err(Error::Storage(StorageError::NodeNotFound(id))) if id == node
        ),
        "a fresh snapshot after the delete must not see the node"
    );
}

/// Issue #3504 regression (backdated update): a node whose UPDATE carries an
/// explicit `valid_from` that is later than the node's creation valid time but
/// still at/before an existing read snapshot's valid coordinate must not vanish
/// from that snapshot. This is the case the sibling
/// `read_tx_sees_pre_update_version_for_node_updated_in_same_clock_tick` does
/// NOT cover: there the update's `valid_from` equals the creation valid time
/// (so `new.valid_from > prev.valid_from` is false and the buggy valid-close
/// never fires). Here the update is backdated to a value strictly between the
/// creation time and the snapshot, so the pre-fix in-place valid-close truncated
/// the pre-update version's valid interval and excluded the snapshot.
///
/// Timeline (frozen `SimulatedClock`, T0 == initial):
///   * create A (v == 1) at valid_from T0;
///   * jump to T0+2, take snapshot S=(T0+2, 0) -- sees A;
///   * update to B (v == 2) with backdated valid_from T0+1 (T0 < T0+1 < T0+2),
///     committing at tx == T0+2. Pre-fix this closed A's valid interval to
///     `[T0, T0+1)`, excluding the snapshot's valid coordinate T0+2 -> the old
///     snapshot got NodeNotFound.
#[cfg(feature = "simulation")]
#[test]
fn read_tx_sees_pre_update_version_when_backdated_update_valid_time_precedes_snapshot() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::simulation::SimulatedClock;

    let mut clock = SimulatedClock::new(1_700_000_000_000_000);
    let _guard = clock.inject();

    let db = AletheiaDB::new().unwrap();

    // Version A (v == 1), created at the frozen instant T0 (valid_from == T0).
    let node = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("v", 1_i64).build(),
        )
        .unwrap();

    // Advance two ticks so a value strictly between T0 and the snapshot exists,
    // then take snapshot S=(T0+2, 0) while A is still current.
    clock.jump_to(1_700_000_000_000_002);
    let read_tx = db.read_transaction().unwrap();
    let before = read_tx.get_node(node).unwrap();
    assert_eq!(
        before.properties.get("v").and_then(|p| p.as_int()),
        Some(1),
        "snapshot must see the pre-update value A (v == 1)"
    );

    // Backdated update to B (v == 2) with valid_from == T0+1 (> creation T0,
    // < snapshot valid coordinate T0+2). Commits at tx == T0+2.
    let backdated = HybridTimestamp::new(1_700_000_000_000_001, 0).unwrap();
    db.update_node_with_valid_time(
        node,
        PropertyMapBuilder::new().insert("v", 2_i64).build(),
        Some(backdated),
    )
    .unwrap();

    // The old snapshot must still resolve the pre-update version A. Before the
    // #3504 fix this returned NodeNotFound because A's valid interval was
    // retroactively closed at T0+1, excluding the snapshot's valid coordinate.
    let after = read_tx.get_node(node);
    assert!(
        after.is_ok(),
        "a backdated update whose valid_from precedes an existing read \
         snapshot must not vanish the node from that snapshot, got {after:?}"
    );
    assert_eq!(
        after.unwrap().properties.get("v").and_then(|p| p.as_int()),
        Some(1),
        "the old snapshot must still observe the pre-update value A (v == 1)"
    );

    // A fresh snapshot taken after the update must see B (v == 2).
    let fresh_tx = db.read_transaction().unwrap();
    assert_eq!(
        fresh_tx
            .get_node(node)
            .unwrap()
            .properties
            .get("v")
            .and_then(|p| p.as_int()),
        Some(2),
        "a fresh snapshot after the update must observe the new value B (v == 2)"
    );
}
