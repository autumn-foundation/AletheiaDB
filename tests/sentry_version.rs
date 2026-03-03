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
fn test_vector_delta_from_diff_mutants() {
    use aletheiadb::core::version::VectorDelta;
    use std::sync::Arc;

    // same vector -> no diff
    assert_eq!(VectorDelta::from_diff(&[1.0, 2.0], &[1.0, 2.0]), None);

    // full replacement logic vs sparse logic
    let v1 = vec![0.0f32; 100];
    let mut v2 = v1.clone();
    v2[0] = 1.0;

    let delta = VectorDelta::from_diff(&v1, &v2).unwrap();
    if let VectorDelta::Sparse { dimension, changes } = delta {
        assert_eq!(dimension, 100);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0], (0, 1.0));
    } else {
        panic!("Expected sparse delta");
    }

    // lots of changes -> full replacement
    let mut v3 = v1.clone();
    for item in v3.iter_mut().take(50) {
        *item = 1.0;
    }
    let delta_full = VectorDelta::from_diff(&v1, &v3).unwrap();
    if let VectorDelta::Full(v) = delta_full {
        assert_eq!(v, Arc::from(v3.into_boxed_slice()));
    } else {
        panic!("Expected full delta");
    }
}

#[test]
fn test_vector_delta_apply_mutants() {
    use aletheiadb::core::version::VectorDelta;
    use std::sync::Arc;

    // Apply sparse to base
    let base = vec![0.0, 0.0, 0.0];
    let delta = VectorDelta::Sparse {
        dimension: 3,
        changes: Arc::new(vec![(1, 1.0)]),
    };
    // Assuming apply returns Vec<f32>, looking at mutant error output
    let new_vec = delta.apply(&base);
    assert_eq!(new_vec, vec![0.0, 1.0, 0.0]);

    // Apply full to base (should just return full)
    let full = VectorDelta::Full(Arc::from(vec![1.0, 1.0, 1.0].into_boxed_slice()));
    let new_vec2 = full.apply(&base);
    assert_eq!(new_vec2, vec![1.0, 1.0, 1.0]);
}

#[test]
fn test_vector_delta_apply_bounds() {
    use aletheiadb::core::version::VectorDelta;
    use std::sync::Arc;
    let base = vec![0.0, 0.0];
    let delta = VectorDelta::Sparse {
        dimension: 2,
        changes: Arc::new(vec![(5, 1.0)]),
    };
    let new_vec = delta.apply(&base);
    // Mutants change `< base.len()` to `<= base.len()`. If index is out of bounds,
    // it panics. We expect a panic when the out of bounds condition fails to prevent the indexing.
    // Let's actually check if it handles the out of bounds. The code says:
    // if index < new_vec.len() { new_vec[index] = val; }
    // If we pass an out of bounds index, it should simply ignore it or we should be able to see the length hasn't changed.
    assert_eq!(new_vec, vec![0.0, 0.0]); // should ignore index 5
}

#[test]
fn test_property_delta_apply_mutants() {
    use aletheiadb::core::property::PropertyMapBuilder;
    use aletheiadb::core::version::PropertyDelta;

    // Test PropertyDelta::apply logic

    // `apply` -> default check: ensure it doesn't return empty default on valid apply
    let base = PropertyMapBuilder::new()
        .insert("k1", 10)
        .insert("k2", 20)
        .build();
    let new = PropertyMapBuilder::new()
        .insert("k2", 30) // changed
        .insert("k3", 40) // added
        .build(); // "k1" removed

    let delta = PropertyDelta::from_diff(&base, &new);
    let applied = delta.apply(&base);

    // Check that we did not get a default (empty) map
    assert!(!applied.is_empty(), "Apply must not return an empty map");
    assert_eq!(applied.get("k2").unwrap().as_int(), Some(30));
    assert_eq!(applied.get("k3").unwrap().as_int(), Some(40));
    assert!(applied.get("k1").is_none());

    // Test `+` -> `-` / `*` logic in PropertyDelta::apply
    // Usually this is around `with_capacity(base.len() + self.changed.len())`
    // We can't easily detect the capacity directly through the public API,
    // but the implementation doesn't fail on wrong capacity.
    // We will just verify the applied map is fully correct.
    assert_eq!(applied.len(), 2);
}

#[test]
fn test_vector_delta_apply_replace() {
    use aletheiadb::core::version::VectorDelta;
    use std::sync::Arc;
    let base = vec![0.0, 0.0];
    let delta = VectorDelta::Sparse {
        dimension: 2,
        changes: Arc::new(vec![(0, 1.0)]),
    };

    // Some mutants replace `<` with `==` which would skip `index < new_vec.len()` if `index != new_vec.len()`
    // We check that valid indices are indeed updated
    let applied = delta.apply(&base);
    assert_eq!(applied, vec![1.0, 0.0]);
}

#[test]
fn test_property_delta_estimated_heap_size_mutants() {
    use aletheiadb::core::version::{PropertyDelta, VectorDelta};
    use std::sync::Arc;

    let delta_empty = PropertyDelta::default();
    assert_eq!(delta_empty.estimated_heap_size(), 0);

    let mut delta = PropertyDelta::default();

    // Using InternedString wrapper directly or interning a string string
    let k1 = aletheiadb::core::interning::GLOBAL_INTERNER
        .intern("k1")
        .unwrap();
    let k2 = aletheiadb::core::interning::GLOBAL_INTERNER
        .intern("k2")
        .unwrap();
    let k3 = aletheiadb::core::interning::GLOBAL_INTERNER
        .intern("k3")
        .unwrap();

    delta
        .changed
        .insert(k1, aletheiadb::PropertyValue::from(42));
    delta.removed.insert(k2);
    delta.vector_deltas.insert(
        k3,
        VectorDelta::Sparse {
            dimension: 10,
            changes: Arc::new(vec![(0, 1.0)]),
        },
    );

    let size = delta.estimated_heap_size();

    assert!(size > 0);
}
