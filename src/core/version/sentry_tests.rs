use super::*;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{MAX_VECTOR_DIMENSIONS, PropertyMapBuilder};
use std::sync::Arc;

#[test]
fn test_materialize_vector_deltas_missing_base_property() {
    let key = GLOBAL_INTERNER.intern("embedding").unwrap();
    let mut delta = PropertyDelta::new();

    // Manually insert a sparse delta that requires a base property
    // We use a dummy sparse delta
    let sparse_delta = VectorDelta::Sparse {
        dimension: 10,
        changes: std::sync::Arc::new(vec![]),
    };
    delta.vector_deltas.insert(key, sparse_delta);

    // Empty base map (missing the key)
    let base = PropertyMapBuilder::new().build();

    let result = delta.materialize_vector_deltas(&base);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("base property not found"));
}

#[test]
fn test_materialize_vector_deltas_wrong_base_type() {
    let key = GLOBAL_INTERNER.intern("embedding").unwrap();
    let mut delta = PropertyDelta::new();

    let sparse_delta = VectorDelta::Sparse {
        dimension: 10,
        changes: std::sync::Arc::new(vec![]),
    };
    delta.vector_deltas.insert(key, sparse_delta);

    // Base map has the key but it's an integer, not a vector
    let base = PropertyMapBuilder::new().insert("embedding", 42i64).build();

    let result = delta.materialize_vector_deltas(&base);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("base property is not a vector"));
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "VectorDelta applied to vector of wrong dimension")]
fn test_vector_delta_apply_dimension_mismatch_panic() {
    // Create a sparse delta expecting dimension 10
    let delta = VectorDelta::Sparse {
        dimension: 10,
        changes: std::sync::Arc::new(vec![]),
    };

    // Apply to a base vector of dimension 5 (mismatch)
    let base = vec![0.0f32; 5];
    let _ = delta.apply(&base);
}

#[test]
fn test_vector_delta_from_diff_max_dimensions() {
    // Create vectors exceeding the maximum allowed dimension
    let len = MAX_VECTOR_DIMENSIONS + 1;
    let v1 = vec![0.0f32; len];
    let v2 = vec![1.0f32; len];

    // Should return None due to dimension limit
    let result = VectorDelta::from_diff(&v1, &v2);
    assert!(result.is_none());
}

#[test]
fn test_vector_delta_from_diff_nan_change() {
    // 💣 Risk: (a - b).abs() > EPSILON returns false if one is NaN.
    // This means changes involving NaN are silently ignored!
    let old = vec![1.0f32];
    let new = vec![f32::NAN];
    let delta = VectorDelta::from_diff(&old, &new);
    assert!(delta.is_some(), "Change from 1.0 to NaN should be detected");

    let old = vec![f32::NAN];
    let new = vec![1.0f32];
    let delta = VectorDelta::from_diff(&old, &new);
    assert!(delta.is_some(), "Change from NaN to 1.0 should be detected");

    let old = vec![1.0f32];
    let new = vec![f32::INFINITY];
    let delta = VectorDelta::from_diff(&old, &new);
    assert!(
        delta.is_some(),
        "Change from 1.0 to Infinity should be detected"
    );
}

#[test]
fn test_vector_delta_apply_manual_construction_oob() {
    // 💣 Risk: Manual construction or deserialization could create invalid indices.
    // Apply should not panic or corrupt memory.
    let dimension = 10;
    let changes = std::sync::Arc::new(vec![(100, 1.0f32)]); // Index 100 > dimension 10
    let delta = VectorDelta::Sparse { dimension, changes };

    let base = vec![0.0f32; 10];
    let result = delta.apply(&base);

    // Should return base unchanged or with ignored OOB updates
    assert_eq!(result.len(), 10);
    assert_eq!(result[0], 0.0);
}

#[test]
fn test_property_delta_apply_full_vector_missing_base() {
    // 💣 Risk: Applying a delta containing a Full vector to a base that misses the key
    // should NOT fail silently. Since it's a full replacement, the base isn't needed!
    // Current implementation requires base property to exist even for Full delta.

    let base = PropertyMapBuilder::new().build(); // Empty base

    let mut delta = PropertyDelta::new();
    let key = GLOBAL_INTERNER.intern("embedding").unwrap();
    let new_vec = vec![1.0f32, 2.0, 3.0];
    let vec_delta = VectorDelta::Full(Arc::from(new_vec.clone()));

    delta.vector_deltas.insert(key, vec_delta);

    let result = delta.apply(&base);

    // Expectation: The vector should be present in the result
    assert!(
        result.get("embedding").is_some(),
        "Full vector delta should be applied even if base property is missing"
    );

    if let Some(val) = result.get("embedding") {
        assert_eq!(val.as_vector(), Some(new_vec.as_slice()));
    }
}

#[test]
fn test_vector_delta_vs_property_value_equality() {
    // 🧪 Strategy: Document the divergence between VectorDelta (approximate) and PropertyValue (exact).
    // VectorDelta ignores changes < epsilon and handles NaN differently than PropertyValue.

    // Case 1: Small difference (less than epsilon)
    // Use 0.0 as base to ensure the small difference is representable in f32
    // (1.0 + 1e-8 might be rounded to 1.0 due to f32 precision)
    let v1 = vec![0.0f32];
    let v2 = vec![VECTOR_EPSILON / 2.0];

    // PropertyValue uses exact equality (bitwise for f32 via PartialEq)
    let pv1 = PropertyValue::vector(&v1);
    let pv2 = PropertyValue::vector(&v2);
    assert_ne!(pv1, pv2, "PropertyValue should use exact equality");

    // VectorDelta uses approximate equality
    let delta = VectorDelta::from_diff(&v1, &v2);
    assert!(
        delta.is_none(),
        "VectorDelta should ignore changes smaller than epsilon"
    );

    // Case 2: NaN handling
    // PropertyValue: NaN != NaN
    let nan_vec = vec![f32::NAN];
    let pv_nan1 = PropertyValue::vector(&nan_vec);
    let pv_nan2 = PropertyValue::vector(&nan_vec);
    assert_ne!(pv_nan1, pv_nan2, "PropertyValue should treat NaN != NaN");

    // VectorDelta: NaN == NaN (treated as no change)
    let delta_nan = VectorDelta::from_diff(&nan_vec, &nan_vec);
    assert!(
        delta_nan.is_none(),
        "VectorDelta should treat NaN as equal to NaN (no change)"
    );

    // This confirms that PropertyDelta (which uses VectorDelta) might report "no change"
    // even when PropertyValue equality says they are different. This is a design choice
    // for storage efficiency but important to document.
}

#[test]
fn test_property_delta_apply_sparse_ignored_on_missing_base() {
    // 🧪 Strategy: Verify that a sparse delta is silently ignored if the base property is missing.
    // This is a known "best effort" behavior (fail open) rather than "fail closed".
    // Documenting it with a test ensures it doesn't change unexpectedly.

    let mut delta = PropertyDelta::new();
    let key = GLOBAL_INTERNER.intern("embedding").unwrap();

    // Sparse delta: change index 0 to 1.0
    let changes = Arc::new(vec![(0, 1.0f32)]);
    let vec_delta = VectorDelta::Sparse {
        dimension: 10,
        changes,
    };

    delta.vector_deltas.insert(key, vec_delta);

    // Base property map *without* "embedding"
    let base = PropertyMapBuilder::new().insert("name", "Alice").build();

    // Apply delta
    let result = delta.apply(&base);

    // Expectation: "embedding" is missing in result (delta ignored)
    assert!(
        result.get("embedding").is_none(),
        "Sparse delta should be silently ignored if base property is missing"
    );
}

#[test]
fn test_property_delta_apply_sparse_ignored_on_wrong_type() {
    // 🧪 Strategy: Verify that a sparse delta is silently ignored if the base property has wrong type.

    let mut delta = PropertyDelta::new();
    let key = GLOBAL_INTERNER.intern("embedding").unwrap();

    let changes = Arc::new(vec![(0, 1.0f32)]);
    let vec_delta = VectorDelta::Sparse {
        dimension: 10,
        changes,
    };

    delta.vector_deltas.insert(key, vec_delta);

    // Base has "embedding" but it's an Int
    let base = PropertyMapBuilder::new().insert("embedding", 42i64).build();

    // Apply delta
    let result = delta.apply(&base);

    // Expectation: "embedding" remains 42i64 (delta ignored)
    assert_eq!(
        result.get("embedding").and_then(|v| v.as_int()),
        Some(42),
        "Sparse delta should be silently ignored if base property is wrong type"
    );
}

#[test]
fn test_property_delta_silently_ignores_dimension_change() {
    // 🧪 Strategy: Verify that dimension changes in vectors are treated as full updates
    // even if sparse delta returns None (which it does for dimension mismatch).
    //
    // This was a bug found by Elenchus where dimension changes were silently ignored.

    // 1. Create base with 2D vector
    let old_vec = vec![1.0f32, 2.0];
    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_vec))
        .build();

    // 2. Create new with 3D vector (dimension change)
    let new_vec = vec![1.0f32, 2.0, 3.0];
    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_vec))
        .build();

    // 3. Create delta
    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // 4. Apply delta to old
    let applied = delta.apply(&old_props);

    // 5. Check if update was applied
    let applied_vec = applied.get("embedding").unwrap().as_vector().unwrap();

    assert_eq!(
        applied_vec.len(),
        3,
        "PropertyDelta should apply full update on dimension change"
    );
    assert_eq!(applied_vec, &new_vec[..]);
}

#[test]
fn test_property_delta_handles_nan_no_change() {
    // 🎯 Target: PropertyDelta::from_diff with NaN values
    // 💣 Risk: NaN != NaN could cause spurious delta entries
    // 🧪 Strategy: Create PropertyMap with NaN, create delta to same map.
    // 🔬 Verification: Delta should be empty.

    let nan_val = PropertyValue::Float(f64::NAN);
    let props = PropertyMapBuilder::new().insert("val", nan_val).build();

    let delta = PropertyDelta::from_diff(&props, &props);
    assert!(delta.is_empty(), "NaN -> NaN should result in empty delta");
}

#[test]
fn test_sentry_floats_epsilon_boundary() {
    // 🛡️ Sentry Test: Verify floats_approx_equal handles exact epsilon difference as equal.
    // This targets mutants that replace `<=` with `<` in (a-b).abs() <= VECTOR_EPSILON.

    let old = vec![0.0f32];
    let new = vec![VECTOR_EPSILON]; // Exact epsilon difference

    // Should be considered equal, so no delta
    let delta = VectorDelta::from_diff(&old, &new);
    assert!(
        delta.is_none(),
        "Difference of exactly EPSILON should be treated as equal"
    );
}

#[test]
fn test_sentry_floats_infinite_equality() {
    // 🛡️ Sentry Test: Verify floats_approx_equal handles infinity equality correctly.
    // This targets mutants that remove the explicit `is_infinite` check.
    // (INF - INF) is NaN, and NaN <= EPSILON is false, so without check they would be unequal.

    let old = vec![f32::INFINITY];
    let new = vec![f32::INFINITY];

    let delta = VectorDelta::from_diff(&old, &new);
    assert!(delta.is_none(), "Infinity == Infinity should be no change");
}

#[test]
fn test_materialize_vector_deltas_success() {
    // 🛡️ Sentry Test: Verify success path of materialize_vector_deltas.
    // This targets mutants that might empty the function body or loop, causing data loss.
    // If materialization fails silently, sparse deltas are not persisted!

    let mut delta = PropertyDelta::new();
    let key = GLOBAL_INTERNER.intern("embedding").unwrap();

    // Create a sparse delta: change index 0 to 1.0
    // Base vector size: 10
    let changes = std::sync::Arc::new(vec![(0, 1.0f32)]);
    let vec_delta = VectorDelta::Sparse {
        dimension: 10,
        changes,
    };

    delta.vector_deltas.insert(key, vec_delta);

    // Base property map with valid vector
    let base_vec = vec![0.0f32; 10];
    let base = PropertyMapBuilder::new()
        .insert_vector("embedding", &base_vec)
        .build();

    // Perform materialization
    let result = delta.materialize_vector_deltas(&base);
    assert!(result.is_ok(), "Materialization should succeed");

    // Verify side effects
    assert!(
        delta.vector_deltas.is_empty(),
        "vector_deltas should be empty after materialization"
    );
    assert!(
        delta.changed.contains_key(&key),
        "changed should contain the materialized vector"
    );

    // Verify the value in changed is correct (should be full vector)
    let materialized = delta.changed.get(&key).unwrap();
    if let PropertyValue::Vector(v) = materialized {
        assert_eq!(v.len(), 10);
        assert_eq!(v[0], 1.0f32); // Applied change
        assert_eq!(v[1], 0.0f32); // Unchanged
    } else {
        panic!("Materialized value should be a Vector");
    }
}

#[test]
fn test_vector_delta_apply_empty_changes() {
    // 🛡️ Sentry Test: Applying sparse delta with no changes should return base vector unchanged.
    let delta = VectorDelta::Sparse {
        dimension: 5,
        changes: Arc::new(vec![]),
    };
    let base = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    let result = delta.apply(&base);
    assert_eq!(result, base);
}

#[test]
fn test_vector_delta_apply_out_of_bounds() {
    // 🛡️ Sentry Test: Verify out-of-bounds indices are ignored safely.
    let delta = VectorDelta::Sparse {
        dimension: 3,
        changes: Arc::new(vec![
            (0, 10.0), // Valid
            (5, 20.0), // Invalid index
        ]),
    };
    let base = vec![1.0, 2.0, 3.0];
    let result = delta.apply(&base);

    assert_eq!(result.len(), 3);
    assert_eq!(result[0], 10.0); // Updated
    assert_eq!(result[1], 2.0); // Unchanged
    assert_eq!(result[2], 3.0); // Unchanged
}
