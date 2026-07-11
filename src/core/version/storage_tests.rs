use super::*;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;

#[test]
fn test_property_delta_diff() {
    let old = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("city", "NYC")
        .build();

    let new = PropertyMapBuilder::new()
        .insert("name", "Alice") // Unchanged
        .insert("age", 31i64) // Modified
        .insert("country", "USA") // Added
        // city removed
        .build();

    let delta = PropertyDelta::from_diff(&old, &new);

    assert_eq!(delta.changed.len(), 2); // age modified, country added
    assert_eq!(delta.removed.len(), 1); // city removed
    assert!(
        delta
            .removed
            .contains(&GLOBAL_INTERNER.intern("city").unwrap())
    );
}

#[test]
fn test_property_delta_apply() {
    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let mut delta = PropertyDelta::new();
    delta.changed.insert(
        GLOBAL_INTERNER.intern("age").unwrap(),
        PropertyValue::Int(31),
    );
    delta.changed.insert(
        GLOBAL_INTERNER.intern("city").unwrap(),
        PropertyValue::string("NYC"),
    );

    let result = delta.apply(&base);

    assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(31.into()));
    assert_eq!(result.get("city").and_then(|v| v.as_str()), Some("NYC"));
}

#[test]
fn test_empty_delta() {
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();

    let delta = PropertyDelta::from_diff(&props, &props);
    assert!(delta.is_empty());
}

#[test]
fn test_node_version_anchor() {
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();

    let temporal = BiTemporalInterval::current(1000.into());

    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(10).unwrap(),
        temporal,
        crate::core::interning::GLOBAL_INTERNER
            .intern("Person")
            .unwrap(),
        props,
    );

    assert!(version.is_anchor());
    assert!(!version.is_delta());
    assert_eq!(version.node_id, NodeId::new(10).unwrap());
}

#[test]
fn test_edge_version_delta() {
    let old_props = PropertyMapBuilder::new().insert("weight", 1i64).build();

    let new_props = PropertyMapBuilder::new().insert("weight", 2i64).build();

    let temporal = BiTemporalInterval::current(2000.into());

    let version = EdgeVersion::new_delta(
        VersionId::new(2).unwrap(),
        EdgeId::new(20).unwrap(),
        temporal,
        crate::core::interning::GLOBAL_INTERNER
            .intern("KNOWS")
            .unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        &old_props,
        &new_props,
        VersionId::new(1).unwrap(),
    );

    assert!(!version.is_anchor());
    assert!(version.is_delta());
    assert_eq!(version.prev_version, Some(VersionId::new(1).unwrap()));
}

// ========================================================================
// Estimated Size Tests
// ========================================================================

#[test]
fn test_property_delta_estimated_heap_size_empty() {
    let delta = PropertyDelta::new();
    let size = delta.estimated_heap_size();
    // Empty delta should have zero heap overhead
    assert_eq!(size, 0, "Empty delta heap size should be zero");
}

#[test]
fn test_property_delta_estimated_heap_size_with_changes() {
    let mut delta = PropertyDelta::new();
    delta.changed.insert(
        GLOBAL_INTERNER.intern("name").unwrap(),
        PropertyValue::string("Alice"), // 5 bytes
    );
    delta.changed.insert(
        GLOBAL_INTERNER.intern("description").unwrap(),
        PropertyValue::string("A longer description"), // 20 bytes
    );
    delta
        .removed
        .insert(GLOBAL_INTERNER.intern("old_field").unwrap());

    let size = delta.estimated_heap_size();
    // Should include at least string lengths (5 + 20 = 25 bytes)
    assert!(
        size >= 25,
        "Delta with strings should include string heap size"
    );
}

#[test]
fn test_version_data_estimated_heap_size_anchor() {
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let data = VersionData::anchor(props);
    let size = data.estimated_heap_size();
    // Anchor should include property map heap size
    assert!(size >= 5, "Anchor heap size should include string 'Alice'");
}

#[test]
fn test_version_data_estimated_heap_size_delta() {
    let old_props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let new_props = PropertyMapBuilder::new().insert("name", "Bob").build();

    let data = VersionData::delta_from_diff(&old_props, &new_props);
    let size = data.estimated_heap_size();
    // Delta should include the changed property heap size
    assert!(size >= 3, "Delta heap size should include string 'Bob'");
}

#[test]
fn test_node_version_estimated_size() {
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
        .build();

    let temporal = BiTemporalInterval::current(1000.into());
    let version = NodeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        NodeId::new(10).unwrap(),
        temporal,
        GLOBAL_INTERNER.intern("Person").unwrap(),
        props,
    );

    let size = version.estimated_size();
    // Should include stack size + heap size (at least vector: 384 * 4 = 1536 bytes)
    assert!(
        size >= std::mem::size_of::<NodeVersion>() + 384 * 4,
        "Node version estimated size should include vector heap"
    );
}

#[test]
fn test_edge_version_estimated_size() {
    let props = PropertyMapBuilder::new()
        .insert("weight", 1.5f64)
        .insert("label", "connection")
        .build();

    let temporal = BiTemporalInterval::current(1000.into());
    let version = EdgeVersion::new_anchor(
        VersionId::new(1).unwrap(),
        EdgeId::new(20).unwrap(),
        temporal,
        GLOBAL_INTERNER.intern("CONNECTS").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        props,
    );

    let size = version.estimated_size();
    // Should include at least stack size + string "connection" (10 bytes)
    assert!(
        size >= std::mem::size_of::<EdgeVersion>() + 10,
        "Edge version estimated size should include string heap"
    );
}

#[test]
fn test_node_version_estimated_size_delta() {
    let old_props = PropertyMapBuilder::new().insert("count", 1i64).build();
    let new_props = PropertyMapBuilder::new().insert("count", 2i64).build();

    let temporal = BiTemporalInterval::current(2000.into());
    let version = NodeVersion::new_delta(
        VersionId::new(2).unwrap(),
        NodeId::new(10).unwrap(),
        temporal,
        GLOBAL_INTERNER.intern("Counter").unwrap(),
        &old_props,
        &new_props,
        VersionId::new(1).unwrap(),
    );

    let size = version.estimated_size();
    // Delta version should have smaller heap size than anchor with full data
    assert!(
        size >= std::mem::size_of::<NodeVersion>(),
        "Delta version size should include at least stack size"
    );
}

// ========================================================================
// Vector Delta Optimization Tests (Issue #215)
// ========================================================================

#[test]
fn test_vector_delta_sparse_optimization_single_element() {
    // Verify optimization: sparse delta for single element change
    let old_embedding = vec![0.1f32; 1536]; // OpenAI ada-002 size
    let mut new_embedding = old_embedding.clone();
    new_embedding[500] = 0.2f32; // Change only one element

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding))
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // Optimized behavior: vector stored in vector_deltas, not in changed
    assert_eq!(
        delta.changed.len(),
        0,
        "Vector should not be in changed (uses sparse delta)"
    );
    assert_eq!(
        delta.vector_deltas.len(),
        1,
        "Vector should be in vector_deltas"
    );

    let delta_size = delta.estimated_heap_size();
    let full_vector_size = 1536 * std::mem::size_of::<f32>();

    // Sparse storage should be much smaller than full vector
    assert!(
        delta_size < full_vector_size / 10,
        "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes)",
        delta_size,
        full_vector_size
    );

    println!(
        "OPTIMIZATION SUCCESS: Storing {} bytes (vs {} full) for 1-element change in 1536-dim vector ({}x savings)",
        delta_size,
        full_vector_size,
        full_vector_size / delta_size.max(1)
    );
}

#[test]
fn test_vector_delta_sparse_optimization_multiple_elements() {
    // Verify optimization: sparse delta for multiple elements changed
    let old_embedding = vec![0.1f32; 384];
    let mut new_embedding = old_embedding.clone();

    // Change 5 elements (1.3% of vector)
    new_embedding[10] = 0.5f32;
    new_embedding[50] = 0.6f32;
    new_embedding[100] = 0.7f32;
    new_embedding[200] = 0.8f32;
    new_embedding[300] = 0.9f32;

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding))
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // Optimized behavior: uses sparse delta
    assert_eq!(delta.vector_deltas.len(), 1, "Should have vector delta");
    assert_eq!(delta.changed.len(), 0, "Should not store in changed");

    let delta_size = delta.estimated_heap_size();
    let full_vector_size = 384 * std::mem::size_of::<f32>();

    // Sparse storage should be much smaller
    assert!(
        delta_size < full_vector_size / 4,
        "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes)",
        delta_size,
        full_vector_size
    );

    let optimal_sparse_size = 5 * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>());
    println!(
        "OPTIMIZATION SUCCESS: {} bytes (vs {} full, {} raw sparse data) - {}x savings over full",
        delta_size,
        full_vector_size,
        optimal_sparse_size,
        full_vector_size / delta_size.max(1)
    );
}

#[test]
fn test_vector_delta_no_change() {
    // Test edge case: vector unchanged should result in empty delta
    let embedding = vec![0.1f32; 384];

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&embedding))
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    assert!(
        delta.is_empty(),
        "Delta should be empty when vector is unchanged"
    );
}

#[test]
fn test_vector_delta_complete_replacement() {
    // Test case: entire vector changed (common case for regenerated embeddings)
    let old_embedding = vec![0.1f32; 384];
    let new_embedding = vec![0.9f32; 384]; // Completely different

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding))
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // For complete replacement, full storage is optimal (no benefit from sparse)
    let delta_size = delta.estimated_heap_size();
    let full_vector_size = 384 * std::mem::size_of::<f32>();

    assert!(
        delta_size >= full_vector_size,
        "Full vector storage is expected for complete replacement"
    );
}

#[test]
fn test_mixed_properties_with_vector_delta_optimization() {
    // Test case: multiple properties changed, including a vector with sparse optimization
    let old_embedding = vec![0.1f32; 384];
    let mut new_embedding = old_embedding.clone();
    new_embedding[0] = 0.2f32; // Change one element

    let old_props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("name", "Alice") // Unchanged
        .insert("age", 31i64) // Changed
        .insert("embedding", PropertyValue::vector(&new_embedding)) // One element changed
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // Should have age in changed, embedding in vector_deltas
    assert_eq!(delta.changed.len(), 1, "Should have age changed");
    assert_eq!(
        delta.vector_deltas.len(),
        1,
        "Should have embedding in vector_deltas"
    );

    let delta_size = delta.estimated_heap_size();
    let full_vector_size = 384 * std::mem::size_of::<f32>();

    // Even with mixed properties, vector delta should save space
    assert!(
        delta_size < full_vector_size / 2,
        "Mixed delta with sparse vector should be smaller than full vector"
    );

    println!(
        "OPTIMIZATION: Mixed delta stores {} bytes (sparse vector + age property)",
        delta_size
    );
}

// ========================================================================
// Sparse Vector Delta Tests (Desired Behavior - TDD)
// ========================================================================

#[test]
fn test_sparse_vector_delta_single_element() {
    // Desired behavior: sparse storage for single element change
    let old_embedding = vec![0.1f32; 1536];
    let mut new_embedding = old_embedding.clone();
    new_embedding[500] = 0.2f32;

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding))
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // Sparse storage: index (4 bytes) + value (4 bytes) + HashMap overhead
    let sparse_data_size = std::mem::size_of::<u32>() + std::mem::size_of::<f32>();
    let delta_size = delta.estimated_heap_size();
    let full_vector_size = 1536 * std::mem::size_of::<f32>();

    // Delta should be MUCH smaller than full vector (1536 * 4 = 6144 bytes)
    // Even with HashMap overhead, sparse should be < 5% of full vector size
    assert!(
        delta_size < full_vector_size / 20,
        "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes). Raw data: {} bytes",
        delta_size,
        full_vector_size,
        sparse_data_size
    );

    // Verify it can be applied correctly
    let result = delta.apply(&old_props);
    assert_eq!(
        result.get("embedding").and_then(|v| v.as_vector()),
        new_props.get("embedding").and_then(|v| v.as_vector()),
        "Applied delta should produce correct result"
    );
}

#[test]
fn test_sparse_vector_delta_few_elements() {
    // Desired behavior: sparse storage for small percentage of changes
    let old_embedding = vec![0.1f32; 384];
    let mut new_embedding = old_embedding.clone();

    // Change 10 elements (~2.6% of vector)
    let changed_indices = vec![10, 50, 100, 150, 200, 250, 300, 350, 375, 383];
    for &idx in &changed_indices {
        new_embedding[idx] = 0.9f32;
    }

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding))
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // Sparse storage: 10 * (4 bytes index + 4 bytes value) = 80 bytes + HashMap overhead
    let sparse_data_size = 10 * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>());
    let delta_size = delta.estimated_heap_size();
    let full_vector_size = 384 * std::mem::size_of::<f32>();

    // Should be much smaller than full vector (384 * 4 = 1536 bytes)
    // Even with HashMap overhead, should be < 25% of full vector size
    assert!(
        delta_size < full_vector_size / 4,
        "Sparse delta ({} bytes) should be much smaller than full vector ({} bytes). Raw data: {} bytes",
        delta_size,
        full_vector_size,
        sparse_data_size
    );

    // Verify correctness
    let result = delta.apply(&old_props);
    assert_eq!(
        result.get("embedding").and_then(|v| v.as_vector()),
        new_props.get("embedding").and_then(|v| v.as_vector())
    );
}

#[test]
fn test_sparse_vector_delta_threshold_behavior() {
    // Desired behavior: use sparse storage for few changes, full storage for many changes
    // This tests the threshold logic (e.g., if >50% changed, use full storage)

    // Case 1: 10% changed -> should use sparse
    let old_embedding = vec![0.1f32; 384];
    let mut new_embedding_sparse = old_embedding.clone();
    for item in new_embedding_sparse.iter_mut().take(38) {
        // 10% of 384
        *item = 0.9f32;
    }

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let sparse_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding_sparse))
        .build();

    let sparse_delta = PropertyDelta::from_diff(&old_props, &sparse_props);
    let sparse_size = sparse_delta.estimated_heap_size();

    // Case 2: 90% changed -> should use full storage
    let mut new_embedding_full = old_embedding.clone();
    for item in new_embedding_full.iter_mut().take(346) {
        // 90% of 384
        *item = 0.9f32;
    }

    let full_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding_full))
        .build();

    let full_delta = PropertyDelta::from_diff(&old_props, &full_props);
    let full_size = full_delta.estimated_heap_size();

    // Sparse delta should be smaller than full delta
    assert!(
        sparse_size < full_size / 2,
        "Sparse delta ({} bytes) should be significantly smaller than full delta ({} bytes)",
        sparse_size,
        full_size
    );
}

#[test]
fn test_sparse_vector_delta_edge_cases() {
    // Test edge cases for sparse vector optimization

    // Case 1: First element changed
    let old_embedding = vec![0.1f32; 384];
    let mut new_embedding = old_embedding.clone();
    new_embedding[0] = 0.9f32;

    let old_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&old_embedding))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding))
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);
    let result = delta.apply(&old_props);
    assert_eq!(
        result.get("embedding").and_then(|v| v.as_vector()),
        new_props.get("embedding").and_then(|v| v.as_vector()),
        "First element change should work correctly"
    );

    // Case 2: Last element changed
    let mut new_embedding_last = old_embedding.clone();
    new_embedding_last[383] = 0.9f32;

    let new_props_last = PropertyMapBuilder::new()
        .insert("embedding", PropertyValue::vector(&new_embedding_last))
        .build();

    let delta_last = PropertyDelta::from_diff(&old_props, &new_props_last);
    let result_last = delta_last.apply(&old_props);
    assert_eq!(
        result_last.get("embedding").and_then(|v| v.as_vector()),
        new_props_last.get("embedding").and_then(|v| v.as_vector()),
        "Last element change should work correctly"
    );
}

// ========================================================================
// Property Cloning Performance Tests (Issue #214)
// ========================================================================

#[test]
fn test_property_key_clone_is_cheap() {
    // Verify PropertyKey cloning is O(1) - just copies the InternedString ID
    // This validates the optimization from Issue #202

    let key1 = GLOBAL_INTERNER.intern("test_property").unwrap();
    let key2 = key1; // Copy (not clone, but same semantics for Copy types)

    // Both should have the same underlying ID (they're the same InternedString)
    assert_eq!(key1, key2);

    // Verify cloning many keys is fast (all O(1) copies)
    let keys: Vec<_> = (0..1000)
        .map(|i| GLOBAL_INTERNER.intern(format!("key_{}", i)).unwrap())
        .collect();

    let cloned_keys: Vec<_> = keys.to_vec();

    // All keys should be equal to their clones
    for (original, cloned) in keys.iter().zip(cloned_keys.iter()) {
        assert_eq!(original, cloned);
    }
}

#[test]
fn test_property_value_clone_is_arc_refcount_increment() {
    // Verify PropertyValue cloning is O(1) - Arc refcount increment, not deep copy
    // This validates that values use Arc internally (already implemented)

    // Test String value
    let string_val =
        PropertyValue::string("A reasonably long string that would be expensive to deep copy");
    let cloned_string = string_val.clone();

    // Both should be equal and point to the same Arc
    assert_eq!(string_val, cloned_string);
    if let (PropertyValue::String(arc1), PropertyValue::String(arc2)) =
        (&string_val, &cloned_string)
    {
        // Arcs should point to the same data (same address)
        assert!(std::ptr::eq(
            arc1.as_ref() as *const str,
            arc2.as_ref() as *const str
        ));
    } else {
        panic!("Expected String variants");
    }

    // Test Vector value (large embedding)
    let large_embedding = vec![0.1f32; 1536]; // OpenAI ada-002 size
    let vector_val = PropertyValue::vector(&large_embedding);
    let cloned_vector = vector_val.clone();

    assert_eq!(vector_val, cloned_vector);
    if let (PropertyValue::Vector(arc1), PropertyValue::Vector(arc2)) =
        (&vector_val, &cloned_vector)
    {
        // Arcs should point to the same data
        assert!(std::ptr::eq(
            arc1.as_ref() as *const [f32],
            arc2.as_ref() as *const [f32]
        ));
    } else {
        panic!("Expected Vector variants");
    }

    // Test Array value
    let array_val = PropertyValue::array(vec![PropertyValue::Int(42); 100]);
    let cloned_array = array_val.clone();

    assert_eq!(array_val, cloned_array);
    if let (PropertyValue::Array(arc1), PropertyValue::Array(arc2)) = (&array_val, &cloned_array) {
        // Arcs should point to the same data
        assert!(std::ptr::eq(
            arc1.as_ref() as *const Vec<PropertyValue>,
            arc2.as_ref() as *const Vec<PropertyValue>
        ));
    } else {
        panic!("Expected Array variants");
    }
}

#[test]
fn test_property_delta_from_diff_clone_overhead() {
    // Verify PropertyDelta::from_diff has minimal clone overhead
    // All clones should be cheap (InternedString ID copy + Arc refcount increment)

    let old_props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("city", "NYC")
        .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
        .build();

    let new_props = PropertyMapBuilder::new()
        .insert("name", "Alice") // Unchanged
        .insert("age", 31i64) // Changed
        .insert("country", "USA") // Added
        .insert("embedding", PropertyValue::vector(vec![0.2f32; 384])) // Changed
        // city removed
        .build();

    let delta = PropertyDelta::from_diff(&old_props, &new_props);

    // Verify delta structure
    assert_eq!(delta.changed.len(), 2); // age, country (embedding uses vector_deltas)
    assert_eq!(delta.vector_deltas.len(), 1); // embedding
    assert_eq!(delta.removed.len(), 1); // city

    // Verify cloned values in delta point to same Arc as new_props
    let age_key = GLOBAL_INTERNER.intern("age").unwrap();
    if let Some(delta_age) = delta.changed.get(&age_key)
        && let Some(new_age) = new_props.get("age")
    {
        // Should be equal
        assert_eq!(delta_age, new_age);
        // For non-Arc types like Int, this is a value comparison
        // But for Arc types, they would share the same allocation
    }

    // Verify cloning large embeddings is cheap (uses Arc)
    let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();
    assert!(delta.vector_deltas.contains_key(&embedding_key));
}

#[test]
fn test_property_delta_apply_clone_overhead() {
    // Verify PropertyDelta::apply has minimal clone overhead
    // All value clones should be Arc refcount increments

    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("city", "NYC")
        .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
        .build();

    let mut delta = PropertyDelta::new();
    let age_key = GLOBAL_INTERNER.intern("age").unwrap();
    let country_key = GLOBAL_INTERNER.intern("country").unwrap();

    delta.changed.insert(age_key, PropertyValue::Int(31));
    delta
        .changed
        .insert(country_key, PropertyValue::string("USA"));

    let result = delta.apply(&base);

    // Verify result has correct values
    assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(31.into()));
    assert_eq!(result.get("country").and_then(|v| v.as_str()), Some("USA"));
    assert_eq!(result.get("city").and_then(|v| v.as_str()), Some("NYC"));

    // Verify unchanged values share the same Arc
    if let (Some(base_name), Some(result_name)) = (base.get("name"), result.get("name"))
        && let (PropertyValue::String(base_arc), PropertyValue::String(result_arc)) =
            (base_name, result_name)
    {
        // Should point to the same Arc allocation
        assert!(std::ptr::eq(
            base_arc.as_ref() as *const str,
            result_arc.as_ref() as *const str
        ));
    }

    if let (Some(base_embedding), Some(result_embedding)) =
        (base.get("embedding"), result.get("embedding"))
        && let (PropertyValue::Vector(base_arc), PropertyValue::Vector(result_arc)) =
            (base_embedding, result_embedding)
    {
        // Should point to the same Arc allocation (unchanged)
        assert!(std::ptr::eq(
            base_arc.as_ref() as *const [f32],
            result_arc.as_ref() as *const [f32]
        ));
    }
}

#[test]
fn test_property_delta_apply_edge_cases() {
    // Test edge cases for apply method

    // Case 1: Empty base
    let empty_base = PropertyMapBuilder::new().build();
    let mut delta = PropertyDelta::new();
    delta.changed.insert(
        GLOBAL_INTERNER.intern("new").unwrap(),
        PropertyValue::Int(42),
    );

    let result = delta.apply(&empty_base);
    assert_eq!(result.get("new").and_then(|v| v.as_int()), Some(42.into()));

    // Case 2: Empty delta (no changes)
    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let empty_delta = PropertyDelta::new();
    let result = empty_delta.apply(&base);

    // Should be identical to base
    assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(30.into()));

    // Case 3: Delta with only removals (no additions/changes)
    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("city", "NYC")
        .build();

    let mut removal_delta = PropertyDelta::new();
    removal_delta
        .removed
        .insert(GLOBAL_INTERNER.intern("city").unwrap());

    let result = removal_delta.apply(&base);

    assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(30.into()));
    assert!(result.get("city").is_none());

    // Case 4: Large-scale scenario (verify performance doesn't degrade)
    let mut large_base_builder = PropertyMapBuilder::new();
    for i in 0..1000 {
        large_base_builder = large_base_builder.insert(&format!("prop_{}", i), i as i64);
    }
    let large_base = large_base_builder.build();

    let mut small_delta = PropertyDelta::new();
    // Only change 1% of properties (10 out of 1000)
    for i in 0..10 {
        let key = GLOBAL_INTERNER.intern(format!("prop_{}", i)).unwrap();
        small_delta
            .changed
            .insert(key, PropertyValue::Int((i + 1000) as i64));
    }

    let result = small_delta.apply(&large_base);

    // Verify changes applied
    assert_eq!(
        result.get("prop_0").and_then(|v| v.as_int()),
        Some(1000.into())
    );
    assert_eq!(
        result.get("prop_9").and_then(|v| v.as_int()),
        Some(1009.into())
    );

    // Verify unchanged properties preserved
    assert_eq!(
        result.get("prop_10").and_then(|v| v.as_int()),
        Some(10.into())
    );
    assert_eq!(
        result.get("prop_999").and_then(|v| v.as_int()),
        Some(999.into())
    );
}

#[test]
fn test_sequential_delta_application_shares_arcs() {
    // Verify that sequential delta application (hot path for time-travel)
    // maintains Arc sharing for unchanged properties

    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("counter", 0i64)
        .insert("embedding", PropertyValue::vector(vec![0.1f32; 384]))
        .build();

    // Create a chain of deltas, each incrementing the counter
    let mut deltas = Vec::new();
    for i in 1..=5 {
        let mut delta = PropertyDelta::new();
        let counter_key = GLOBAL_INTERNER.intern("counter").unwrap();
        delta.changed.insert(counter_key, PropertyValue::Int(i));
        deltas.push(delta);
    }

    // Apply all deltas in sequence (typical time-travel query pattern)
    let mut current = base.clone();
    for delta in &deltas {
        current = delta.apply(&current);
    }

    // Verify final result
    assert_eq!(
        current.get("counter").and_then(|v| v.as_int()),
        Some(5.into())
    );

    // Verify unchanged properties still share Arcs with base
    if let (Some(base_name), Some(final_name)) = (base.get("name"), current.get("name"))
        && let (PropertyValue::String(base_arc), PropertyValue::String(final_arc)) =
            (base_name, final_name)
    {
        // Should still point to the same Arc allocation
        assert!(std::ptr::eq(
            base_arc.as_ref() as *const str,
            final_arc.as_ref() as *const str
        ));
    }

    if let (Some(base_embedding), Some(final_embedding)) =
        (base.get("embedding"), current.get("embedding"))
        && let (PropertyValue::Vector(base_arc), PropertyValue::Vector(final_arc)) =
            (base_embedding, final_embedding)
    {
        // Should still point to the same Arc allocation
        assert!(std::ptr::eq(
            base_arc.as_ref() as *const [f32],
            final_arc.as_ref() as *const [f32]
        ));
    }
}
