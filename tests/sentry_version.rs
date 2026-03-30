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

#[test]
fn test_entity_version_methods_exhaustive_mutants() {
    let mut node = NodeVersion::new_anchor(
        VersionId::new(42).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Node").unwrap(),
        PropertyMapBuilder::new().build(),
    );

    // Default implementations would return false, None, or 0/Default::default()
    assert!(node.is_anchor());
    assert!(!node.is_delta());
    assert_eq!(node.version_id().as_u64(), 42);
    assert_ne!(node.version_id().as_u64(), 0);

    node.set_prev_version(Some(VersionId::new(1).unwrap()));
    assert_eq!(node.prev_version().unwrap().as_u64(), 1);

    node.set_next_version(Some(VersionId::new(2).unwrap()));
    assert_eq!(node.next_version().unwrap().as_u64(), 2);

    // Test EdgeVersion
    let mut edge = EdgeVersion::new_anchor(
        VersionId::new(42).unwrap(),
        EdgeId::new(200).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Edge").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
    );

    assert!(edge.is_anchor());
    assert!(!edge.is_delta());
    assert_eq!(edge.version_id().as_u64(), 42);
    assert_ne!(edge.version_id().as_u64(), 0);

    edge.set_prev_version(Some(VersionId::new(1).unwrap()));
    assert_eq!(edge.prev_version().unwrap().as_u64(), 1);

    edge.set_next_version(Some(VersionId::new(2).unwrap()));
    assert_eq!(edge.next_version().unwrap().as_u64(), 2);
}

#[test]
fn test_vector_delta_partial_eq_exhaustive() {
    use aletheiadb::core::version::VectorDelta;
    let delta1 = VectorDelta::Sparse {
        dimension: 10,
        changes: std::sync::Arc::new(vec![(1, 1.0), (2, 2.0)]),
    };

    let delta2 = VectorDelta::Sparse {
        dimension: 10,
        changes: std::sync::Arc::new(vec![(1, 1.0), (2, 2.0)]),
    };

    assert_eq!(delta1, delta2);
}

#[test]
fn test_property_delta_is_empty_exhaustive() {
    let empty = PropertyDelta::default();
    assert!(empty.is_empty());

    let mut changed = PropertyDelta::default();
    changed.changed.insert(
        GLOBAL_INTERNER.intern("a").unwrap(),
        std::convert::From::from(1i64),
    );
    assert!(!changed.is_empty());

    let mut removed = PropertyDelta::default();
    removed.removed.insert(GLOBAL_INTERNER.intern("a").unwrap());
    assert!(!removed.is_empty());

    let mut vector = PropertyDelta::default();
    vector.vector_deltas.insert(
        GLOBAL_INTERNER.intern("v").unwrap(),
        aletheiadb::core::version::VectorDelta::Sparse {
            dimension: 10,
            changes: std::sync::Arc::new(vec![(1, 1.0)]),
        },
    );
    assert!(!vector.is_empty());
}

#[test]
fn test_temporal_version_close_transaction_time_exhaustive() {
    use aletheiadb::core::version::TemporalVersion;
    let mut node = NodeVersion::new_anchor(
        VersionId::new(10).unwrap(),
        NodeId::new(100).unwrap(),
        BiTemporalInterval::current(1000.into()),
        GLOBAL_INTERNER.intern("Node").unwrap(),
        PropertyMapBuilder::new().build(),
    );

    let res = node.close_transaction_time(2000.into());
    assert!(res.is_ok());
    assert_eq!(node.temporal().transaction_time().end(), 2000.into());
}

#[test]
fn test_property_delta_estimated_heap_size_exhaustive() {
    let mut delta = PropertyDelta::default();
    assert_eq!(delta.estimated_heap_size(), 0);

    delta.changed.insert(
        GLOBAL_INTERNER.intern("a").unwrap(),
        std::convert::From::from(1i64),
    );
    delta.removed.insert(GLOBAL_INTERNER.intern("b").unwrap());
    delta.vector_deltas.insert(
        GLOBAL_INTERNER.intern("v").unwrap(),
        aletheiadb::core::version::VectorDelta::Sparse {
            dimension: 10,
            changes: std::sync::Arc::new(vec![(1, 1.0)]),
        },
    );

    let size = delta.estimated_heap_size();
    assert!(size > 0);
    // Explicit positive size tests kill returning 0/1, and +/-/*// operator mutants since they corrupt bounds
    assert_eq!(
        size,
        12 + // changed
        4 + // removed
        60 // vector_deltas (size of VectorDelta + allocations)
    );
}

#[test]
fn test_version_data_estimated_heap_size_exhaustive() {
    let anchor = VersionData::anchor(PropertyMapBuilder::new().insert("a", 1).build());
    let anchor_size = anchor.estimated_heap_size();
    assert!(anchor_size > 0);

    let delta = VersionData::delta_from_diff(
        &PropertyMapBuilder::new().build(),
        &PropertyMapBuilder::new().insert("a", 1).build(),
    );
    let delta_size = delta.estimated_heap_size();
    assert!(delta_size > 0);
}

#[test]
fn test_vector_delta_partial_eq_exhaustive_mutants() {
    use aletheiadb::core::version::VectorDelta;
    use std::sync::Arc;
    let sparse1 = VectorDelta::Sparse {
        dimension: 10,
        changes: Arc::new(vec![(1, 1.0), (2, 2.0)]),
    };

    let sparse2 = VectorDelta::Sparse {
        dimension: 10,
        changes: Arc::new(vec![(1, 1.0), (2, 2.0)]),
    };

    assert_eq!(sparse1, sparse2);

    // Mismatched dimension (kills != -> ==)
    let sparse_dim = VectorDelta::Sparse {
        dimension: 11,
        changes: Arc::new(vec![(1, 1.0), (2, 2.0)]),
    };
    assert_ne!(sparse1, sparse_dim);

    // Mismatched changes len (kills != -> ==)
    let sparse_len = VectorDelta::Sparse {
        dimension: 10,
        changes: Arc::new(vec![(1, 1.0)]),
    };
    assert_ne!(sparse1, sparse_len);

    // Mismatched idx (kills != -> ==, || -> &&)
    let sparse_idx = VectorDelta::Sparse {
        dimension: 10,
        changes: Arc::new(vec![(3, 1.0), (2, 2.0)]),
    };
    assert_ne!(sparse1, sparse_idx);

    // Mismatched val (kills ! -> delete)
    let sparse_val = VectorDelta::Sparse {
        dimension: 10,
        changes: Arc::new(vec![(1, 1.0), (2, 3.0)]), // difference > epsilon
    };
    assert_ne!(sparse1, sparse_val);
}
