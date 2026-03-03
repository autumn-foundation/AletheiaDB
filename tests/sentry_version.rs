// tests/sentry_version.rs

use aletheiadb::core::id::{EdgeId, NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::BiTemporalInterval;
use aletheiadb::core::version::{
    EdgeVersion, EntityVersion, NodeVersion, PropertyDelta, VersionData,
};

#[test]
fn test_property_delta_from_diff_not_default() {
    let old = PropertyMapBuilder::new().insert("a", 1).build();
    let new = PropertyMapBuilder::new().insert("a", 2).build();
    let delta = PropertyDelta::from_diff(&old, &new);
    assert!(
        !delta.is_empty(),
        "PropertyDelta::from_diff should not return default (empty) delta for changes"
    );
}

#[test]
fn test_property_delta_from_diff_returns_empty_when_identical() {
    // Covers the fast path: if old == new { return PropertyDelta::new(); }
    // If removed, it will still work correctly but be slower. However, some mutants might
    // change the behavior or return different things.
    let old = PropertyMapBuilder::new().insert("a", 1).build();
    // Using identical arc pointers
    let new = old.clone();
    let delta = PropertyDelta::from_diff(&old, &new);
    assert!(delta.is_empty(), "Fast path should return empty delta");
}

#[test]
fn test_property_delta_semantically_equal_guard() {
    // Tests: replace match guard old_value.semantically_equal(new_value) with true/false
    let old = PropertyMapBuilder::new().insert("a", 1).build();
    let new = PropertyMapBuilder::new().insert("a", 1).build();

    // If replaced with false, it would treat identical values as changed
    let delta = PropertyDelta::from_diff(&old, &new);
    assert!(
        delta.is_empty(),
        "Identical values should not produce a delta"
    );

    let old = PropertyMapBuilder::new().insert("a", 1).build();
    let new = PropertyMapBuilder::new().insert("a", 2).build();

    // If replaced with true, it would treat different values as unchanged
    let delta = PropertyDelta::from_diff(&old, &new);
    assert!(!delta.is_empty(), "Different values must produce a delta");
}

#[test]
fn test_vector_delta_match_arm_removal() {
    // Tests: delete match arm (Some(old_vec), Some(new_vec))
    let v1 = vec![0.0f32; 10];
    let mut v2 = v1.clone();
    v2[0] = 1.0;

    let old = PropertyMapBuilder::new().insert_vector("vec", &v1).build();
    let new = PropertyMapBuilder::new().insert_vector("vec", &v2).build();

    let delta = PropertyDelta::from_diff(&old, &new);

    // If the vector optimization arm is removed, it falls back to full replacement in `changed`
    // We want to ensure we are using the optimized path (in `vector_deltas`)
    assert!(
        delta.changed.is_empty(),
        "Should use sparse optimization, not full replacement"
    );
    assert!(!delta.vector_deltas.is_empty(), "Should have vector delta");
}

#[test]
fn test_property_delta_dimension_mismatch_logic() {
    // Tests: replace != with == in PropertyDelta::from_diff (for dimension check)
    let v1 = vec![0.0f32; 10];
    let v2 = vec![0.0f32; 11]; // Different dimension

    let old = PropertyMapBuilder::new().insert_vector("vec", &v1).build();
    let new = PropertyMapBuilder::new().insert_vector("vec", &v2).build();

    let delta = PropertyDelta::from_diff(&old, &new);

    // Should be in `changed` as a full replacement because dimensions differ
    assert!(
        !delta.changed.is_empty(),
        "Dimension mismatch should cause full update"
    );
    assert!(
        delta.vector_deltas.is_empty(),
        "Dimension mismatch should not be a vector delta"
    );
}

#[test]
fn test_property_delta_removed_logic() {
    // Tests: delete ! in PropertyDelta::from_diff (in !new.contains_interned_key(key))
    let old = PropertyMapBuilder::new().insert("a", 1).build();
    let new = PropertyMapBuilder::new().build(); // "a" removed

    let delta = PropertyDelta::from_diff(&old, &new);
    assert!(
        !delta.removed.is_empty(),
        "Removed property must be tracked"
    );

    let old = PropertyMapBuilder::new().insert("a", 1).build();
    let new = PropertyMapBuilder::new().insert("a", 1).build(); // "a" present

    let delta = PropertyDelta::from_diff(&old, &new);
    assert!(
        delta.removed.is_empty(),
        "Present property must not be tracked as removed"
    );
}

#[test]
fn test_version_data_get_vector_snapshot_id_mutants() {
    // Tests: replace VersionData::get_vector_snapshot_id -> Option<usize> with None / Some(0) / Some(1)
    // And: delete match arm VersionData::Anchor{vector_snapshot_id, ..}

    // Case 1: Anchor with ID
    let mut anchor = VersionData::anchor(PropertyMapBuilder::new().build());
    anchor.set_vector_snapshot_id(42);
    assert_eq!(
        anchor.get_vector_snapshot_id(),
        Some(42),
        "Must return set ID"
    );

    // Case 2: Anchor without ID
    let anchor_none = VersionData::anchor(PropertyMapBuilder::new().build());
    assert_eq!(
        anchor_none.get_vector_snapshot_id(),
        None,
        "Must return None if not set"
    );

    // Case 3: Delta (always None)
    let delta = VersionData::delta_from_diff(
        &PropertyMapBuilder::new().build(),
        &PropertyMapBuilder::new().build(),
    );
    assert_eq!(
        delta.get_vector_snapshot_id(),
        None,
        "Delta must return None"
    );
}

#[test]
fn test_entity_version_methods_not_default() {
    // Tests replacements with Default::default() or fixed values for EntityVersion trait impls

    let node = NodeVersion::new_anchor(
        VersionId::new(10).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Node").unwrap(),
        PropertyMapBuilder::new().build(),
    );

    assert_eq!(node.version_id(), VersionId::new(10).unwrap());
    assert!(node.is_anchor());
    assert!(!node.is_delta()); // Implicitly tests is_delta mutants via EntityVersion if exposed, but mostly NodeVersion direct methods

    let edge = EdgeVersion::new_anchor(
        VersionId::new(20).unwrap(),
        EdgeId::new(200).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Edge").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
    );

    assert_eq!(edge.version_id(), VersionId::new(20).unwrap());
    assert!(edge.is_anchor());
}
