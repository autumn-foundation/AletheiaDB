#[cfg(test)]
mod metadata_tests {
    use crate::core::id::*;

    use crate::core::temporal::*;
    use crate::core::version::*;

    #[test]
    fn test_version_metadata_new() {
        let tx_id = TxId::new(100);
        let timestamp = Timestamp::from(5000);
        let metadata = VersionMetadata::new(tx_id, timestamp);

        assert_eq!(metadata.created_by_tx, tx_id);
        assert_eq!(metadata.commit_timestamp, Some(timestamp));
    }

    #[test]
    fn test_version_metadata_uncommitted() {
        let tx_id = TxId::new(200);
        let metadata = VersionMetadata::uncommitted(tx_id);

        assert_eq!(metadata.created_by_tx, tx_id);
        assert_eq!(metadata.commit_timestamp, None);
    }

    #[test]
    fn test_version_metadata_default() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "core::version::metadata_tests::test_version_metadata_default_subprocess_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for default metadata test");

        // CI environments can be slow, so give it plenty of time (10s)
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "subprocess helper failed for VersionMetadata default semantics"
                    );
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("VersionMetadata::default/default_for_existing did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn test_version_metadata_default_subprocess_helper() {
        let metadata = VersionMetadata::default();
        let default_expected = VersionMetadata::default_for_existing();

        assert_eq!(metadata.created_by_tx, default_expected.created_by_tx);
        assert_eq!(metadata.commit_timestamp, default_expected.commit_timestamp);
        assert_eq!(metadata.created_by_tx, TxId::new(0));
        assert!(metadata.commit_timestamp.is_some());
    }

    #[test]
    fn test_version_metadata_debug() {
        let tx_id = TxId::new(123);
        let timestamp = Timestamp::from(456);
        let metadata = VersionMetadata::new(tx_id, timestamp);
        let debug_str = format!("{:?}", metadata);

        assert!(debug_str.contains("VersionMetadata"));
        assert!(debug_str.contains("created_by_tx"));
        assert!(debug_str.contains("commit_timestamp"));
        assert!(debug_str.contains("123"));
    }

    #[test]
    fn test_version_metadata_clone_copy() {
        let tx_id = TxId::new(123);
        let timestamp = Timestamp::from(456);
        let metadata = VersionMetadata::new(tx_id, timestamp);

        let copy = metadata; // Copy
        assert_eq!(metadata, copy);

        #[allow(clippy::clone_on_copy)]
        let clone = metadata.clone(); // Clone
        assert_eq!(metadata, clone);
    }
}

#[cfg(test)]
mod storage_tests {
    use crate::core::id::*;
    use crate::core::interning::GLOBAL_INTERNER;

    use crate::core::property::PropertyMapBuilder;
    use crate::core::property::*;
    use crate::core::temporal::*;
    use crate::core::version::*;

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
        if let (PropertyValue::Array(arc1), PropertyValue::Array(arc2)) =
            (&array_val, &cloned_array)
        {
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
}

#[cfg(test)]
mod sentry_tests {

    use crate::core::interning::GLOBAL_INTERNER;

    use crate::core::property::*;
    use crate::core::property::{MAX_VECTOR_DIMENSIONS, PropertyMapBuilder};

    use crate::core::version::*;
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
}

#[cfg(test)]
mod mutant_kill_tests {
    use crate::core::id::*;
    use crate::core::interning::GLOBAL_INTERNER;

    use crate::core::property::PropertyMapBuilder;
    use crate::core::property::*;
    use crate::core::temporal::TIMESTAMP_MAX;
    use crate::core::temporal::*;
    use crate::core::version::*;
    use std::sync::Arc;

    fn make_node_anchor() -> NodeVersion {
        let props = PropertyMapBuilder::new().insert("name", "node").build();
        NodeVersion::new_anchor(
            VersionId::new(10).unwrap(),
            NodeId::new(11).unwrap(),
            BiTemporalInterval::current(1_000.into()),
            GLOBAL_INTERNER.intern("Node").unwrap(),
            props,
        )
    }

    fn make_edge_anchor() -> EdgeVersion {
        let props = PropertyMapBuilder::new().insert("weight", 1i64).build();
        EdgeVersion::new_anchor(
            VersionId::new(20).unwrap(),
            EdgeId::new(21).unwrap(),
            BiTemporalInterval::current(1_000.into()),
            GLOBAL_INTERNER.intern("EDGE").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            props,
        )
    }

    fn make_edge_delta() -> EdgeVersion {
        let old_props = PropertyMapBuilder::new().insert("weight", 1i64).build();
        let new_props = PropertyMapBuilder::new().insert("weight", 2i64).build();
        EdgeVersion::new_delta(
            VersionId::new(22).unwrap(),
            EdgeId::new(23).unwrap(),
            BiTemporalInterval::current(2_000.into()),
            GLOBAL_INTERNER.intern("EDGE").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            &old_props,
            &new_props,
            VersionId::new(20).unwrap(),
        )
    }

    #[test]
    fn test_vector_delta_from_diff_allows_exact_max_dimensions() {
        let len = MAX_VECTOR_DIMENSIONS;
        let old = vec![0.0f32; len];
        let mut new = old.clone();
        new[0] = 1.0;

        let delta = VectorDelta::from_diff(&old, &new);
        assert!(delta.is_some(), "Max dimension should be allowed");
    }

    #[test]
    fn test_vector_delta_from_diff_threshold_behavior_for_sparse_vs_full() {
        // Boundary: changes * 2 == dimension should be full storage.
        let old_full = vec![0.0f32; 4];
        let mut new_full = old_full.clone();
        new_full[0] = 1.0;
        new_full[1] = 2.0;
        let delta_full = VectorDelta::from_diff(&old_full, &new_full).unwrap();
        assert!(
            matches!(delta_full, VectorDelta::Full(_)),
            "Threshold boundary should choose full storage"
        );

        // One change in 3 dimensions should be sparse.
        let old_sparse = vec![0.0f32; 3];
        let mut new_sparse = old_sparse.clone();
        new_sparse[0] = 1.0;
        let delta_sparse = VectorDelta::from_diff(&old_sparse, &new_sparse).unwrap();
        assert!(
            matches!(delta_sparse, VectorDelta::Sparse { .. }),
            "Few changes should choose sparse storage"
        );
    }

    #[test]
    fn test_vector_delta_apply_ignores_index_equal_to_length() {
        let base = vec![0.0f32, 1.0, 2.0];
        let delta = VectorDelta::Sparse {
            dimension: 3,
            changes: Arc::new(vec![(3, 99.0)]),
        };

        let result = delta.apply(&base);
        assert_eq!(result, base);
    }

    #[test]
    fn test_vector_delta_sparse_estimated_heap_size_matches_formula() {
        let mut changes = Vec::with_capacity(4);
        changes.push((0u32, 1.0f32));
        changes.push((3u32, 2.0f32));
        let delta = VectorDelta::Sparse {
            dimension: 8,
            changes: Arc::new(changes),
        };

        let expected = match &delta {
            VectorDelta::Sparse { changes, .. } => {
                changes.capacity() * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
            }
            _ => unreachable!(),
        };
        assert_eq!(delta.estimated_heap_size(), expected);
    }

    #[test]
    fn test_vector_delta_partial_eq_semantics() {
        let sparse_a = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
        };
        let sparse_b = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
        };
        assert_eq!(sparse_a, sparse_b);

        let sparse_dim_mismatch = VectorDelta::Sparse {
            dimension: 5,
            changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
        };
        assert_ne!(sparse_a, sparse_dim_mismatch);

        let sparse_len_mismatch = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5)]),
        };
        assert_ne!(sparse_a, sparse_len_mismatch);

        let sparse_idx_mismatch = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(0, 0.5), (3, 1.0)]),
        };
        assert_ne!(sparse_a, sparse_idx_mismatch);

        let sparse_val_mismatch = VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 0.5 + 1e-3), (3, 1.0)]),
        };
        assert_ne!(sparse_a, sparse_val_mismatch);

        let full_a = VectorDelta::Full(Arc::from(vec![1.0f32, 2.0f32]));
        let full_b = VectorDelta::Full(Arc::from(vec![1.0f32, 2.0f32]));
        assert_eq!(full_a, full_b);

        let full_len_mismatch = VectorDelta::Full(Arc::from(vec![1.0f32]));
        assert_ne!(full_a, full_len_mismatch);

        let full_val_mismatch = VectorDelta::Full(Arc::from(vec![1.0f32, 2.5f32]));
        assert_ne!(full_a, full_val_mismatch);

        assert_ne!(sparse_a, full_a);
    }

    #[test]
    fn test_temporal_version_close_transaction_time_updates_tx_dimension() {
        let mut node = make_node_anchor();
        let end = Timestamp::from(2_000);

        node.close_transaction_time(end).unwrap();

        assert_eq!(node.temporal().transaction_time().end(), end);
        assert_eq!(node.temporal().valid_time().end(), TIMESTAMP_MAX);
    }

    #[test]
    fn test_property_delta_is_empty_only_when_all_collections_empty() {
        let key_a = GLOBAL_INTERNER.intern("a").unwrap();
        let key_v = GLOBAL_INTERNER.intern("v").unwrap();
        let key_r = GLOBAL_INTERNER.intern("r").unwrap();

        let mut changed_only = PropertyDelta::new();
        changed_only.changed.insert(key_a, PropertyValue::Int(1));
        assert!(!changed_only.is_empty());

        let mut vector_only = PropertyDelta::new();
        vector_only.vector_deltas.insert(
            key_v,
            VectorDelta::Sparse {
                dimension: 2,
                changes: Arc::new(vec![(0, 1.0)]),
            },
        );
        assert!(!vector_only.is_empty());

        let mut removed_only = PropertyDelta::new();
        removed_only.removed.insert(key_r);
        assert!(!removed_only.is_empty());

        assert!(PropertyDelta::new().is_empty());
    }

    #[test]
    fn test_property_delta_estimated_heap_size_matches_formula() {
        let mut delta = PropertyDelta::new();
        let key_name = GLOBAL_INTERNER.intern("name").unwrap();
        let key_vec = GLOBAL_INTERNER.intern("embedding").unwrap();
        let key_removed = GLOBAL_INTERNER.intern("old").unwrap();

        delta
            .changed
            .insert(key_name, PropertyValue::string("Alice"));
        delta.vector_deltas.insert(
            key_vec,
            VectorDelta::Sparse {
                dimension: 4,
                changes: Arc::new(vec![(1, 2.0)]),
            },
        );
        delta.removed.insert(key_removed);

        let expected_changed_overhead = delta.changed.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<PropertyValue>() + 8);
        let expected_changed_values: usize = delta
            .changed
            .values()
            .map(PropertyValue::estimated_heap_size)
            .sum();

        let expected_vector_overhead = delta.vector_deltas.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<VectorDelta>() + 8);
        let expected_vector_values: usize = delta
            .vector_deltas
            .values()
            .map(VectorDelta::estimated_heap_size)
            .sum();

        let expected_removed_overhead =
            delta.removed.capacity() * (std::mem::size_of::<PropertyKey>() + 8);

        let expected = expected_changed_overhead
            + expected_changed_values
            + expected_vector_overhead
            + expected_vector_values
            + expected_removed_overhead;

        assert_eq!(delta.estimated_heap_size(), expected);
    }

    #[test]
    fn test_node_and_edge_estimated_size_match_formula() {
        let node = make_node_anchor();
        assert_eq!(
            node.estimated_size(),
            std::mem::size_of::<NodeVersion>() + node.data.estimated_heap_size()
        );

        let edge = make_edge_anchor();
        assert_eq!(
            edge.estimated_size(),
            std::mem::size_of::<EdgeVersion>() + edge.data.estimated_heap_size()
        );
    }

    #[test]
    fn test_edge_anchor_reports_not_delta() {
        let edge = make_edge_anchor();
        assert!(!edge.is_delta());
    }

    #[test]
    fn test_entity_version_trait_round_trip_links_for_node_and_edge() {
        fn set_links<V: EntityVersion>(
            v: &mut V,
            prev: Option<VersionId>,
            next: Option<VersionId>,
        ) {
            v.set_prev_version(prev);
            v.set_next_version(next);
        }
        fn links<V: EntityVersion>(v: &V) -> (Option<VersionId>, Option<VersionId>) {
            (v.prev_version(), v.next_version())
        }

        let mut node = make_node_anchor();
        let node_prev = Some(VersionId::new(99).unwrap());
        let node_next = Some(VersionId::new(100).unwrap());
        set_links(&mut node, node_prev, node_next);
        assert_eq!(links(&node), (node_prev, node_next));
        set_links(&mut node, None, None);
        assert_eq!(links(&node), (None, None));

        let mut edge = make_edge_anchor();
        let edge_prev = Some(VersionId::new(199).unwrap());
        let edge_next = Some(VersionId::new(200).unwrap());
        set_links(&mut edge, edge_prev, edge_next);
        assert_eq!(links(&edge), (edge_prev, edge_next));
        set_links(&mut edge, None, None);
        assert_eq!(links(&edge), (None, None));
    }

    #[test]
    fn test_entity_version_trait_is_anchor_for_edge_variants() {
        fn trait_is_anchor<V: EntityVersion>(v: &V) -> bool {
            v.is_anchor()
        }

        let edge_anchor = make_edge_anchor();
        let edge_delta = make_edge_delta();

        assert!(trait_is_anchor(&edge_anchor));
        assert!(!trait_is_anchor(&edge_delta));
    }

    #[test]
    fn test_vector_delta_epsilon_boundary() {
        // 🛡️ Sentinel Test: Verify exact epsilon handling in VectorDelta.
        // This targets mutants that replace `<=` with `<` in floats_approx_equal.
        // (a - b).abs() <= VECTOR_EPSILON

        // A small offset to test boundaries around VECTOR_EPSILON.
        const SMALL_OFFSET: f32 = 1e-9;

        // Use 0.0 as base to ensure EPSILON is exactly representable and addition
        // doesn't lose precision (which happens around 1.0 due to f32::EPSILON > VECTOR_EPSILON).
        let v1 = vec![0.0f32];

        // Case 1: Difference exactly equal to EPSILON
        // 0.0 + 1e-7. abs(diff) = 1e-7.
        // Original (<=): Equal -> No Delta (None)
        // Mutant (<): Not Equal -> Delta (Some)
        let v2 = vec![VECTOR_EPSILON];
        assert!(
            VectorDelta::from_diff(&v1, &v2).is_none(),
            "Difference of exactly EPSILON should be considered equal (no delta)"
        );

        // Case 2: Difference slightly larger than EPSILON
        // Should be detected as different
        let v3 = vec![VECTOR_EPSILON + SMALL_OFFSET];
        assert!(
            VectorDelta::from_diff(&v1, &v3).is_some(),
            "Difference > EPSILON should be detected"
        );

        // Case 3: Difference slightly smaller than EPSILON
        // Should be considered equal
        let v4 = vec![VECTOR_EPSILON - SMALL_OFFSET];
        assert!(
            VectorDelta::from_diff(&v1, &v4).is_none(),
            "Difference < EPSILON should be considered equal"
        );
    }
}

// ============================================================================
// RED PHASE: Tests for embedded commit timestamps in versions (Issue #238)
// HyPer/TiDB approach: embed commit timestamp directly in version data
// so visibility checks can bypass the TxVisibilityManager::committed map.
// ============================================================================

#[cfg(test)]
mod embedded_commit_timestamp_tests {
    use crate::core::id::*;
    use crate::core::interning::GLOBAL_INTERNER;

    use crate::core::property::PropertyMapBuilder;

    use crate::core::temporal::*;
    use crate::core::version::*;

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
}
