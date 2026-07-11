use super::*;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;

#[test]
fn test_node_version_anchor_has_embedded_commit_timestamp() {
    // NodeVersion must expose commit_timestamp directly (HyPer/TiDB pattern).
    // This enables visibility checks without a separate committed-map lookup.
    let commit_ts: Timestamp = 500.into();
    let temporal = BiTemporalInterval::now(100.into(), commit_ts);
    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(1).unwrap(),
        temporal,
        GLOBAL_INTERNER.intern("TestLabel").unwrap(),
        PropertyMapBuilder::new().build(),
    );
    assert_eq!(
        version.commit_timestamp, commit_ts,
        "NodeVersion must embed commit_timestamp equal to transaction_time start"
    );
}

#[test]
fn test_node_version_delta_has_embedded_commit_timestamp() {
    let first_ts: Timestamp = 100.into();
    let second_ts: Timestamp = 200.into();
    let node_id = NodeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();
    let old_props = PropertyMapBuilder::new().insert("x", 1i64).build();
    let new_props = PropertyMapBuilder::new().insert("x", 2i64).build();

    let delta = NodeVersion::new_delta(
        VersionId::new(2).unwrap(),
        node_id,
        BiTemporalInterval::now(50.into(), second_ts),
        label,
        &old_props,
        &new_props,
        VersionId::new(1).unwrap(),
    );
    assert_eq!(
        delta.commit_timestamp, second_ts,
        "NodeVersion delta must embed commit_timestamp from transaction_time start"
    );
    // Earlier anchor at first_ts should still have its own commit_timestamp
    let anchor = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        node_id,
        BiTemporalInterval::now(50.into(), first_ts),
        label,
        old_props,
    );
    assert_eq!(anchor.commit_timestamp, first_ts);
}

#[test]
fn test_edge_version_anchor_has_embedded_commit_timestamp() {
    let commit_ts: Timestamp = 750.into();
    let temporal = BiTemporalInterval::now(200.into(), commit_ts);
    let version = EdgeVersion::new_anchor(
        VersionId::new(10).unwrap(),
        EdgeId::new(5).unwrap(),
        temporal,
        GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
    );
    assert_eq!(
        version.commit_timestamp, commit_ts,
        "EdgeVersion must embed commit_timestamp equal to transaction_time start"
    );
}

#[test]
fn test_edge_version_delta_has_embedded_commit_timestamp() {
    let commit_ts: Timestamp = 900.into();
    let edge_id = EdgeId::new(5).unwrap();
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let old_props = PropertyMapBuilder::new().insert("w", 1i64).build();
    let new_props = PropertyMapBuilder::new().insert("w", 2i64).build();

    let delta = EdgeVersion::new_delta(
        VersionId::new(11).unwrap(),
        edge_id,
        BiTemporalInterval::now(200.into(), commit_ts),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        &old_props,
        &new_props,
        VersionId::new(10).unwrap(),
    );
    assert_eq!(
        delta.commit_timestamp, commit_ts,
        "EdgeVersion delta must embed commit_timestamp from transaction_time start"
    );
}

#[test]
fn test_commit_timestamp_enables_direct_visibility_check() {
    // The core HyPer/TiDB pattern: given a version and a snapshot timestamp,
    // visibility can be determined with a single comparison — no map lookup.
    let commit_ts: Timestamp = 50.into();
    let snapshot_ts: Timestamp = 100.into();
    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(1).unwrap(),
        BiTemporalInterval::now(10.into(), commit_ts),
        GLOBAL_INTERNER.intern("N").unwrap(),
        PropertyMapBuilder::new().build(),
    );
    // Committed before snapshot → visible
    assert!(version.commit_timestamp < snapshot_ts);
    // Committed after snapshot → not visible
    let late_version = NodeVersion::new_anchor(
        VersionId::new(2).unwrap(),
        NodeId::new(1).unwrap(),
        BiTemporalInterval::now(10.into(), 150.into()),
        GLOBAL_INTERNER.intern("N").unwrap(),
        PropertyMapBuilder::new().build(),
    );
    assert!(late_version.commit_timestamp > snapshot_ts);
}
