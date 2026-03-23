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
fn test_property_delta_from_diff_exact_mutants() {
    use aletheiadb::core::property::PropertyMapBuilder;

    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let new = PropertyMapBuilder::new()
        .insert("name", "Bob")
        .insert("age", 30i64)
        .build();

    let delta = PropertyDelta::from_diff(&base, &new);

    assert!(
        !delta.is_empty(),
        "from_diff should not return default (empty)"
    );

    // == vs != on reference equality (old == new)
    let delta_same = PropertyDelta::from_diff(&base, &base);
    assert!(delta_same.is_empty(), "Same reference should return empty");
}

#[test]
fn test_property_delta_apply_exact_mutants() {
    use aletheiadb::core::property::{PropertyMapBuilder, PropertyValue};

    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let mut delta = PropertyDelta::new();
    let key_age = aletheiadb::core::interning::GLOBAL_INTERNER
        .intern("age")
        .unwrap();
    delta.changed.insert(key_age, PropertyValue::Int(31));

    // test `!self.removed.contains(key)`
    // if removed ! -> deleted, it skips inserting non-removed.
    let applied = delta.apply(&base);
    assert_eq!(applied.get("name").unwrap().as_str(), Some("Alice"));
    assert_eq!(applied.get("age").unwrap().as_int(), Some(31));
    assert_eq!(applied.len(), 2);
}

use aletheiadb::core::version::VectorDelta;

#[test]
fn test_property_delta_is_empty_exact_mutants() {
    use aletheiadb::core::property::PropertyValue;

    let mut delta = PropertyDelta::new();
    let key = aletheiadb::core::interning::GLOBAL_INTERNER
        .intern("key")
        .unwrap();

    delta.changed.insert(key, PropertyValue::Int(1));
    // if && was mutated to ||, checking changed.is_empty() (false) || ... would return false,
    // but if it's evaluated differently, we want strict bounds.
    assert!(!delta.is_empty());

    let mut delta2 = PropertyDelta::new();
    delta2.removed.insert(key);
    assert!(!delta2.is_empty());

    let mut delta3 = PropertyDelta::new();
    delta3
        .vector_deltas
        .insert(key, VectorDelta::Full(std::sync::Arc::from(vec![1.0f32])));
    assert!(!delta3.is_empty());
}

#[test]
fn test_vector_delta_apply_exact_mutants() {
    let delta = VectorDelta::Full(std::sync::Arc::from(vec![1.0f32]));
    // test replace VectorDelta::apply with vec![]
    let res = delta.apply(&[0.0]);
    assert_eq!(res, vec![1.0f32]);
}

#[test]
fn test_vector_delta_partial_eq_exact_mutants() {
    let v1 = VectorDelta::Full(std::sync::Arc::from(vec![1.0f32, 2.0]));
    let v2 = VectorDelta::Full(std::sync::Arc::from(vec![1.0f32, 3.0]));
    assert_ne!(v1, v2);

    let v3 = VectorDelta::Full(std::sync::Arc::from(vec![1.0f32]));
    assert_ne!(v1, v3);

    let sparse1 = VectorDelta::Sparse {
        dimension: 2,
        changes: std::sync::Arc::new(vec![(0, 1.0)]),
    };
    let sparse2 = VectorDelta::Sparse {
        dimension: 3,
        changes: std::sync::Arc::new(vec![(0, 1.0)]),
    };
    let sparse3 = VectorDelta::Sparse {
        dimension: 2,
        changes: std::sync::Arc::new(vec![(0, 1.0), (1, 2.0)]),
    };
    let sparse4 = VectorDelta::Sparse {
        dimension: 2,
        changes: std::sync::Arc::new(vec![(1, 1.0)]),
    };
    let sparse5 = VectorDelta::Sparse {
        dimension: 2,
        changes: std::sync::Arc::new(vec![(0, 2.0)]),
    };

    assert_ne!(sparse1, sparse2, "dimension mismatch");
    assert_ne!(sparse1, sparse3, "length mismatch");
    assert_ne!(sparse1, sparse4, "index mismatch");
    assert_ne!(sparse1, sparse5, "value mismatch");
}

#[test]
fn test_vector_delta_from_diff_exact_mutants() {
    // Need at least 3 elements where 1 changes to use sparse
    // (changes.len() * 2 < dimension -> 1 * 2 < 3 -> 2 < 3 is true)
    let old = vec![1.0f32, 2.0, 3.0];
    let new = vec![1.0f32, 2.0, 4.0];
    let diff = VectorDelta::from_diff(&old, &new).unwrap();
    // Test that the diff is NOT None (from replace with None)
    assert!(matches!(diff, VectorDelta::Sparse { .. }));

    // Test different lengths (from != to ==)
    let old2 = vec![1.0f32];
    assert!(VectorDelta::from_diff(&old2, &new).is_none());

    // > with >=
    // MAX_VECTOR_DIMENSIONS
    use aletheiadb::core::property::MAX_VECTOR_DIMENSIONS;
    let old_max = vec![0.0f32; MAX_VECTOR_DIMENSIONS];
    let mut new_max = old_max.clone();
    new_max[0] = 1.0;
    assert!(VectorDelta::from_diff(&old_max, &new_max).is_some());
}
